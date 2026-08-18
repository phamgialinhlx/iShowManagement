//! The pi coding agent, as rmux needs to see it.
//!
//! pi (`@earendil-works/pi-coding-agent`, the `pi` binary) is a second coding
//! agent rmux can run and Redstone can drive, exactly as it drives Claude. This
//! module is the pi twin of [`crate::launch`], [`crate::sessions`] and
//! [`crate::transcript`] — how you start it, where it keeps its conversations,
//! and how you read one back.
//!
//! The three rules from the Claude side apply here unchanged, and for the same
//! reasons:
//!
//! 1. **Only ever read the tail.** pi's transcripts grow without bound like any
//!    agent's.
//! 2. **Never bind to pi's schema.** Fields are picked out one at a time and
//!    unknown record types skipped, so a pi release that adds a field does not
//!    turn every transcript into "empty".
//! 3. **pi's session directory encodes the cwd, and the encoding is pi's, not
//!    ours.** `~/.pi/agent/sessions/--<cwd with separators turned to dashes>--/`.
//!    We reproduce it to *find* sessions, and read each header's own `cwd` as the
//!    authority — the same belt-and-braces the Claude reader uses, because a slug
//!    scheme is exactly the kind of thing that changes between versions.

use serde::{Deserialize, Serialize};

use crate::transcript::{Entry, Speaker, Transcript};

/// One pi conversation on disk, enough to list it without opening it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// pi's session id — the `<id>.jsonl` filename, confirmed against the
    /// header's own `id` where present.
    pub id: String,
    /// Where it was running, from the header's `cwd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// First line of the first human message, for a list nobody has to open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Milliseconds since the epoch, from the file's mtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
    /// Bytes on disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// The shell line rmux runs to launch pi.
///
/// `--tui-mode regular` is pi's **inline** mode — no alternate screen — which is
/// the same choice rmux makes for Claude and for the same reasons: an alternate
/// screen takes the mouse, breaks selection, and makes scrolling round-trip. It
/// also keeps the conversation in the terminal's own scrollback, which is what a
/// remote live view and a scrollback read both depend on.
///
/// The initial prompt is a **bare positional argument** — pi collects any
/// non-flag word into its initial messages — and is shell-quoted, because it is
/// free text that reaches a login shell. `--session-id` resumes an existing
/// conversation; `--name` labels a new one.
pub fn launch_line(prompt: Option<&str>, name: Option<&str>, resume_id: Option<&str>) -> String {
    let mut line = String::from("pi --tui-mode regular");
    if let Some(id) = resume_id {
        line.push_str(" --session-id ");
        line.push_str(&rmux_transport::shell_quote(id));
    }
    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
        line.push_str(" --name ");
        line.push_str(&rmux_transport::shell_quote(name));
    }
    // The prompt goes last, as a positional. A prompt that looks like a flag
    // (starts with `-`) would be misread by pi, but `shell_quote` does not change
    // that — so a leading `--` guard is added, which pi treats as "positionals
    // follow". Only when there is a prompt, to keep the common line clean.
    if let Some(prompt) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
        line.push_str(" -- ");
        line.push_str(&rmux_transport::shell_quote(prompt));
    }
    line
}

/// pi's sessions root under a home directory: `~/.pi/agent/sessions`.
pub fn sessions_root(home: &str) -> String {
    format!("{}/.pi/agent/sessions", home.trim_end_matches('/'))
}

/// The directory pi encodes `cwd` into, under [`sessions_root`].
///
/// pi's own scheme (`getDefaultSessionDirPath`): strip a leading separator, turn
/// every `/`, `\` and `:` into `-`, and wrap in `--…--`. Reproduced so a
/// per-folder listing can go straight to the right directory; the header's `cwd`
/// remains the authority for what a session actually belongs to.
pub fn dir_for_cwd(home: &str, cwd: &str) -> String {
    let stripped = cwd.trim_start_matches(['/', '\\']);
    let encoded: String = stripped
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' { '-' } else { c })
        .collect();
    format!("{}/--{encoded}--", sessions_root(home))
}

/// A pi transcript's opening header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub id: Option<String>,
    pub cwd: Option<String>,
    /// Milliseconds since the epoch, as pi records it.
    pub created_at: Option<u64>,
}

/// Read the header out of a transcript's first line.
///
/// `None` when the first line is not a pi header — which is the honest answer for
/// a truncated tail read that began mid-file, and the caller falls back to the
/// filename for the id.
pub fn parse_header(line: &[u8]) -> Option<Header> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    if v.get("kind").and_then(|k| k.as_str()) != Some("header") {
        return None;
    }
    Some(Header {
        id: v.get("id").and_then(|s| s.as_str()).map(str::to_owned),
        cwd: v.get("cwd").and_then(|s| s.as_str()).map(str::to_owned),
        created_at: v.get("createdAt").and_then(serde_json::Value::as_u64),
    })
}

/// Parse a pi transcript, defensively.
///
/// `tailed` says the read began mid-file, so the first (partial) line is dropped
/// — the same rule as the Claude reader.
///
/// pi wraps every record as `{"kind":"entry"|"record"|...}`; only
/// `kind:"entry", type:"message"` carries conversation, and everything else —
/// operations, tool bookkeeping, usage, model changes — is skipped rather than
/// bound to.
pub fn parse(bytes: &[u8], tailed: bool) -> Transcript {
    let mut entries = Vec::new();
    let text = String::from_utf8_lossy(bytes);

    for (i, line) in text.lines().enumerate() {
        // A tailed read's first line is very likely a fragment.
        if tailed && i == 0 {
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };

        if v.get("kind").and_then(|k| k.as_str()) != Some("entry") {
            continue;
        }
        if v.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let Some(message) = v.get("message") else { continue };
        let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let body = message.get("content").map(extract_text).unwrap_or_default();
        if body.trim().is_empty() {
            continue;
        }

        let (speaker, tool) = match role {
            "user" => (Speaker::User, None),
            "assistant" => (Speaker::Assistant, None),
            // pi's own vocabulary for a command it ran.
            "bashExecution" => (Speaker::Tool, Some("bash".to_owned())),
            // Everything else — custom, summaries, notices — is not the user or
            // the model speaking, and is demoted so a reader can drop it.
            _ => (Speaker::System, None),
        };

        entries.push(Entry {
            speaker,
            text: body,
            tool,
            timestamp: v
                .get("timestamp")
                .and_then(serde_json::Value::as_u64)
                .map(|ms| ms.to_string()),
        });
    }

    Transcript { entries, ..Default::default() }
}

/// Pull readable text out of pi's `content`, which is a string or an array of
/// typed parts.
fn extract_text(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    let Some(parts) = content.as_array() else { return String::new() };
    let mut out = String::new();
    for part in parts {
        // `{"type":"text","text":"…"}` is the only part with words in it; images
        // and the rest have nothing a reader wants.
        if part.get("type").and_then(|t| t.as_str()) == Some("text")
            && let Some(text) = part.get("text").and_then(|t| t.as_str())
        {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launch_line_is_inline_and_quotes_the_prompt() {
        // `--tui-mode regular` is the no-alternate-screen mode, exactly as the
        // Claude launch opts out of fullscreen.
        let line = launch_line(Some("fix the test; rm -rf /"), None, None);
        assert!(line.starts_with("pi --tui-mode regular"), "{line}");
        // The prompt survives as one shell word, after the `--` guard.
        assert!(line.contains(" -- 'fix the test; rm -rf /'"), "{line}");
    }

    #[test]
    fn resume_and_name_are_placed_and_quoted() {
        let line = launch_line(None, Some("billing"), Some("sess-1"));
        assert!(line.contains("--session-id sess-1"), "{line}");
        assert!(line.contains("--name billing"), "{line}");
        // No prompt, no trailing positional guard.
        assert!(!line.contains(" -- "), "{line}");
    }

    #[test]
    fn a_prompt_that_looks_like_a_flag_cannot_be_read_as_one() {
        // The `--` guard is what stops pi treating a prompt starting with a dash
        // as an option. Proven by asking a shell to count the arguments.
        let line = launch_line(Some("--help me please"), None, None);
        let args = &line[line.find("pi ").unwrap()..];
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("set -- {}; echo $#", &args[3..]))
            .output()
            .expect("sh");
        // `set -- --tui-mode regular -- '--help me please'`: the first `--` ends
        // option parsing, leaving four positionals, the last being the whole
        // prompt as one word — which is the property that matters.
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "4", "{line}");
    }

    #[test]
    fn the_cwd_encoding_matches_pis_own() {
        // pi: strip leading sep, `/`\`:` → `-`, wrap in `--…--`.
        assert_eq!(
            dir_for_cwd("/home/dev", "/home/dev/api"),
            "/home/dev/.pi/agent/sessions/--home-dev-api--"
        );
        assert_eq!(
            dir_for_cwd("/h", "C:\\work\\x"),
            "/h/.pi/agent/sessions/--C--work-x--"
        );
    }

    #[test]
    fn the_header_yields_id_and_cwd_or_nothing() {
        let h = parse_header(br#"{"kind":"header","version":4,"id":"s1","createdAt":1730000000000,"cwd":"/srv/api"}"#).unwrap();
        assert_eq!(h.id.as_deref(), Some("s1"));
        assert_eq!(h.cwd.as_deref(), Some("/srv/api"));
        assert_eq!(h.created_at, Some(1_730_000_000_000));
        // A non-header first line (a tail that began mid-file) is not a header.
        assert!(parse_header(br#"{"kind":"entry","type":"message"}"#).is_none());
    }

    #[test]
    fn a_conversation_reads_back_in_order_and_skips_bookkeeping() {
        let jsonl = concat!(
            r#"{"kind":"header","version":4,"id":"s1","createdAt":1,"cwd":"/srv/api"}"#, "\n",
            r#"{"kind":"entry","type":"message","id":"e1","timestamp":10,"message":{"role":"user","content":"run the tests"}}"#, "\n",
            r#"{"kind":"record","type":"operation_started","id":"r1"}"#, "\n",
            r#"{"kind":"entry","type":"model_change","id":"e2","message":{"role":"assistant"}}"#, "\n",
            r#"{"kind":"entry","type":"message","id":"e3","timestamp":20,"message":{"role":"assistant","content":[{"type":"text","text":"Three failures."},{"type":"image","data":"…"}]}}"#, "\n",
            r#"{"kind":"entry","type":"message","id":"e4","timestamp":30,"message":{"role":"bashExecution","content":"npm test"}}"#, "\n",
        );
        let t = parse(jsonl.as_bytes(), false);
        assert_eq!(t.entries.len(), 3, "bookkeeping and non-message entries must be skipped");
        assert_eq!(t.entries[0].speaker, Speaker::User);
        assert_eq!(t.entries[0].text, "run the tests");
        assert_eq!(t.entries[1].speaker, Speaker::Assistant);
        assert_eq!(t.entries[1].text, "Three failures.", "image parts carry no text");
        assert_eq!(t.entries[2].speaker, Speaker::Tool);
        assert_eq!(t.entries[2].tool.as_deref(), Some("bash"));
    }

    #[test]
    fn a_tailed_read_drops_the_leading_partial_line() {
        let jsonl = concat!(
            r#"pe":"message","message":{"role":"assistant","content":"cut off"}}"#, "\n",
            r#"{"kind":"entry","type":"message","id":"e","timestamp":1,"message":{"role":"user","content":"kept"}}"#, "\n",
        );
        let t = parse(jsonl.as_bytes(), true);
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].text, "kept");
    }

    #[test]
    fn an_unknown_message_shape_does_not_panic_or_poison_the_rest() {
        let jsonl = concat!(
            r#"{"kind":"entry","type":"message","message":{"role":"user"}}"#, "\n",           // no content
            r#"{"kind":"entry","type":"message","message":{"content":"orphan"}}"#, "\n",       // no role → System
            r#"garbage not json"#, "\n",
            r#"{"kind":"entry","type":"message","id":"e","timestamp":1,"message":{"role":"assistant","content":"survived"}}"#, "\n",
        );
        let t = parse(jsonl.as_bytes(), false);
        assert!(t.entries.iter().any(|e| e.text == "survived"));
    }
}
