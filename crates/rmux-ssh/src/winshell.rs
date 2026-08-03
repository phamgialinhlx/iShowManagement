//! Reaching a POSIX shell on a Windows host.
//!
//! ## Why this is needed at all
//!
//! Every remote operation in rmux is a POSIX shell script — `rmux-fs` lists with
//! `for e in *`, searches with `grep -rInZ`, writes through `cat`. That is a
//! deliberate choice: it means any host reachable by `ssh` is immediately
//! usable, with nothing installed.
//!
//! Windows breaks the assumption underneath it. OpenSSH for Windows hands the
//! remote command to whatever `DefaultShell` says, and unset — the shipping
//! default — that is `cmd.exe`, which cannot parse a word of it. Measured on a
//! real machine: `sh` is not on `PATH`, `bash` is not on `PATH`, and the first
//! thing rmux does (`uname -s`) fails, so the connection never even completes.
//!
//! But a POSIX shell is almost always *present* — Git for Windows ships one, and
//! anyone using `ssh` to develop has Git. So rmux finds it and routes through it,
//! rather than rewriting every script twice.
//!
//! ## The wrapper has to survive `cmd.exe`, and leave stdin alone
//!
//! Two constraints, and the second is the one that is easy to miss.
//!
//! `cmd` re-parses the command line, so the POSIX script cannot be interpolated
//! into it — quotes, `|`, `>`, `&` and `%` are all live. The script is therefore
//! **base64-encoded**, whose alphabet (`A–Z a–z 0–9 + / =`) contains nothing
//! `cmd` reacts to, and decoded on the far side.
//!
//! The obvious decode — `echo <b64> | base64 -d | bash` — is wrong, and the bug
//! is silent: the final `bash` inherits its stdin from the *pipe*, not from the
//! SSH connection. Saving a file and uploading both stream their payload over
//! stdin, so every write would have received the script instead of the data. The
//! decode goes to a temporary file and the script is run from there, which
//! leaves stdin connected to the caller.
//!
//! Verified end to end against a real Windows 11 host: `cd && pwd` returned
//! `/c/Users/…`, and a `cat > file` in the same script received exactly the
//! bytes written to stdin.

/// Every host whose shell has been identified, by alias.
///
/// **Process-wide, not per `SshTarget`.** Several targets are constructed for
/// one host — the filesystem builds its own, so do metrics and the agent — and
/// only one of them calls `connect`. With the answer held per instance the
/// others silently assumed POSIX and every command failed with
/// `'sh' is not recognized`, which is exactly what the first live run against a
/// real Windows host did. Learning it once and sharing it also means the probe
/// costs one round trip per host per run, not one per object.
static SHELLS: std::sync::LazyLock<
    parking_lot::RwLock<std::collections::HashMap<String, RemoteShell>>,
> = std::sync::LazyLock::new(Default::default);

/// What is known about `alias`, defaulting to POSIX.
pub fn shell_for(alias: &str) -> RemoteShell {
    SHELLS.read().get(alias).cloned().unwrap_or(RemoteShell::Posix)
}

/// Record how a host has to be reached.
pub fn remember(alias: &str, shell: RemoteShell) {
    SHELLS.write().insert(alias.to_owned(), shell);
}

/// How a host's default shell must be reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteShell {
    /// The login shell parses POSIX directly — Linux, macOS, BSD.
    Posix,
    /// The default shell is `cmd.exe`; commands go through this `bash`.
    Via {
        /// A path `cmd` can execute with no quoting — see [`CANDIDATES`].
        bash: String,
    },
}

/// Where a POSIX shell lives on a Windows machine, in order of likelihood.
///
/// **Short (8.3) paths deliberately.** The real location is
/// `C:\Program Files\Git\bin\bash.exe`, and that space would have to be quoted
/// inside a command line `cmd` is already re-parsing — nested quoting that works
/// until a path changes. `PROGRA~1` sidesteps it entirely, and Windows has
/// generated these aliases for `Program Files` since it shipped.
pub const CANDIDATES: &[&str] = &[
    r"C:\PROGRA~1\Git\bin\bash.exe",
    r"C:\PROGRA~2\Git\bin\bash.exe",
    r"C:\msys64\usr\bin\bash.exe",
    r"C:\cygwin64\bin\bash.exe",
];

/// A `cmd` line that prints the first POSIX shell it finds, or nothing.
///
/// `where` is asked first so a shell already on `PATH` — an unusual install, or
/// one the operator put there on purpose — wins over a guess. Everything is
/// `2>nul` because "not found" is the ordinary answer for most of the list, and
/// four error lines would drown the one line that matters.
pub fn probe_script() -> String {
    let mut line = String::from("@where bash.exe 2>nul");
    for path in CANDIDATES {
        line.push_str(&format!(" & @if exist {path} @echo {path}"));
    }
    line
}

/// Pick the shell out of [`probe_script`]'s output.
///
/// First line wins, and a path is only accepted if it looks like one — a
/// localised `cmd` answers a failed `where` with a sentence, and treating that
/// as a program name produces a baffling error much later.
pub fn parse_probe(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| {
            line.to_ascii_lowercase().ends_with("bash.exe") && !line.contains(' ')
        })
        .map(str::to_owned)
}

/// Wrap a POSIX shell line so `cmd.exe` delivers it to `bash` intact.
///
/// See the module docs for why this is base64 and why the decode goes through a
/// file rather than a pipe. The exit status is preserved explicitly: without it
/// every command would report the status of `rm`, and a failed remote operation
/// would look like a successful one.
pub fn wrap(bash: &str, posix_line: &str) -> String {
    let encoded = base64(posix_line.as_bytes());

    // No double quotes anywhere inside: `cmd` would treat one as the end of the
    // argument. Every character here is either base64 or plain ASCII that `cmd`
    // passes through untouched while inside the quotes it *does* see.
    // **`SHELL` is corrected, and it is load-bearing.** Measured on the real
    // host: inside Git Bash, `$SHELL` is still `/c/windows/system32/cmd.exe` —
    // Windows sets it and MSYS does not override it. `CommandSpec::login_shell`
    // builds `$SHELL -l -i`, so every terminal and every Claude launch would
    // have started **cmd.exe** through a POSIX wrapper, which fails in a way
    // that looks like the shell is broken rather than misidentified. `$BASH` is
    // the path of the bash actually running, so it is right by construction.
    let inner = format!(
        "s=$(mktemp);echo {encoded}|base64 -d>$s;SHELL=$BASH bash $s;e=$?;rm -f $s;exit $e"
    );

    format!("{bash} -lc \"{inner}\"")
}

/// Standard base64, no wrapping.
///
/// Hand-rolled for the same reason `rmux-fs` hand-rolls it: forty lines of table
/// lookup is not worth a dependency, and the output has to match what the remote
/// `base64` expects rather than whatever a crate decides about padding.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;

        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_cmd_reacts_to_survives_into_the_command_line() {
        // The whole point. This script is full of characters `cmd` treats as
        // operators — interpolating it directly would execute fragments of it as
        // Windows commands, which is both broken and an injection.
        let hostile = r#"grep -rInZ -e "a|b" -- 'C:\x' > /tmp/out & echo %PATH%"#;
        let line = wrap(r"C:\PROGRA~1\Git\bin\bash.exe", hostile);

        // None of the *script's* own text reaches the command line — it is all
        // base64. The `|` and `>` that remain are the wrapper's, and they sit
        // inside the quotes, where `cmd` passes them through: verified against a
        // real Windows 11 host before this was written.
        for fragment in ["a|b", "C:\\x", "%PATH%", "/tmp/out", "grep"] {
            assert!(!line.contains(fragment), "{fragment:?} reached the command line: {line}");
        }

        // Exactly one pair of quotes, opened and closed by us. A third would end
        // the argument early and hand the rest to `cmd` as commands.
        assert_eq!(line.matches('"').count(), 2, "{line}");
        // And a `%` from a script must never survive: `cmd` expands it.
        assert!(!line.contains('%'), "{line}");
    }

    #[test]
    fn the_payload_round_trips() {
        let script = "printf 'f\\0plain.txt\\0'";
        let line = wrap("bash", script);
        let encoded = line
            .split("echo ")
            .nth(1)
            .and_then(|rest| rest.split('|').next())
            .expect("no payload");

        // Decoded with a different implementation than the one that wrote it.
        let decoded = decode_for_test(encoded);
        assert_eq!(decoded, script);
    }

    #[test]
    fn stdin_is_left_for_the_caller() {
        // The silent bug this guards: `echo … | base64 -d | bash` gives the
        // inner shell the *pipe* as stdin, so every file save and every upload
        // would have written the script instead of the payload.
        let line = wrap("bash", "cat > f");
        assert!(line.contains("bash $s"), "the script must be run from a file: {line}");
        assert!(!line.contains("base64 -d|bash"), "stdin was piped away: {line}");
    }

    #[test]
    fn the_login_shell_is_bash_and_not_cmd() {
        // Windows sets `SHELL=/c/windows/system32/cmd.exe` and MSYS leaves it
        // alone, so `$SHELL -l -i` — how every terminal and every Claude session
        // starts — would have launched cmd through a POSIX wrapper.
        let line = wrap("bash", "exec $SHELL -l -i");
        assert!(line.contains("SHELL=$BASH bash $s"), "{line}");
    }

    #[test]
    fn the_remote_exit_status_is_preserved() {
        // Without this every command reports `rm`'s status, so a failed remote
        // operation is indistinguishable from a successful one.
        let line = wrap("bash", "false");
        assert!(line.contains("e=$?"), "{line}");
        assert!(line.contains("exit $e"), "{line}");
    }

    #[test]
    fn the_probe_finds_git_bash_and_ignores_prose() {
        // A failed `where` on a localised Windows answers with a sentence, and
        // treating that as a program name fails much later and confusingly.
        let output = "INFO: Could not find files for the given pattern(s).\r\n\
                      C:\\PROGRA~1\\Git\\bin\\bash.exe\r\n";
        assert_eq!(parse_probe(output).as_deref(), Some(r"C:\PROGRA~1\Git\bin\bash.exe"));

        assert_eq!(parse_probe("INFO: Could not find files.\r\n"), None);
        assert_eq!(parse_probe(""), None);
    }

    #[test]
    fn the_probe_asks_path_before_guessing() {
        let script = probe_script();
        assert!(script.starts_with("@where bash.exe"), "{script}");
        assert!(script.contains(r"C:\PROGRA~1\Git\bin\bash.exe"), "{script}");
    }

    fn decode_for_test(s: &str) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let value = |c: u8| ALPHABET.iter().position(|a| *a == c);

        let mut bits = 0u32;
        let mut count = 0;
        let mut out = Vec::new();
        for byte in s.bytes() {
            let Some(v) = value(byte) else { continue };
            bits = (bits << 6) | v as u32;
            count += 6;
            if count >= 8 {
                count -= 8;
                out.push((bits >> count) as u8);
            }
        }
        String::from_utf8(out).unwrap()
    }
}
