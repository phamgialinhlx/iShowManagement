//! The shell snippets used to talk to a remote filesystem, and the parsers for
//! what they emit.
//!
//! Kept apart from the transport so the wire format is testable without a host:
//! these are pure functions over bytes, and the shell snippets are plain strings.
//!
//! Two properties drive the design:
//!
//! - **One round trip per operation.** Every extra `ssh` invocation is a network
//!   round trip, so a directory listing must not be a process per entry.
//! - **Filenames are arbitrary bytes.** On Unix a filename may contain spaces,
//!   tabs, newlines and invalid UTF-8 — everything except `/` and NUL. Any format
//!   that separates fields with whitespace, or records with newlines, corrupts
//!   those names. So records and fields are **NUL-separated**, the one byte a
//!   filename cannot contain.
//!
//! Paths go through [`shell_quote_path`] rather than [`shell_quote`], so a
//! leading `~` still means the remote home directory. Quoting it would make the
//! shell look for a directory literally named "~".
//!
//! [`shell_quote_path`]: zmux_transport::shell_quote_path
//! [`shell_quote`]: zmux_transport::shell_quote

use crate::{DirEntry, EntryKind};

/// Largest file we will read into the editor.
///
/// Bounded on the *remote* side so an accidental `cat` of a 4GB log never crosses
/// the network at all.
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// Shell that lists a directory as NUL-separated `kind\0name\0` pairs.
///
/// Type comes from the shell's own `[ -d ]` / `[ -L ]` builtins — no process per
/// entry — which keeps a thousand-entry directory to a single round trip. Sizes
/// are deliberately *not* collected here: on POSIX there is no portable way to
/// stat many files at once (`stat` differs between GNU and BSD, and parsing `ls`
/// breaks on the very filenames this format exists to protect), and the honest
/// options are a process per entry or no sizes. Listings do not need them; the
/// editor stats a single file when it opens it.
pub fn list_dir_script(path: &str) -> String {
    let quoted = zmux_transport::shell_quote_path(path);
    format!(
        r#"cd -- {quoted} 2>/dev/null || {{ printf 'X'; exit 0; }}
for e in * .[!.]* ..?*; do
  [ -e "$e" ] || [ -L "$e" ] || continue
  if [ -L "$e" ]; then k=l; elif [ -d "$e" ]; then k=d; else k=f; fi
  printf '%s\0%s\0' "$k" "$e"
done"#
    )
}

/// Shell that reads a file, framing the payload with its exact length.
///
/// The length prefix is what makes a single round trip unambiguous: without it
/// there is no way to tell a file that happens to end mid-transfer from one that
/// was truncated by a dropped connection.
pub fn read_file_script(path: &str, max_bytes: u64) -> String {
    let quoted = zmux_transport::shell_quote_path(path);
    format!(
        r#"p={quoted}
if [ -d "$p" ]; then printf 'D\n'; exit 0; fi
if [ ! -f "$p" ]; then printf 'M\n'; exit 0; fi
if [ ! -r "$p" ]; then printf 'P\n'; exit 0; fi
s=$(wc -c < "$p" | tr -d ' ')
if [ "$s" -gt {max_bytes} ]; then printf 'L%s\n' "$s"; exit 0; fi
printf 'S%s\n' "$s"
cat -- "$p""#
    )
}

/// Shell that writes stdin to a file.
///
/// Content is streamed through stdin rather than interpolated into the command:
/// a command line has a length limit, and embedding arbitrary bytes in one is an
/// injection waiting to happen.
///
/// The write goes to a temporary file first and is then copied **over** the
/// original rather than `mv`d onto it. `mv` would replace the inode, silently
/// discarding the file's permissions, ownership and any hard links — a
/// destructive surprise for something as ordinary as saving a file. Copying keeps
/// the original inode and narrows the window in which a dropped connection could
/// leave a half-written file to the local copy.
pub fn write_file_script(path: &str) -> String {
    let quoted = zmux_transport::shell_quote_path(path);
    format!(
        r#"p={quoted}
t=$(mktemp "${{TMPDIR:-/tmp}}/zmux.XXXXXX") || {{ printf 'E'; exit 1; }}
cat > "$t" || {{ rm -f "$t"; printf 'E'; exit 1; }}
cat "$t" > "$p" || {{ rm -f "$t"; printf 'E'; exit 1; }}
rm -f "$t"
printf 'O'"#
    )
}

/// Shell that creates a directory, including parents.
pub fn mkdir_script(path: &str) -> String {
    format!("mkdir -p -- {} && printf 'O'", zmux_transport::shell_quote_path(path))
}

/// Shell that reads a file as base64, for previewing non-text formats.
///
/// Base64 rather than raw bytes because the payload crosses a shell pipeline and
/// then JSON — both of which mangle arbitrary binary. The ~33% size cost is the
/// price of getting an image through intact.
///
/// `base64` is in coreutils on Linux and ships with macOS, but the flag to
/// disable line wrapping differs, so the wrapping is stripped here instead of
/// argued about remotely.
pub fn read_base64_script(path: &str, max_bytes: u64) -> String {
    let quoted = zmux_transport::shell_quote_path(path);
    format!(
        r#"p={quoted}
if [ ! -f "$p" ]; then printf 'M
'; exit 0; fi
if [ ! -r "$p" ]; then printf 'P
'; exit 0; fi
s=$(wc -c < "$p" | tr -d ' ')
if [ "$s" -gt {max_bytes} ]; then printf 'L%s
' "$s"; exit 0; fi
printf 'B%s
' "$s"
base64 < "$p" | tr -d '
'"#
    )
}

/// How big is it, and can it be read at all?
///
/// The first half of a download. Asking the size up front is what lets the
/// transfer be *windowed* rather than read in one gulp — see
/// [`read_chunk_base64_script`] — and it is also the only place a download can
/// cheaply tell "this is a folder" from "this is a file", which are completely
/// different answers for the operator.
pub fn file_size_script(path: &str) -> String {
    let quoted = zmux_transport::shell_quote_path(path);
    format!(
        r#"p={quoted}
if [ -d "$p" ]; then printf 'D
'; exit 0; fi
if [ ! -f "$p" ]; then printf 'M
'; exit 0; fi
if [ ! -r "$p" ]; then printf 'P
'; exit 0; fi
printf 'S%s
' "$(wc -c < "$p" | tr -d ' ')""#
    )
}

/// What [`file_size_script`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum SizeOutcome {
    Size(u64),
    Missing,
    PermissionDenied,
    IsDirectory,
}

pub fn parse_size(bytes: &[u8]) -> anyhow::Result<SizeOutcome> {
    let line = bytes.split(|b| *b == b'\n').next().unwrap_or(bytes);
    match line.first() {
        Some(b'D') => Ok(SizeOutcome::IsDirectory),
        Some(b'M') => Ok(SizeOutcome::Missing),
        Some(b'P') => Ok(SizeOutcome::PermissionDenied),
        Some(b'S') => Ok(SizeOutcome::Size(std::str::from_utf8(&line[1..])?.trim().parse()?)),
        _ => anyhow::bail!("unrecognised size response"),
    }
}

/// One window of a file, base64'd.
///
/// **Why windowed at all.** `Output::stdout` is a `String`, so every byte a
/// command returns has already been through a UTF-8 conversion — which is
/// exactly why previews are base64 and why a download cannot simply `cat`. Base64
/// then means the whole payload is resident in memory twice over, once encoded
/// and once decoded, so a single-shot read forces a size cap. A cap would fail on
/// the build artefact or the 200MB log that is precisely what someone reaches for
/// "download" to get.
///
/// Reading a window at a time keeps the peak bounded by the chunk rather than by
/// the file, at the cost of one round trip per chunk. That trade is right here:
/// downloads are not on any hot path, and "slower for very large files" beats
/// "refuses very large files".
///
/// **Redirection rather than a filename argument.** `tail -c +N < "$p"` sidesteps
/// the question of whether this `tail` understands `--` to end its options — BSD
/// and GNU disagree, and a path beginning with `-` would otherwise be read as
/// flags. The offset is 1-based, which is what `+N` means.
pub fn read_chunk_base64_script(path: &str, offset: u64, len: u64) -> String {
    let quoted = zmux_transport::shell_quote_path(path);
    let start = offset + 1;
    format!(
        r#"p={quoted}
tail -c +{start} < "$p" | head -c {len} | base64 | tr -d '
'"#
    )
}

/// Shell that deletes a file or an empty-or-not directory.
///
/// `rm -rf` is deliberate but narrow: it is only ever handed a single quoted
/// path the user picked in the tree. The quoting is what makes that safe — an
/// unquoted path containing whitespace would expand into multiple arguments and
/// delete things nobody asked about.
pub fn delete_script(path: &str) -> String {
    format!("rm -rf -- {} && printf 'O'", zmux_transport::shell_quote_path(path))
}

/// Shell that renames or moves a path.
///
/// Refuses to clobber an existing destination: silently overwriting a file
/// because a rename collided is data loss the user never asked for.
pub fn rename_script(from: &str, to: &str) -> String {
    let from = zmux_transport::shell_quote_path(from);
    let to = zmux_transport::shell_quote_path(to);
    format!(
        r#"if [ -e {to} ]; then printf 'X'; exit 0; fi
mv -- {from} {to} && printf 'O'"#
    )
}

/// Shell that creates an empty file, refusing to truncate an existing one.
pub fn create_file_script(path: &str) -> String {
    let p = zmux_transport::shell_quote_path(path);
    format!(
        r#"if [ -e {p} ]; then printf 'X'; exit 0; fi
: > {p} && printf 'O'"#
    )
}

/// Shell that writes stdin to a *new* file, refusing to overwrite an existing one.
///
/// ## The bytes go through stdin, never argv
///
/// This is the same rule the image paste follows and for the same measured
/// reason: `ARG_MAX` caps a single argument at 128 KiB, so an argv-shaped upload
/// works for an icon and fails on anything a person would actually drag in. It
/// is also why the payload is raw here rather than base64 — the wire is already
/// binary-clean with no PTY attached, so inflating by a third would buy nothing.
///
/// ## Refusing to clobber is `set -C`, not a check
///
/// A `[ -e ]` test followed by a redirect is a race, and the losing side
/// silently truncates a file the operator never named. `set -C` (POSIX
/// noclobber) makes the redirect itself `O_EXCL`, so the refusal is atomic. The
/// prior test stays only to tell "it was already there" apart from "the write
/// failed", which are different sentences for the operator to read.
///
/// Both failure paths drain stdin. Without that the far side exits while we are
/// still writing megabytes into the pipe, and the upload surfaces as a broken
/// pipe rather than the reason it actually stopped.
pub fn upload_script(path: &str) -> String {
    let p = zmux_transport::shell_quote_path(path);
    format!(
        r#"p={p}
if [ -e "$p" ]; then cat > /dev/null; printf 'X'; exit 0; fi
set -C
if cat > "$p"; then printf 'O'; else cat > /dev/null; printf 'E'; fi"#
    )
}

/// Shell that resolves the home directory — the file browser's starting point.
pub fn home_script() -> String {
    // `cd` with no argument goes home even when $HOME is unset.
    "cd && pwd".to_owned()
}

/// Parse the output of [`list_dir_script`].
pub fn parse_listing(bytes: &[u8]) -> anyhow::Result<Vec<DirEntry>> {
    // The script emits a bare "X" when the directory could not be entered.
    if bytes == b"X" {
        anyhow::bail!("cannot open directory");
    }

    let mut entries = Vec::new();
    let mut fields = bytes.split(|b| *b == 0);

    while let Some(kind) = fields.next() {
        // A trailing NUL leaves one empty field; that is the normal terminator.
        if kind.is_empty() {
            break;
        }
        let Some(name) = fields.next() else {
            // Truncated output — a connection that died mid-listing. Better to
            // report the directory as unreadable than to show a partial one as
            // if it were complete.
            anyhow::bail!("truncated directory listing");
        };

        let kind = match kind {
            b"d" => EntryKind::Directory,
            b"l" => EntryKind::Symlink,
            _ => EntryKind::File,
        };

        entries.push(DirEntry {
            // Lossy is right here: a filename may be invalid UTF-8, and refusing
            // to list a whole directory because one entry has an odd byte would
            // be worse than showing that entry with a replacement character.
            name: String::from_utf8_lossy(name).into_owned(),
            kind,
        });
    }

    sort_entries(&mut entries);
    Ok(entries)
}

/// Directories first, then case-insensitive by name — the ordering every file
/// browser uses, applied here so local and remote listings match.
pub fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| {
        let group = |k: EntryKind| if k == EntryKind::Directory { 0 } else { 1 };
        group(a.kind)
            .cmp(&group(b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// What [`read_base64_script`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Base64Outcome {
    /// Base64 text, unwrapped.
    Content { bytes: u64, base64: String },
    Missing,
    PermissionDenied,
    TooLarge(u64),
}

/// Parse the output of [`read_base64_script`].
pub fn parse_base64(bytes: &[u8]) -> anyhow::Result<Base64Outcome> {
    let newline = bytes.iter().position(|b| *b == b'\n');
    let (header, rest) = match newline {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &bytes[bytes.len()..]),
    };

    match header.first() {
        Some(b'M') => Ok(Base64Outcome::Missing),
        Some(b'P') => Ok(Base64Outcome::PermissionDenied),
        Some(b'L') => Ok(Base64Outcome::TooLarge(std::str::from_utf8(&header[1..])?.trim().parse()?)),
        Some(b'B') => {
            let size: u64 = std::str::from_utf8(&header[1..])?.trim().parse()?;
            // Whitespace is stripped rather than trusted: a shell may wrap the
            // encoding, and a data: URL with embedded newlines silently fails to
            // render in some webviews.
            let base64: String =
                String::from_utf8_lossy(rest).chars().filter(|c| !c.is_whitespace()).collect();
            Ok(Base64Outcome::Content { bytes: size, base64 })
        }
        _ => anyhow::bail!("unrecognised base64 response"),
    }
}

/// What [`read_file_script`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    Content(Vec<u8>),
    IsDirectory,
    Missing,
    PermissionDenied,
    /// Larger than the cap; carries the real size so the UI can say how big.
    TooLarge(u64),
}

/// Parse the output of [`read_file_script`].
pub fn parse_read(bytes: &[u8]) -> anyhow::Result<ReadOutcome> {
    let newline = bytes.iter().position(|b| *b == b'\n');

    // Status-only replies have no payload and may arrive without a newline.
    let (header, rest) = match newline {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &bytes[bytes.len()..]),
    };

    match header.first() {
        Some(b'D') => Ok(ReadOutcome::IsDirectory),
        Some(b'M') => Ok(ReadOutcome::Missing),
        Some(b'P') => Ok(ReadOutcome::PermissionDenied),
        Some(b'L') => {
            let size = std::str::from_utf8(&header[1..])?.trim().parse()?;
            Ok(ReadOutcome::TooLarge(size))
        }
        Some(b'S') => {
            let size: usize = std::str::from_utf8(&header[1..])?.trim().parse()?;
            // Trust the declared length over what arrived: a short read means the
            // transfer was cut off, and silently returning a truncated file would
            // let the editor save that truncation back over the original.
            anyhow::ensure!(
                rest.len() >= size,
                "truncated read: expected {size} bytes, got {}",
                rest.len()
            );
            Ok(ReadOutcome::Content(rest[..size].to_vec()))
        }
        _ => anyhow::bail!("unrecognised read response"),
    }
}

/// Whether a byte slice looks like text.
///
/// A NUL byte is the classic signal; real text files essentially never contain
/// one, and every common binary format does within the first few KB.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_refuses_to_clobber_atomically() {
        let script = upload_script("/srv/app/notes.txt");
        // `set -C` is the actual refusal — the `[ -e ]` above it only exists to
        // produce a better message. Losing the noclobber turns this back into a
        // check-then-write race that silently truncates.
        assert!(script.contains("set -C"), "{script}");
        assert!(script.contains("printf 'X'"), "{script}");
        // Both failure paths drain stdin, or the writer sees a broken pipe
        // instead of the reason the upload stopped.
        assert_eq!(script.matches("cat > /dev/null").count(), 2, "{script}");
    }

    #[test]
    fn upload_never_puts_the_payload_in_the_command_line() {
        // The bytes belong on stdin. `ARG_MAX` caps one argument at 128 KiB, so
        // an argv-shaped upload works in testing and fails on a real file.
        let script = upload_script("/srv/app/x.bin");
        assert!(script.contains(r#"cat > "$p""#), "{script}");
    }

    #[test]
    fn an_upload_path_is_quoted() {
        // It reaches a remote login shell that re-parses the line.
        let hostile = "/srv/'; rm -rf /; echo '/x";
        let script = upload_script(hostile);
        // Asserting the substring is *absent* would be wrong — the correctly
        // quoted form still contains it. What matters is that it appears only
        // inside the quoting.
        assert!(script.contains(&zmux_transport::shell_quote_path(hostile)), "{script}");
        assert!(!script.contains("p=/srv/'; rm"), "escaped its quotes: {script}");
    }

    #[test]
    fn listings_survive_filenames_with_spaces_tabs_and_newlines() {
        // The whole reason the format is NUL-separated. A newline-delimited or
        // whitespace-split format silently mangles all three of these.
        let raw = b"f\0plain.txt\0f\0two words.txt\0f\0tab\there.txt\0f\0new\nline.txt\0d\0src\0";
        let entries = parse_listing(raw).unwrap();

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"two words.txt"));
        assert!(names.contains(&"tab\there.txt"));
        assert!(names.contains(&"new\nline.txt"));
        assert_eq!(entries.len(), 5);
    }

    #[test]
    fn listings_put_directories_first_then_sort_case_insensitively() {
        let raw = b"f\0zebra.txt\0d\0Src\0f\0Apple.txt\0d\0assets\0";
        let entries = parse_listing(raw).unwrap();

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["assets", "Src", "Apple.txt", "zebra.txt"]);
    }

    #[test]
    fn a_truncated_listing_is_an_error_not_a_short_directory() {
        // A dropped connection mid-listing must not look like an empty folder.
        assert!(parse_listing(b"f\0a.txt\0d").is_err());
    }

    #[test]
    fn an_unreadable_directory_is_reported() {
        assert!(parse_listing(b"X").is_err());
    }

    #[test]
    fn an_empty_directory_lists_nothing() {
        assert_eq!(parse_listing(b"").unwrap().len(), 0);
    }

    #[test]
    fn symlinks_are_distinguished_from_regular_files() {
        // The browser shows them differently, and following one blindly can loop.
        let entries = parse_listing(b"l\0link\0f\0real\0").unwrap();
        let link = entries.iter().find(|e| e.name == "link").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
    }

    #[test]
    fn a_read_returns_exactly_the_declared_number_of_bytes() {
        let outcome = parse_read(b"S5\nhello").unwrap();
        assert_eq!(outcome, ReadOutcome::Content(b"hello".to_vec()));
    }

    #[test]
    fn content_containing_newlines_survives_framing() {
        // The frame is a byte count, not a line count — content is opaque.
        let body = b"line1\nline2\n";
        let raw = [b"S12\n".as_slice(), body].concat();
        assert_eq!(parse_read(&raw).unwrap(), ReadOutcome::Content(body.to_vec()));
    }

    #[test]
    fn a_short_read_is_rejected_rather_than_silently_truncated() {
        // This is the dangerous one: returning partial content would let the
        // editor save a truncated file back over the original.
        let err = parse_read(b"S100\nonly-a-few").unwrap_err();
        assert!(err.to_string().contains("truncated"), "got: {err}");
    }

    #[test]
    fn trailing_bytes_beyond_the_declared_length_are_ignored() {
        // A login banner or MOTD appended after the payload must not corrupt it.
        assert_eq!(parse_read(b"S5\nhelloTRAILING").unwrap(), ReadOutcome::Content(b"hello".to_vec()));
    }

    #[test]
    fn read_failures_are_distinguished_from_each_other() {
        assert_eq!(parse_read(b"D\n").unwrap(), ReadOutcome::IsDirectory);
        assert_eq!(parse_read(b"M\n").unwrap(), ReadOutcome::Missing);
        assert_eq!(parse_read(b"P\n").unwrap(), ReadOutcome::PermissionDenied);
        assert_eq!(parse_read(b"L4294967296\n").unwrap(), ReadOutcome::TooLarge(4_294_967_296));
    }

    #[test]
    fn an_empty_file_reads_as_empty_not_as_an_error() {
        assert_eq!(parse_read(b"S0\n").unwrap(), ReadOutcome::Content(Vec::new()));
    }

    #[test]
    fn base64_output_is_unwrapped() {
        // A wrapped encoding produces a data: URL with newlines in it, which some
        // webviews refuse to render — silently, as a broken image.
        let raw = b"B9\naGVsbG8=\nZXh0cmE=\n";
        match parse_base64(raw).unwrap() {
            Base64Outcome::Content { bytes, base64 } => {
                assert_eq!(bytes, 9);
                assert!(!base64.contains('\n'), "newlines survived: {base64:?}");
                assert_eq!(base64, "aGVsbG8=ZXh0cmE=");
            }
            other => panic!("expected content, got {other:?}"),
        }
    }

    #[test]
    fn base64_failures_are_distinguished() {
        assert_eq!(parse_base64(b"M\n").unwrap(), Base64Outcome::Missing);
        assert_eq!(parse_base64(b"P\n").unwrap(), Base64Outcome::PermissionDenied);
        assert_eq!(parse_base64(b"L99999\n").unwrap(), Base64Outcome::TooLarge(99999));
    }

    #[test]
    fn a_tilde_path_reaches_the_right_directory() {
        // Quoted, the remote shell looks for a folder named "~" and reports a
        // path that exists as missing — found against a real server.
        assert!(list_dir_script("~/project").contains(r#""$HOME"/project"#));
        assert!(read_file_script("~/notes.md", 1).contains(r#""$HOME"/notes.md"#));
        assert!(write_file_script("~/notes.md").contains(r#""$HOME"/notes.md"#));
    }

    #[test]
    fn destructive_operations_refuse_to_clobber() {
        // A rename that silently overwrites, or a "new file" that truncates an
        // existing one, is data loss the user never asked for.
        // Simple paths need no quoting, so match on the guard rather than on
        // quotes — asserting quoting here would be testing shell_quote, not this.
        assert!(rename_script("/a", "/b").starts_with("if [ -e /b ]; then printf 'X'"));
        assert!(create_file_script("/a").starts_with("if [ -e /a ]; then printf 'X'"));
        // ...and a path that DOES need quoting still gets it.
        assert!(rename_script("/a", "/b c").contains("[ -e '/b c' ]"));
    }

    #[test]
    fn hostile_paths_are_quoted_in_every_script() {
        // Each of these is interpolated into a shell line the remote host parses.
        let hostile = "/tmp/'; rm -rf ~; '";
        for script in [
            list_dir_script(hostile),
            read_file_script(hostile, MAX_READ_BYTES),
            write_file_script(hostile),
            mkdir_script(hostile),
            delete_script(hostile),
            create_file_script(hostile),
            rename_script(hostile, hostile),
        ] {
            assert!(
                !script.contains("; rm -rf ~; '\n") && script.contains(r"'\''"),
                "path was not quoted: {script}"
            );
        }
    }

    #[test]
    fn the_size_cap_is_enforced_remotely() {
        // Must appear in the script itself, or a huge file crosses the network
        // before we can reject it.
        assert!(read_file_script("/var/log/big", 1234).contains("-gt 1234"));
    }

    #[test]
    fn writes_preserve_the_original_file_rather_than_replacing_it() {
        // `mv` onto the target would discard its permissions, owner and hardlinks.
        let script = write_file_script("/etc/app.conf");
        assert!(script.contains("cat \"$t\" > \"$p\""), "should copy over the original");
        assert!(!script.contains("mv "), "must not replace the inode: {script}");
    }

    #[test]
    fn binary_content_is_detected_by_a_nul_byte() {
        assert!(!looks_binary(b"plain text\nwith lines\n"));
        assert!(looks_binary(b"\x7fELF\0\0\0"));
        assert!(!looks_binary(&[]));
    }

    #[test]
    fn a_size_probe_tells_the_four_outcomes_apart() {
        assert_eq!(parse_size(b"S4096\n").unwrap(), SizeOutcome::Size(4096));
        assert_eq!(parse_size(b"D\n").unwrap(), SizeOutcome::IsDirectory);
        assert_eq!(parse_size(b"M\n").unwrap(), SizeOutcome::Missing);
        assert_eq!(parse_size(b"P\n").unwrap(), SizeOutcome::PermissionDenied);
        // Never guess: an unrecognised answer is an error, not a zero-byte file.
        assert!(parse_size(b"").is_err());
        assert!(parse_size(b"what?\n").is_err());
    }

    #[test]
    fn download_windows_are_one_based_and_bounded() {
        // `tail -c +N` counts from 1, so offset 0 is `+1`. Off by one here reads
        // the file shifted by a byte and corrupts every download silently.
        let first = read_chunk_base64_script("/var/log/app.log", 0, 1024);
        assert!(first.contains("tail -c +1 "), "{first}");
        assert!(first.contains("head -c 1024"), "{first}");

        let second = read_chunk_base64_script("/var/log/app.log", 1024, 512);
        assert!(second.contains("tail -c +1025 "), "{second}");
        assert!(second.contains("head -c 512"), "{second}");
    }

    #[test]
    fn download_reads_through_a_redirect_rather_than_a_filename_argument() {
        // BSD and GNU `tail` disagree about `--`, so a path starting with `-`
        // would otherwise be read as flags.
        let script = read_chunk_base64_script("-rf", 0, 64);
        assert!(script.contains(r#"tail -c +1 < "$p""#), "{script}");
    }

    #[test]
    fn download_scripts_quote_a_hostile_path() {
        // The remote login shell re-parses this line, so an unquoted path is an
        // injection. Assert the *quoted* form is present rather than asserting
        // the dangerous substring is absent — quoting legitimately preserves it.
        let hostile = "/tmp/a b; rm -rf ~";
        let quoted = zmux_transport::shell_quote_path(hostile);

        for script in [file_size_script(hostile), read_chunk_base64_script(hostile, 0, 8)] {
            assert!(script.contains(&quoted), "path must be quoted: {script}");
            assert!(
                !script.contains("; rm -rf ~\n"),
                "the raw fragment must not reach the line: {script}"
            );
        }
    }
}
