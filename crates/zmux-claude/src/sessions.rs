//! Discovering the Claude Code sessions that already exist in a folder.
//!
//! Claude Code keeps a transcript per session at
//! `~/.claude/projects/<slug>/<session-id>.jsonl`, where `<slug>` is the working
//! directory with every `/` replaced by `-`. Listing those files is how zmux
//! offers "resume the conversation you had here yesterday" instead of always
//! starting cold.
//!
//! The transcripts are large — megabytes after a long session — so nothing reads
//! them wholesale. The remote side greps out one line per file and only the
//! matches cross the network.

use serde::{Deserialize, Serialize};

/// A Claude session that can be resumed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// The id `claude --resume` expects.
    pub id: String,
    /// Unix seconds of last activity, for ordering and "3 hours ago".
    pub modified: u64,
    /// Claude's own title for the conversation, when it has generated one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Where it was running, read from the transcript's own `cwd`.
    ///
    /// Only set by the host-wide listing, where it is the whole point: it lets
    /// a session be resumed without the operator having found the folder first.
    /// The per-folder listing leaves it `None` because the caller already knows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// Every directory name Claude Code might have filed this folder's sessions under.
///
/// There is more than one, because the scheme has changed between versions and
/// both spellings survive on a machine that has been through the change:
///
/// ```text
/// /home/dev.user/some-project
///   -home-anh-nguyen-redstone-agent    <- current: dots go too
///   -home-dev.user-some-project    <- older: only separators
/// ```
///
/// Guessing one and reporting "no previous sessions" is the worst outcome — it
/// says a folder has no history when it has years of it, and the operator's only
/// recourse is to start over. So zmux tries every spelling it knows, and falls
/// back to reading the transcripts themselves (see [`list_sessions_script`]),
/// which record their own `cwd` and cannot be wrong.
///
/// Ordered most-likely first; the caller takes the first directory that exists.
pub fn project_slug_candidates(folder: &str) -> Vec<String> {
    let trimmed = folder.trim_end_matches('/');
    if trimmed.is_empty() {
        return vec!["-".to_owned()];
    }

    // Current scheme: anything that is not alphanumeric becomes a dash. Runs are
    // *not* collapsed — `/.claude` really does become `--claude` on disk.
    let modern: String = trimmed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // The same, but keeping underscores. Which of these two is right is not
    // settled by any machine we have looked at, and the cost of trying both is
    // one `test -d`.
    let with_underscores: String = trimmed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' })
        .collect();

    // The original scheme: separators only.
    let legacy = trimmed.replace('/', "-");

    let mut out = vec![modern];
    for candidate in [with_underscores, legacy] {
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// The single most likely directory name — what a new session would be filed as.
pub fn project_slug(folder: &str) -> String {
    project_slug_candidates(folder).swap_remove(0)
}

/// Shell that resolves a folder's Claude project directory into `$d`.
///
/// Shared by every command that needs it, because the resolution is subtle
/// (see [`project_slug_candidates`]) and two copies would drift.
///
/// Leaves `$d` empty when there is no such directory; callers decide whether
/// that is an error or simply "no history".
pub fn resolve_project_dir(folder: &str) -> String {
    let candidates = project_slug_candidates(folder)
        .iter()
        .map(|s| zmux_transport::shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ");
    let folder = zmux_transport::shell_quote(folder.trim_end_matches('/'));

    format!(
        r#"root="$HOME/.claude/projects"
d=
for c in {candidates}; do
  if [ -d "$root/$c" ]; then d="$root/$c"; break; fi
done

# Nothing matched by name. The transcripts record the directory they were
# started in, so ask them instead of guessing harder — this is what makes the
# lookup survive a change to Claude's naming scheme.
if [ -z "$d" ]; then
  for c in "$root"/*/; do
    f=$(ls -1t "$c"*.jsonl 2>/dev/null | head -n 1)
    [ -n "$f" ] || continue
    # Only the head: `cwd` appears on the first real message, and these files
    # run to tens of megabytes.
    if head -c 65536 "$f" 2>/dev/null | grep -qF '"cwd":"'{folder}'"'; then
      d="$c"
      break
    fi
  done
fi"#
    )
}

/// Shell that lists the sessions recorded for `folder`.
///
/// Emits NUL-separated `id`, `mtime`, `title-line` triples. NUL rather than
/// whitespace because a title is free text and will contain spaces — and could
/// contain anything else.
pub fn list_sessions_script(folder: &str) -> String {
    let resolve = resolve_project_dir(folder);

    format!(
        r#"{resolve}

[ -n "$d" ] && [ -d "$d" ] || exit 0
for f in "$d"/*.jsonl; do
  [ -f "$f" ] || continue
  id=$(basename "$f" .jsonl)
  mt=$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null || echo 0)
  t=$(grep '"aiTitle"' "$f" 2>/dev/null | tail -n 1)
  printf '%s\0%s\0%s\0' "$id" "$mt" "$t"
done"#
    )
}

/// Every Claude session on a host, wherever it was started.
///
/// **The folder comes from the session, not the other way round.** Resuming used
/// to require finding the project directory first, which is backwards: the
/// operator wants "the conversation about the offload bug", and being made to
/// remember which of forty checkouts it happened in — and then hunt for it in a
/// file tree — is the slowest possible way to get there. Every transcript
/// records its own `cwd`, so the folder is something zmux can simply read.
///
/// That also sidesteps the project-slug problem entirely. `project_slug` has to
/// guess how Claude Code spelled a directory name, and the scheme changed
/// between versions; here nothing is guessed, because the answer is inside the
/// file.
///
/// Scanning is bounded on purpose. A transcript measured **228MB** on a real
/// host and there can be hundreds of them, so this reads the `cwd` from the
/// *first* line of each — it is recorded on every record, so the first will do —
/// and the title from the last matching line. Reading whole files here would
/// take minutes and produce the same answer.
pub fn list_all_sessions_script() -> String {
    // `$HOME` is expanded by the remote shell: this whole script is one quoted
    // argument, so it must not be shell-quoted itself. See `provision`.
    r#"root="$HOME/.claude/projects"
[ -d "$root" ] || exit 0
for d in "$root"/*; do
  [ -d "$d" ] || continue
  for f in "$d"/*.jsonl; do
    [ -f "$f" ] || continue
    id=$(basename "$f" .jsonl)
    mt=$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null || echo 0)
    # A bounded prefix, not the first line. Verified against a real host: the
    # first record is a `mode` entry carrying only type/mode/sessionId, and
    # `cwd` does not appear until about the fourth line — so "read line one"
    # silently returned nothing for every session. 256KB is far more than
    # enough to reach it and still never reads a 228MB transcript.
    cwd=$(head -c 262144 "$f" 2>/dev/null | grep -m1 -o '"cwd":"[^"]*"' | cut -d'"' -f4)
    t=$(tail -c 262144 "$f" 2>/dev/null | grep '"aiTitle"' | tail -n 1)
    printf '%s\0%s\0%s\0%s\0' "$id" "$mt" "$cwd" "$t"
  done
done"#
    .to_owned()
}

/// Parse what [`list_all_sessions_script`] emits.
///
/// Four fields per record rather than three — the folder is the addition.
pub fn parse_all_sessions(bytes: &[u8]) -> Vec<SessionInfo> {
    let mut sessions = Vec::new();
    let mut fields = bytes.split(|b| *b == 0);

    while let Some(id) = fields.next() {
        if id.is_empty() {
            break;
        }
        let (Some(modified), Some(cwd), Some(title_line)) =
            (fields.next(), fields.next(), fields.next())
        else {
            // Truncated output — a connection cut mid-listing. Keep what parsed
            // rather than discarding the lot.
            break;
        };

        let folder = String::from_utf8_lossy(cwd).trim().to_owned();
        // A session whose transcript never recorded a cwd cannot be placed, and
        // resuming it into the wrong directory would point Claude at someone
        // else's code. Skipped rather than guessed.
        if folder.is_empty() {
            continue;
        }

        sessions.push(SessionInfo {
            id: String::from_utf8_lossy(id).trim().to_owned(),
            modified: String::from_utf8_lossy(modified).trim().parse().unwrap_or(0),
            title: extract_title(&String::from_utf8_lossy(title_line)),
            folder: Some(folder),
        });
    }

    // Newest first: the session you want is nearly always the one you were last
    // in, and on a shared host this list is long.
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

/// Parse what [`list_sessions_script`] emits.
pub fn parse_sessions(bytes: &[u8]) -> Vec<SessionInfo> {
    let mut sessions = Vec::new();
    let mut fields = bytes.split(|b| *b == 0);

    while let Some(id) = fields.next() {
        if id.is_empty() {
            break;
        }
        let (Some(modified), Some(title_line)) = (fields.next(), fields.next()) else {
            // Truncated output — a connection cut mid-listing. Keep what parsed
            // rather than discarding the lot.
            break;
        };

        let id = String::from_utf8_lossy(id).trim().to_owned();
        if id.is_empty() {
            continue;
        }

        sessions.push(SessionInfo {
            id,
            modified: String::from_utf8_lossy(modified).trim().parse().unwrap_or(0),
            title: extract_title(&String::from_utf8_lossy(title_line)),
            // The caller asked about one folder, so it already knows which.
            folder: None,
        });
    }

    // Most recent first: the session you want is almost always the last one you
    // were in.
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

/// Pull `aiTitle` out of a transcript line.
///
/// Hand-extracted rather than deserialised: the line is one record from a
/// megabyte-scale file whose full schema is Claude's business, not ours, and
/// binding to it would break every time that schema gains a field.
fn extract_title(line: &str) -> Option<String> {
    let key = "\"aiTitle\":\"";
    let start = line.find(key)? + key.len();
    let rest = &line[start..];

    // Walk to the closing quote, honouring backslash escapes.
    let mut title = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => title.push(' '),
                Some('t') => title.push(' '),
                Some(escaped) => title.push(escaped),
                None => break,
            },
            c => title.push(c),
        }
    }

    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_maps_to_claudes_project_directory() {
        // Verified against a real ~/.claude/projects listing.
        assert_eq!(project_slug("/Users/macprom1/Code/zmux"), "-Users-macprom1-Code-zmux");
        assert_eq!(project_slug("/home/me/api"), "-home-me-api");
        // A trailing slash must not produce a different directory for the same
        // folder, or resuming silently finds nothing.
        assert_eq!(project_slug("/home/me/api/"), "-home-me-api");
        assert_eq!(project_slug("/"), "-");
    }

    #[test]
    fn sessions_are_parsed_newest_first() {
        let raw = b"aaa\x00100\x00\x00bbb\x00300\x00\x00ccc\x00200\x00\x00";
        let sessions = parse_sessions(raw);

        // The session you want is nearly always the most recent.
        assert_eq!(
            sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["bbb", "ccc", "aaa"]
        );
        assert_eq!(sessions[0].modified, 300);
    }

    #[test]
    fn claudes_own_title_is_used_when_present() {
        let line = r#"{"type":"ai-title","aiTitle":"Rebuild cowork client with direct SSH","sessionId":"x"}"#;
        let raw = format!("abc\x00500\x00{line}\x00");
        let sessions = parse_sessions(raw.as_bytes());

        assert_eq!(sessions[0].title.as_deref(), Some("Rebuild cowork client with direct SSH"));
    }

    #[test]
    fn a_session_with_no_title_is_still_listed() {
        // A conversation too short for Claude to have titled it is still
        // resumable, and hiding it would lose work.
        let sessions = parse_sessions(b"abc\x00500\x00\x00");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, None);
    }

    #[test]
    fn escaped_characters_in_a_title_survive() {
        let line = r#"{"aiTitle":"Fix the \"login\" bug\nand ship it"}"#;
        let raw = format!("abc\x001\x00{line}\x00");

        // Quotes must not truncate the title, and a newline must not break the
        // single-line row it is rendered into.
        assert_eq!(
            parse_sessions(raw.as_bytes())[0].title.as_deref(),
            Some("Fix the \"login\" bug and ship it")
        );
    }

    #[test]
    fn no_sessions_yields_an_empty_list_rather_than_an_error() {
        // A folder Claude has never been run in is the normal first case.
        assert!(parse_sessions(b"").is_empty());
    }

    #[test]
    fn truncated_output_keeps_what_parsed() {
        // A connection cut mid-listing should not discard the sessions that did
        // arrive.
        let sessions = parse_sessions(b"aaa\x00100\x00\x00bbb\x00200");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "aaa");
    }

    #[test]
    fn the_script_looks_in_claudes_project_directory() {
        let script = list_sessions_script("/home/me/api");
        assert!(script.contains(".claude/projects"));
        assert!(script.contains("-home-me-api"));
        // Reads one line per transcript rather than whole multi-megabyte files.
        assert!(script.contains("tail -n 1"));
    }

    #[test]
    fn a_hostile_folder_name_cannot_smuggle_a_command() {
        let script = list_sessions_script("/tmp/'; rm -rf ~; '");
        assert!(script.contains(r"'\''"), "the slug was not quoted: {script}");
    }
}
