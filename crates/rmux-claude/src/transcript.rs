//! Reading a Claude conversation back as text.
//!
//! The terminal view shows Claude's TUI, which is a *rendering* — reflowed to the
//! pane, redrawn constantly, and impossible to scroll back through or copy out of
//! reliably. The transcript is the conversation itself, and it is what you want
//! when you need to re-read a decision from an hour ago or paste an answer
//! somewhere.
//!
//! Claude Code writes one JSON object per line to
//! `~/.claude/projects/<slug>/<session-id>.jsonl`. These files reach tens of
//! megabytes, so only the tail is fetched: a conversation is read from the end,
//! and pulling 40MB across an SSH link to show the last twenty messages would be
//! absurd.
//!
//! Nothing here binds to Claude's full schema. It is Claude's file, it gains
//! fields between releases, and a strict deserialise would turn every such change
//! into "the transcript is empty". Fields are picked out individually and
//! anything unrecognised is skipped.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How much of the tail to read by default.
///
/// Enough for a long conversation's recent history, small enough to feel instant
/// on a slow link.
pub const DEFAULT_TAIL_BYTES: u64 = 512 * 1024;

/// Who produced an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Speaker {
    User,
    Assistant,
    /// A tool Claude ran, and what it returned.
    Tool,
    /// Claude's own notes — mode changes, titles, system notices.
    System,
}

/// One turn, flattened for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub speaker: Speaker,
    /// Markdown for user and assistant text; plain text for tool output.
    pub text: String,
    /// The tool's name, when this is a tool entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// ISO-8601, straight from the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Token counts, summed across the transcript that was read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Assistant turns counted, so a per-turn average is possible.
    pub turns: u64,
}

/// The session state Claude's own status line would show.
///
/// Inline rendering does not draw a status line, so rmux has to. Every field is
/// read out of the transcript rather than guessed, and every one is optional —
/// a young session has not recorded them yet, and showing a default would be
/// asserting something nobody measured.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// `normal`, `plan`, ... — the last `mode` record seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// `default`, `acceptEdits`, `bypassPermissions`, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// The model that served the most recent turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tokens in the newest request's prompt — everything the model was sent,
    /// cached or not. This is *context in use*, not a running total: it goes down
    /// after a compaction, which a cumulative figure never would.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
}

/// A transcript, as far as it was read.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    /// The conversation this came from.
    pub session_id: String,
    pub entries: Vec<Entry>,
    pub usage: Usage,
    /// Per-assistant-turn output tokens, oldest first — the usage widget's chart.
    pub per_turn: Vec<u64>,
    /// Total size on disk, so the UI can say whether it is showing all of it.
    pub total_bytes: u64,
    /// How many bytes were actually read.
    pub read_bytes: u64,
    /// Mode, model and context — what the status line would have said.
    pub status: Status,
}

/// Shell that emits `session-id`, total size, then the tail of the transcript.
///
/// With no `session`, the most recently modified conversation is chosen — "the
/// latest", which is what you want when you have just been talking to it.
pub fn transcript_script(folder: &str, session: Option<&str>, tail_bytes: u64) -> String {
    let resolve = crate::sessions::resolve_project_dir(folder);

    // A named session is addressed directly; otherwise take the newest file.
    let pick = match session {
        Some(id) => format!(
            "f=\"$d\"/{}.jsonl\n[ -f \"$f\" ] || f=$(ls -1t \"$d\"/*.jsonl 2>/dev/null | head -n 1)",
            rmux_transport::shell_quote(id)
        ),
        None => "f=$(ls -1t \"$d\"/*.jsonl 2>/dev/null | head -n 1)".to_owned(),
    };

    format!(
        r#"{resolve}

[ -n "$d" ] && [ -d "$d" ] || exit 0
{pick}
[ -n "$f" ] && [ -f "$f" ] || exit 0

id=$(basename "$f" .jsonl)
size=$(stat -c %s "$f" 2>/dev/null || stat -f %z "$f" 2>/dev/null || echo 0)
printf '%s\0%s\0' "$id" "$size"
tail -c {tail_bytes} "$f""#
    )
}

/// Split what [`transcript_script`] emits into its header and body.
fn split_output(bytes: &[u8]) -> Option<(String, u64, &[u8])> {
    let mut parts = bytes.splitn(3, |b| *b == 0);
    let id = parts.next()?;
    let size = parts.next()?;
    let body = parts.next()?;

    Some((
        String::from_utf8_lossy(id).trim().to_owned(),
        String::from_utf8_lossy(size).trim().parse().unwrap_or(0),
        body,
    ))
}

/// Parse the script's output into a transcript.
pub fn parse(bytes: &[u8], tailed: bool) -> Transcript {
    let Some((session_id, total_bytes, body)) = split_output(bytes) else {
        return Transcript::default();
    };

    // `tail -c` cuts at a byte, not a line, so the first line is very likely half
    // a JSON object. Dropping it costs one message; keeping it would mean the
    // parser's first act is to fail on garbage.
    let body = if tailed && total_bytes > body.len() as u64 {
        match body.iter().position(|b| *b == b'\n') {
            Some(at) => &body[at + 1..],
            None => &[],
        }
    } else {
        body
    };

    let mut out = Transcript {
        session_id,
        total_bytes,
        read_bytes: body.len() as u64,
        ..Default::default()
    };

    for line in body.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        // A line that does not parse is skipped rather than fatal: the file is
        // appended to live, so the last line is routinely half-written.
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        absorb(&value, &mut out);
    }

    out
}

/// Fold one record into the transcript.
fn absorb(value: &Value, out: &mut Transcript) {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
    let timestamp = value.get("timestamp").and_then(Value::as_str).map(str::to_owned);

    match kind {
        "user" | "assistant" => {
            let speaker = if kind == "user" { Speaker::User } else { Speaker::Assistant };
            let message = value.get("message");

            // Claude injects **synthetic** assistant messages — notices and
            // interruptions it generates itself. They carry `model:
            // "<synthetic>"` and an all-zero usage block, so letting one update
            // the status reports a model nobody is using and a context of zero.
            // Seen on a real 228MB transcript, where the newest record was one.
            let synthetic = message
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .is_some_and(|m| m.starts_with('<'));

            if let Some(usage) = message.and_then(|m| m.get("usage")) {
                let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
                let input = get("input_tokens");
                let cache_read = get("cache_read_input_tokens");
                let cache_write = get("cache_creation_input_tokens");

                out.usage.input += input;
                let output = get("output_tokens");
                out.usage.output += output;
                out.usage.cache_read += cache_read;
                out.usage.cache_write += cache_write;
                out.usage.turns += 1;
                out.per_turn.push(output);

                // Overwritten each turn, so the last one wins: this is the size
                // of the newest prompt, which is what "context used" means. Only
                // from a real turn, and only when it actually carries a prompt.
                let prompt = input + cache_read + cache_write;
                if !synthetic && prompt > 0 {
                    out.status.context_tokens = Some(prompt);
                }
            }

            if !synthetic
                && let Some(model) = message.and_then(|m| m.get("model")).and_then(Value::as_str)
            {
                out.status.model = Some(model.to_owned());
            }

            let Some(content) = message.and_then(|m| m.get("content")) else {
                return;
            };

            for entry in flatten_content(content, speaker, timestamp.as_deref()) {
                out.entries.push(entry);
            }
        }

        // Claude's own title for the conversation. Worth showing — it is the one
        // line that says what the whole thing was about.
        "ai-title" => {
            if let Some(title) = value.get("aiTitle").and_then(Value::as_str) {
                out.entries.push(Entry {
                    speaker: Speaker::System,
                    text: format!("Titled: {title}"),
                    tool: None,
                    timestamp,
                });
            }
        }

        // Mode changes carry no conversation, but they are exactly what the
        // status line reports — so they are recorded rather than discarded.
        "mode" => {
            if let Some(mode) = value.get("mode").and_then(Value::as_str) {
                out.status.mode = Some(mode.to_owned());
            }
        }
        "permission-mode" => {
            if let Some(mode) = value.get("permissionMode").and_then(Value::as_str) {
                out.status.permission_mode = Some(mode.to_owned());
            }
        }

        // Genuine bookkeeping — snapshots and the like. No conversation, and
        // nothing the operator needs to see.
        _ => {}
    }
}

/// Claude Code's own plumbing, wrapped in pseudo-XML inside "user" messages.
///
/// Slash-command echoes, local command output, caveat banners and hook results
/// are all recorded as though the user typed them. They did not, and in a reading
/// view they drown the actual conversation — the tail of a long session is mostly
/// these. Demoted to `System` so the UI can fold them away rather than dropped,
/// because occasionally you do want to see that `/compact` ran.
fn is_plumbing(text: &str) -> bool {
    const MARKERS: [&str; 7] = [
        "<local-command-",
        "<command-name>",
        "<command-message>",
        "<command-args>",
        "<user-prompt-submit-hook>",
        "<system-reminder>",
        "Caveat: The messages below were generated by the user while running local commands",
    ];
    let head = text.trim_start();
    MARKERS.iter().any(|m| head.starts_with(m))
}

/// Turn a message's `content` into displayable entries.
///
/// Content is either a bare string or a list of typed blocks, and the blocks are
/// where tool calls and their results live.
fn flatten_content(content: &Value, speaker: Speaker, timestamp: Option<&str>) -> Vec<Entry> {
    let stamp = || timestamp.map(str::to_owned);

    if let Some(text) = content.as_str() {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let speaker = if speaker == Speaker::User && is_plumbing(text) {
            Speaker::System
        } else {
            speaker
        };
        return vec![Entry { speaker, text: text.to_owned(), tool: None, timestamp: stamp() }];
    }

    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for block in blocks {
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or_default();
        match block_type {
            "text" => {
                let text = block.get("text").and_then(Value::as_str).unwrap_or_default().trim();
                if !text.is_empty() {
                    let speaker = if speaker == Speaker::User && is_plumbing(text) {
                        Speaker::System
                    } else {
                        speaker
                    };
                    out.push(Entry {
                        speaker,
                        text: text.to_owned(),
                        tool: None,
                        timestamp: stamp(),
                    });
                }
            }

            "thinking" => {
                let text =
                    block.get("thinking").and_then(Value::as_str).unwrap_or_default().trim();
                if !text.is_empty() {
                    out.push(Entry {
                        speaker: Speaker::System,
                        text: text.to_owned(),
                        tool: Some("thinking".to_owned()),
                        timestamp: stamp(),
                    });
                }
            }

            "tool_use" => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                out.push(Entry {
                    speaker: Speaker::Tool,
                    text: summarise_tool_input(block.get("input")),
                    tool: Some(name.to_owned()),
                    timestamp: stamp(),
                });
            }

            "tool_result" => {
                let text = tool_result_text(block.get("content"));
                if !text.trim().is_empty() {
                    out.push(Entry {
                        speaker: Speaker::Tool,
                        text,
                        tool: Some("result".to_owned()),
                        timestamp: stamp(),
                    });
                }
            }

            _ => {}
        }
    }
    out
}

/// The interesting part of a tool call's input.
///
/// Tools differ wildly, so the common keys are preferred and the whole object is
/// the fallback. A command or a file path is what identifies the call at a
/// glance; the full JSON is noise in a reading view.
fn summarise_tool_input(input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };

    for key in ["command", "file_path", "path", "pattern", "query", "prompt", "description"] {
        if let Some(text) = input.get(key).and_then(Value::as_str) {
            return text.to_owned();
        }
    }

    serde_json::to_string(input).unwrap_or_default()
}

/// Tool results are a string, or blocks of them.
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(id: &str, size: u64, body: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(id.as_bytes());
        out.push(0);
        out.extend_from_slice(size.to_string().as_bytes());
        out.push(0);
        out.extend_from_slice(body.as_bytes());
        out
    }

    #[test]
    fn a_conversation_reads_back_in_order() {
        let body = concat!(
            r#"{"type":"user","timestamp":"t1","message":{"role":"user","content":"do the thing"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"t2","message":{"role":"assistant","content":[{"type":"text","text":"on it"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            "\n",
        );
        let t = parse(&output("abc", 200, body), false);

        assert_eq!(t.session_id, "abc");
        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.entries[0].speaker, Speaker::User);
        assert_eq!(t.entries[0].text, "do the thing");
        assert_eq!(t.entries[1].speaker, Speaker::Assistant);
        assert_eq!(t.usage.input, 10);
        assert_eq!(t.usage.output, 5);
        assert_eq!(t.per_turn, vec![5]);
    }

    #[test]
    fn a_half_written_last_line_does_not_lose_the_rest() {
        // The file is appended to while it is being read, so the tail routinely
        // ends mid-object. Failing the whole parse there would make the
        // transcript flicker empty exactly while Claude is working.
        let body = concat!(
            r#"{"type":"user","message":{"role":"user","content":"hello"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assis"#,
        );
        let t = parse(&output("abc", 100, body), false);

        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].text, "hello");
    }

    #[test]
    fn a_tailed_read_drops_the_leading_partial_line() {
        // `tail -c` cuts at a byte. The first line is half an object, and keeping
        // it would put JSON fragments at the top of the view.
        let body = concat!(
            r#"e":"user","message":{"role":"user","content":"truncated"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"intact"}}"#,
            "\n",
        );
        // total_bytes far exceeds what was read, so this was a tail.
        let t = parse(&output("abc", 1_000_000, body), true);

        assert_eq!(t.entries.len(), 1, "{:?}", t.entries);
        assert_eq!(t.entries[0].text, "intact");
    }

    #[test]
    fn a_complete_read_keeps_its_first_line() {
        // When the whole file fits, the first line is real and must survive.
        let body = concat!(
            r#"{"type":"user","message":{"role":"user","content":"first"}}"#,
            "\n",
        );
        let t = parse(&output("abc", body.len() as u64, body), true);

        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].text, "first");
    }

    #[test]
    fn tool_calls_and_results_are_kept_but_labelled() {
        let body = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"total 0"}]}}"#,
            "\n",
        );
        let t = parse(&output("abc", 500, body), false);

        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.entries[0].speaker, Speaker::Tool);
        assert_eq!(t.entries[0].tool.as_deref(), Some("Bash"));
        assert_eq!(t.entries[0].text, "ls -la");
        assert_eq!(t.entries[1].text, "total 0");
    }

    #[test]
    fn slash_command_plumbing_is_not_shown_as_the_user_speaking() {
        // The tail of a real session is mostly these. Left as `User` they bury
        // the conversation — verified against a 228MB transcript on a real host.
        let body = concat!(
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/compact</command-name>"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout>done</local-command-stdout>"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"a real question"}}"#,
            "\n",
        );
        let t = parse(&output("abc", 500, body), false);

        assert_eq!(t.entries.len(), 3, "plumbing should be kept, just demoted");
        assert_eq!(t.entries[0].speaker, Speaker::System);
        assert_eq!(t.entries[1].speaker, Speaker::System);
        assert_eq!(t.entries[2].speaker, Speaker::User, "a real message was demoted");
    }

    #[test]
    fn an_assistant_message_is_never_treated_as_plumbing() {
        // The markers only mean anything on the user side; Claude quoting one
        // back must stay an assistant message.
        let body = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"<command-name>x</command-name>"}]}}"#,
            "\n",
        );
        let t = parse(&output("abc", 100, body), false);
        assert_eq!(t.entries[0].speaker, Speaker::Assistant);
    }

    #[test]
    fn bookkeeping_records_are_left_out() {
        // These outnumber real messages in a long transcript and carry nothing
        // worth reading.
        let body = concat!(
            r#"{"type":"file-history-snapshot","snapshot":{}}"#,
            "\n",
            r#"{"type":"mode","mode":"normal"}"#,
            "\n",
            r#"{"type":"permission-mode","permissionMode":"default"}"#,
            "\n",
        );
        let t = parse(&output("abc", 100, body), false);
        assert!(t.entries.is_empty(), "{:?}", t.entries);
    }

    #[test]
    fn usage_sums_across_turns_for_the_widget() {
        let body = concat!(
            r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":5,"output_tokens":11}}}"#,
            "\n",
        );
        let t = parse(&output("abc", 100, body), false);

        assert_eq!(t.usage.input, 8);
        assert_eq!(t.usage.output, 18);
        assert_eq!(t.usage.cache_read, 100);
        assert_eq!(t.usage.cache_write, 20);
        assert_eq!(t.usage.turns, 2);
        assert_eq!(t.per_turn, vec![7, 11]);
    }

    #[test]
    fn an_unknown_block_type_does_not_discard_its_siblings() {
        // Claude's schema gains block types between releases. A new one must cost
        // that block, not the message it arrived in.
        let body = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"something_new","x":1},{"type":"text","text":"still here"}]}}"#,
            "\n",
        );
        let t = parse(&output("abc", 100, body), false);

        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].text, "still here");
    }

    #[test]
    fn the_status_line_is_reconstructed_from_the_transcript() {
        // Inline rendering draws no status line, so these records are the only
        // source for mode, permissions, model and context.
        let body = concat!(
            r#"{"type":"mode","mode":"normal"}"#,
            "\n",
            r#"{"type":"permission-mode","permissionMode":"default"}"#,
            "\n",
            r#"{"type":"mode","mode":"plan"}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-5","content":[],"usage":{"input_tokens":12,"output_tokens":5,"cache_read_input_tokens":40000,"cache_creation_input_tokens":800}}}"#,
            "\n",
        );
        let t = parse(&output("abc", 900, body), false);

        // The *latest* mode wins, not the first.
        assert_eq!(t.status.mode.as_deref(), Some("plan"));
        assert_eq!(t.status.permission_mode.as_deref(), Some("default"));
        assert_eq!(t.status.model.as_deref(), Some("claude-opus-5"));
        // Context in use = everything in the prompt, cached or not.
        assert_eq!(t.status.context_tokens, Some(12 + 40_000 + 800));
    }

    #[test]
    fn context_is_the_newest_prompt_not_a_running_total() {
        // The distinction that matters: after a compaction the prompt shrinks.
        // A cumulative figure would keep climbing and imply the session was about
        // to run out when it had just been given room.
        let body = concat!(
            r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":150000}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":2000}}}"#,
            "\n",
        );
        let t = parse(&output("abc", 900, body), false);

        assert_eq!(t.status.context_tokens, Some(2010), "context should drop after a compaction");
        // The cumulative counters still accumulate — both are wanted, for
        // different questions.
        assert_eq!(t.usage.cache_read, 152_000);
    }

    #[test]
    fn a_synthetic_message_does_not_overwrite_the_status() {
        // Found on a real transcript: the newest record was synthetic, so the
        // status reported model "<synthetic>" and a context of 0 tokens.
        let body = concat!(
            r#"{"type":"assistant","message":{"model":"claude-opus-5","content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":50000}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"<synthetic>","content":[],"usage":{"input_tokens":0,"output_tokens":0}}}"#,
            "\n",
        );
        let t = parse(&output("abc", 900, body), false);

        assert_eq!(t.status.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(t.status.context_tokens, Some(50_010));
    }

    #[test]
    fn a_session_with_no_records_yet_reports_nothing_rather_than_defaults() {
        // Showing "normal / default" for a session that never said so would be
        // inventing a reading.
        let t = parse(&output("abc", 0, ""), false);
        assert_eq!(t.status, Status::default());
        assert!(t.status.mode.is_none());
        assert!(t.status.context_tokens.is_none());
    }

    #[test]
    fn empty_output_is_an_empty_transcript_not_a_panic() {
        assert!(parse(b"", false).entries.is_empty());
        assert!(parse(b"no-nul-anywhere", false).entries.is_empty());
    }

    #[test]
    fn the_script_addresses_a_named_session_directly() {
        let script = transcript_script("/srv/app", Some("abc-123"), 4096);
        assert!(script.contains("abc-123.jsonl"), "{script}");
        // And still falls back, because a session can be deleted between the
        // listing and the read.
        assert!(script.contains("ls -1t"), "{script}");
        assert!(script.contains("tail -c 4096"), "{script}");
    }

    #[test]
    fn the_script_takes_the_newest_when_unnamed() {
        let script = transcript_script("/srv/app", None, 1024);
        assert!(script.contains("ls -1t"), "{script}");
        assert!(!script.contains(".jsonl\n["), "should not address a named file: {script}");
    }
}
