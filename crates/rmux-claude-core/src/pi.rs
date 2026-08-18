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
    // **Two formats in the wild.** The shipped pi (v0.84.2) writes a *version 3*
    // header as `{"type":"session",…}`; a newer *version 4* writes
    // `{"kind":"header",…}`. Reading zero messages from a real transcript is
    // exactly the "never bind to the schema" failure this whole module warns
    // about — and it is what a synthetic test written against the source, rather
    // than a real file, will happily pass.
    let is_v3 = v.get("type").and_then(|t| t.as_str()) == Some("session");
    let is_v4 = v.get("kind").and_then(|k| k.as_str()) == Some("header");
    if !is_v3 && !is_v4 {
        return None;
    }
    Some(Header {
        id: v.get("id").and_then(|s| s.as_str()).map(str::to_owned),
        cwd: v.get("cwd").and_then(|s| s.as_str()).map(str::to_owned),
        // v4 carries `createdAt` in ms; v3 carries an ISO `timestamp` string,
        // which is not a u64 and is left as `None` — the listing uses the file's
        // own mtime for ordering, so this is display sugar, not load-bearing.
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

        // A message record in either format:
        //   v3: `{"type":"message","message":{…}}`            (shipped pi)
        //   v4: `{"kind":"entry","type":"message","message":{…}}`
        // Everything else — session/model_change/thinking_level_change/record —
        // is bookkeeping and skipped.
        let type_field = v.get("type").and_then(|t| t.as_str());
        let is_v3_message = type_field == Some("message")
            && v.get("kind").is_none();
        let is_v4_message = v.get("kind").and_then(|k| k.as_str()) == Some("entry")
            && type_field == Some("message");
        if !is_v3_message && !is_v4_message {
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
            // The record's own `timestamp`: an ISO-8601 string in v3, a number
            // in v4. Kept as text either way — a reader wants a label, and the
            // ordering comes from the file's mtime.
            timestamp: v.get("timestamp").and_then(|t| {
                t.as_str().map(str::to_owned).or_else(|| t.as_u64().map(|n| n.to_string()))
            }),
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
    fn the_header_is_read_in_both_real_and_future_formats() {
        // v3 — the *real* shipped format, captured verbatim from pi v0.84.2.
        let v3 = parse_header(br#"{"type":"session","version":3,"id":"rmuxlive01","timestamp":"2026-08-18T09:59:47.711Z","cwd":"/root/pi-live"}"#).unwrap();
        assert_eq!(v3.id.as_deref(), Some("rmuxlive01"));
        assert_eq!(v3.cwd.as_deref(), Some("/root/pi-live"));

        // v4 — the newer format from the source, for when pi ships it.
        let v4 = parse_header(br#"{"kind":"header","version":4,"id":"s1","createdAt":1730000000000,"cwd":"/srv/api"}"#).unwrap();
        assert_eq!(v4.id.as_deref(), Some("s1"));
        assert_eq!(v4.created_at, Some(1_730_000_000_000));

        // A message line (a tail that began mid-file) is not a header.
        assert!(parse_header(br#"{"type":"message","message":{"role":"user"}}"#).is_none());
    }

    #[test]
    fn a_real_v3_conversation_reads_back_and_skips_bookkeeping() {
        // **Captured verbatim from pi v0.84.2 on a real host.** The message
        // records are top-level `type:"message"` with no `kind` wrapper, and the
        // model/thinking-level records are the bookkeeping this must skip. This
        // is the format a synthetic test written against the source got wrong.
        let jsonl = concat!(
            r#"{"type":"session","version":3,"id":"rmuxlive01","timestamp":"2026-08-18T09:59:47.711Z","cwd":"/root/pi-live"}"#, "\n",
            r#"{"type":"model_change","id":"59b7","parentId":null,"timestamp":"2026-08-18T09:59:47.758Z","provider":"openai-codex","modelId":"gpt-5.6-terra"}"#, "\n",
            r#"{"type":"thinking_level_change","id":"8ccb","parentId":"59b7","timestamp":"2026-08-18T09:59:47.759Z","thinkingLevel":"medium"}"#, "\n",
            r#"{"type":"message","id":"d593","parentId":"8ccb","timestamp":"2026-08-18T09:59:47.768Z","message":{"role":"user","content":[{"type":"text","text":"In one short sentence, what is 17 times 23?"}],"timestamp":1787047187767}}"#, "\n",
            r#"{"type":"message","id":"3bc3","parentId":"d593","timestamp":"2026-08-18T09:59:53.689Z","message":{"role":"assistant","content":[{"type":"text","text":"17 times 23 is 391."}]},"usage":{"totalTokens":1068}}"#, "\n",
        );
        let t = parse(jsonl.as_bytes(), false);
        assert_eq!(t.entries.len(), 2, "model_change/thinking_level_change must be skipped");
        assert_eq!(t.entries[0].speaker, Speaker::User);
        assert_eq!(t.entries[0].text, "In one short sentence, what is 17 times 23?");
        assert_eq!(t.entries[0].timestamp.as_deref(), Some("2026-08-18T09:59:47.768Z"));
        assert_eq!(t.entries[1].speaker, Speaker::Assistant);
        assert_eq!(t.entries[1].text, "17 times 23 is 391.");
    }

    #[test]
    fn a_v4_conversation_also_reads_when_pi_ships_it() {
        // The newer wrapped format, so the parser is ready for it.
        let jsonl = concat!(
            r#"{"kind":"header","version":4,"id":"s1","createdAt":1,"cwd":"/srv/api"}"#, "\n",
            r#"{"kind":"entry","type":"message","id":"e1","timestamp":10,"message":{"role":"user","content":"run the tests"}}"#, "\n",
            r#"{"kind":"record","type":"operation_started","id":"r1"}"#, "\n",
            r#"{"kind":"entry","type":"message","id":"e2","timestamp":20,"message":{"role":"bashExecution","content":"npm test"}}"#, "\n",
        );
        let t = parse(jsonl.as_bytes(), false);
        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.entries[0].text, "run the tests");
        assert_eq!(t.entries[1].speaker, Speaker::Tool);
    }

    #[test]
    fn a_tailed_read_drops_the_leading_partial_line() {
        let jsonl = concat!(
            r#"ssage","message":{"role":"assistant","content":"cut off"}}"#, "\n",
            r#"{"type":"message","id":"e","timestamp":1,"message":{"role":"user","content":"kept"}}"#, "\n",
        );
        let t = parse(jsonl.as_bytes(), true);
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].text, "kept");
    }

    #[test]
    fn an_unknown_message_shape_does_not_panic_or_poison_the_rest() {
        let jsonl = concat!(
            r#"{"type":"message","message":{"role":"user"}}"#, "\n",           // no content
            r#"{"type":"message","message":{"content":"orphan"}}"#, "\n",       // no role → System
            r#"garbage not json"#, "\n",
            r#"{"type":"message","id":"e","timestamp":1,"message":{"role":"assistant","content":"survived"}}"#, "\n",
        );
        let t = parse(jsonl.as_bytes(), false);
        assert!(t.entries.iter().any(|e| e.text == "survived"));
    }
}
