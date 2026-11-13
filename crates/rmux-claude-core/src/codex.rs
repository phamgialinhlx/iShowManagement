//! OpenAI's Codex CLI, as data: how rmux launches it, how it signs in, where its
//! conversations live, and what its screen says about whether it is working.
//!
//! The Codex twin of [`crate::launch`] and [`crate::pi`], here for the same
//! reason they are: the desktop app (over ssh) and `rmux-agent` (on the host)
//! both launch Codex, and one definition is what stops the two drifting.
//!
//! ## Status is read off the screen, and that is a deliberate second choice
//!
//! Claude publishes its own status file; pi gets one through a bundled
//! extension. Codex has two hook mechanisms, and neither is usable:
//!
//! - **Lifecycle hooks** (`hooks.json` / `[hooks]` in `config.toml`) run only
//!   once the operator has *trusted* them — Codex stores a hash of each user hook
//!   it has been told to run, and an untrusted one is skipped. An installed hook
//!   would therefore do nothing until someone approved it inside the TUI, and
//!   writing the trust hash ourselves would be rmux bypassing a security control
//!   of another program. `--dangerously-bypass-hook-trust` exists and is
//!   exactly that, by name.
//! - **`notify`** (the legacy hook) fires only on `agent-turn-complete`, so it can
//!   say "idle" but never "working" or "waiting on an approval", and it is
//!   marked for removal upstream.
//!
//! So the state comes from the rendered screen, using the rules herdr maintains
//! for Codex ([`detect`] credits them). This is *observing* the TUI — the same
//! summary the rail always drew for a Claude with no session file — never
//! reimplementing it.

use serde::{Deserialize, Serialize};

pub use crate::pi::Conversation;
use crate::transcript::{Entry, Speaker, Transcript};

/// How Codex should be launched. Every field is a per-session choice the
/// operator made at launch, kept with the session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    /// A recorded conversation to continue (`codex resume <id>`).
    pub resume: Option<String>,
    /// `--sandbox <mode>`. Only the two safe modes are offered by name; the
    /// unsandboxed one is [`Launch::bypass`], an explicit, separate choice.
    pub sandbox: Option<Sandbox>,
    /// `--dangerously-bypass-approvals-and-sandbox`. The Codex equivalent of
    /// Claude's `--dangerously-skip-permissions`, and governed by the same rule:
    /// a per-launch judgement, never a stored default, off unless chosen.
    pub bypass: bool,
    /// `-m <model>` — from the session's OpenAI profile. Not a secret.
    pub model: Option<String>,
    /// `-c openai_base_url=…` — from the session's OpenAI profile. Current Codex
    /// reads the base URL from configuration rather than the environment, so the
    /// profile's `OPENAI_BASE_URL` becomes a config override here. Not a secret.
    pub base_url: Option<String>,
}

/// The sandbox policies offered by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sandbox {
    ReadOnly,
    WorkspaceWrite,
}

impl Sandbox {
    pub fn as_flag(self) -> &'static str {
        match self {
            Sandbox::ReadOnly => "read-only",
            Sandbox::WorkspaceWrite => "workspace-write",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "read-only" => Some(Sandbox::ReadOnly),
            "workspace-write" => Some(Sandbox::WorkspaceWrite),
            _ => None,
        }
    }
}

/// The flag that turns Codex's approvals *and* sandbox off.
pub const BYPASS_FLAG: &str = "--dangerously-bypass-approvals-and-sandbox";

/// The shell line rmux runs to launch Codex.
///
/// `codex resume <id> [flags]` continues a conversation; Codex's `resume`
/// subcommand accepts the same interactive flags as the bare command, so the
/// flags follow the id in both forms. Everything interpolated is
/// `shell_quote`d — a resume id, a model name and a base URL all reach a shell
/// that re-parses them. An optional initial prompt goes last, after `--`, so a
/// prompt that begins with `-` is not read as a flag.
pub fn launch_line(launch: &Launch, prompt: Option<&str>) -> String {
    let q = rmux_transport::shell_quote;
    let mut line = String::from("codex");
    if let Some(id) = launch.resume.as_deref().filter(|s| !s.is_empty()) {
        line.push_str(" resume ");
        if id.starts_with('-') {
            line.push_str("-- ");
        }
        line.push_str(&q(id));
    }
    if let Some(url) = launch.base_url.as_deref().filter(|s| !s.is_empty()) {
        // TOML string: the value is quoted *for TOML* first, then the whole
        // `key=value` is quoted for the shell. `-c` parses its value as TOML and
        // falls back to a bare string, but a URL with a `#` or a quote would not
        // survive the fallback.
        let toml = format!("openai_base_url={}", toml_string(url));
        line.push_str(" -c ");
        line.push_str(&q(&toml));
    }
    if let Some(model) = launch.model.as_deref().filter(|s| !s.is_empty()) {
        line.push_str(" -m ");
        line.push_str(&q(model));
    }
    if launch.bypass {
        // Codex refuses `--sandbox` together with the bypass, and the bypass is
        // the stronger statement, so it wins rather than the line failing.
        line.push(' ');
        line.push_str(BYPASS_FLAG);
    } else if let Some(mode) = launch.sandbox {
        line.push_str(" --sandbox ");
        line.push_str(mode.as_flag());
    }
    if let Some(prompt) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
        line.push_str(" -- ");
        line.push_str(&q(prompt));
    }
    line
}

/// A TOML basic string.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The shell line that signs Codex in on a host.
///
/// **`--device-auth`, always.** The default `codex login` opens a browser and
/// waits for a callback on `localhost:1455` *of the machine Codex runs on* — on a
/// server, that is a port the operator's browser cannot reach, so the flow
/// would hang. The device-code flow prints a URL and a code to type into any
/// browser, which works identically on this Mac and on a host, so there is no
/// local/remote branch to write. rmux runs the real CLI in a terminal and never
/// scripts the browser flow itself — same reasoning as `claude setup-token`.
pub fn login_line() -> &'static str {
    "codex login --device-auth"
}

// ── status from the screen ────────────────────────────────────────────────────

/// What Codex's screen says it is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Working,
    /// Waiting on the operator — an approval, a trust question, an update gate.
    Blocked,
    Idle,
    /// Nothing recognisable (starting up, a transcript viewer open). Never
    /// guessed as idle: reporting "finished" for work in progress is the one
    /// thing the rail must not do.
    Unknown,
}

/// The composer's prompt glyph. Codex draws its input line as `› …`.
const PROMPT: char = '›';

/// Glyphs that begin a Codex transcript cell (a response, a tool call, a
/// separator). A status line that has one of these *after* it is stale.
const CELL_GLYPHS: &[char] = &['•', '◦', '›', '■', '✗', '✓', '─'];

/// Read Codex's state off its rendered screen, one string per row.
///
/// The rules — regions, markers, priorities — are adapted from herdr's Codex
/// detection manifest (`src/detect/manifests/codex.toml`, Apache-2.0,
/// <https://github.com/ogulcancelik/herdr>), which tracks Codex's TUI across
/// releases. Priorities follow it: a blocker beats work, work beats idle. The OSC
/// title rules are omitted because the emulator rmux feeds does not keep the
/// title; the composer's presence stands in for herdr's title-based idle rule.
pub fn detect(lines: &[String]) -> State {
    let lines: Vec<&str> = lines.iter().map(|l| l.trim_end()).collect();
    let non_empty: Vec<&str> = lines.iter().copied().filter(|l| !l.trim().is_empty()).collect();
    let prompt_at = lines.iter().rposition(|l| l.trim_start().starts_with(PROMPT));
    let after_prompt: Vec<&str> = match prompt_at {
        Some(i) => lines[i..].to_vec(),
        None => lines.clone(),
    };
    let lower = |v: &[&str]| v.join("\n").to_lowercase();

    // `transcript_viewer`: Codex's full-screen transcript pager. Neither working
    // nor idle — say nothing rather than something wrong.
    let tail = lower(&after_prompt);
    if ["↑/↓ to scroll", "pgup/pgdn to", "home/end to jump", "q to quit"]
        .iter()
        .all(|m| tail.contains(m))
        && (tail.contains("esc to edit prev") || tail.contains("esc/← to edit prev"))
    {
        return State::Unknown;
    }

    // `trust_directory`: the first-run folder-trust question, at the top.
    let top = non_empty.iter().take(20).copied().collect::<Vec<_>>().join("\n");
    let first = non_empty.first().copied().unwrap_or("");
    if (first.starts_with("> You are in ") || top.contains("Folder access"))
        && (squash(&top).contains("Do you trust the contents of this directory?")
            || (top.contains("Trust this folder?")
                && top.contains("Codex can read, edit, and run files here")
                && (top.contains("Trust and continue") || top.contains("enter continue"))))
    {
        return State::Blocked;
    }

    // `startup_update`: the "update available" gate, at the bottom.
    let bottom: Vec<&str> =
        non_empty.iter().rev().take(20).rev().copied().collect::<Vec<_>>();
    let bottom_text = bottom.join("\n");
    let last = bottom.last().map(|l| l.trim()).unwrap_or("");
    if (bottom_text.contains("Update available!")
        && bottom_text.contains("Update now")
        && (squash(&bottom_text).contains("Skip until next version")
            || last.ends_with("Press enter to continue")))
        || (bottom.iter().any(|l| l.trim_start().starts_with("Update available ·"))
            && squash(last).ends_with("enter continue · esc skip"))
    {
        return State::Blocked;
    }

    // `live_strong_blocker`: an approval or a question on screen right now.
    if [
        "press enter to confirm or esc to cancel",
        "enter to submit answer",
        "enter to submit all",
        "allow command?",
    ]
    .iter()
    .any(|m| tail.contains(m))
        || (tail.contains("all results") && tail.contains("filesystem only") && tail.contains("plugins"))
    {
        return State::Blocked;
    }

    // `screen_working_fallback`: the live status line with its elapsed timer,
    // with nothing but queued input or plain text below it.
    let before_prompt = match prompt_at {
        Some(i) => &lines[..i],
        None => &lines[..],
    };
    if working(before_prompt) {
        return State::Working;
    }

    // `weak_blocker`: a y/n question near the bottom, outside the composer.
    // Scoped to the last lines so prose that once asked a question further up
    // the scrollback does not pin the session on "waiting".
    let recent = lower(
        &before_prompt
            .iter()
            .copied()
            .filter(|l| !l.trim().is_empty())
            .rev()
            .take(12)
            .collect::<Vec<_>>(),
    );
    if recent.contains("[y/n]")
        || recent.contains("yes (y)")
        || ((recent.contains("do you want to") || recent.contains("would you like to"))
            && (recent.contains("yes") || recent.contains('❯')))
    {
        return State::Blocked;
    }

    if prompt_at.is_some() { State::Idle } else { State::Unknown }
}

/// Collapse runs of whitespace, so a question wrapped across rows still matches.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Is the region above the composer ending in a live status line?
///
/// Walks up from the bottom. Blank and plain lines are allowed below the status
/// (wrapped text, a hint row); a queued-input notice is allowed too. The first
/// *cell* glyph that is not the status line means a later response was drawn,
/// so any timer above it is from a finished turn.
fn working(region: &[&str]) -> bool {
    for line in region.iter().rev() {
        let t = line.trim_start();
        if t.is_empty() {
            continue;
        }
        if is_status_line(t) {
            return true;
        }
        let Some(first) = t.chars().next() else { continue };
        if CELL_GLYPHS.contains(&first) {
            if first == '•' && is_queue_notice(t) {
                continue;
            }
            return false;
        }
    }
    false
}

fn is_queue_notice(line: &str) -> bool {
    let body = line.trim_start_matches(['•', '◦']).trim_start();
    body.starts_with("Queued follow-up inputs")
        || body.starts_with("Messages to be submitted after next tool call")
        || body.starts_with("Messages to be submitted at end of turn")
}

/// `Working (12s • esc to interrupt)`, `• Running tests (1m 3s)`, and the like.
fn is_status_line(line: &str) -> bool {
    let body = match line.chars().next() {
        Some('•' | '◦') => line[line.char_indices().nth(1).map(|(i, _)| i).unwrap_or(line.len())..]
            .trim_start(),
        _ => line,
    };
    let Some(first) = body.chars().next() else { return false };
    if first.is_whitespace() || CELL_GLYPHS.contains(&first) {
        return false;
    }
    // A failed reconnect keeps its last timer but is no longer working.
    if body.starts_with("Reconnect failed — check the endpoint, then relaunch (") {
        return false;
    }
    // The timer group may be followed by ` · <anything>`; strip that first.
    let core = match body.find(") · ") {
        Some(i) => &body[..=i],
        None => body,
    };
    let Some(core) = core.strip_suffix(')') else { return false };
    let Some(open) = core.rfind(" (") else { return false };
    let inside = &core[open + 2..];
    let timer = match inside.split_once(" • ") {
        Some((timer, hint)) => {
            if !hint.ends_with(" to interrupt") {
                return false;
            }
            timer
        }
        None => inside,
    };
    is_timer(timer)
}

/// `(<n>[hm] )*<n>s` — the elapsed clock Codex prints while a turn runs. The
/// unit is required, or prose ending "(see 2)" would read as working forever.
fn is_timer(timer: &str) -> bool {
    let parts: Vec<&str> = timer.split(' ').collect();
    let Some((last, rest)) = parts.split_last() else { return false };
    let unit = |p: &str, units: &[char]| {
        let Some(u) = p.chars().last() else { return false };
        let digits = &p[..p.len() - u.len_utf8()];
        units.contains(&u) && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
    };
    unit(last, &['s']) && rest.iter().all(|p| unit(p, &['h', 'm']))
}

// ── conversations on disk ─────────────────────────────────────────────────────

/// Where Codex keeps recorded conversations, as a shell expression.
///
/// `$CODEX_HOME` when the operator set one, `~/.codex` otherwise. Expanded by the
/// *remote* shell, so it is never `shell_quote`d.
const SESSIONS_ROOT: &str = "${CODEX_HOME:-$HOME/.codex}/sessions";

/// Shell that lists every recorded Codex conversation on a host.
///
/// Codex writes `sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`, whose first
/// line is a `session_meta` record carrying the id and cwd. Per file this emits
/// four **NUL-separated** fields — path, mtime (s), size, first line — the same
/// framing [`crate::pi::parse_all_conversations`] reads, because a cwd may
/// contain any byte but NUL. Only the first line is read: a rollout grows for
/// as long as the conversation does, and a listing needs nothing past line one.
/// The first line carries Codex's instructions and can run to tens of kilobytes,
/// so the guard is wider than pi's; a line past it simply falls back to the id
/// in the file name.
pub fn list_all_conversations_script() -> String {
    format!(
        r#"root="{SESSIONS_ROOT}"
[ -d "$root" ] || exit 0
find "$root" -type f -name 'rollout-*.jsonl' 2>/dev/null | while IFS= read -r f; do
  mt=$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null || echo 0)
  sz=$(stat -c %s "$f" 2>/dev/null || stat -f %z "$f" 2>/dev/null || echo 0)
  hdr=$(head -c 262144 "$f" 2>/dev/null | head -n 1)
  printf '%s\0%s\0%s\0%s\0' "$f" "$mt" "$sz" "$hdr"
done"#
    )
}

/// The conversation id in a rollout file name: the trailing UUID.
fn id_from_path(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?.strip_suffix(".jsonl")?;
    // `rollout-2025-11-27T10-05-57-<uuid>` — a UUID is 36 characters.
    (name.len() > 36).then(|| name[name.len() - 36..].to_owned()).filter(|id| {
        id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') && id.matches('-').count() == 4
    })
}

/// Parse what [`list_all_conversations_script`] emits, newest first.
///
/// The `session_meta` line is the authority for id and cwd, read field by field
/// so a Codex release that adds to it changes nothing here. A first line that
/// will not parse falls back to the id in the file name, with no cwd.
pub fn parse_all_conversations(bytes: &[u8]) -> Vec<Conversation> {
    let mut out = Vec::new();
    let mut fields = bytes.split(|b| *b == 0);

    while let Some(path) = fields.next() {
        if path.is_empty() {
            break;
        }
        let (Some(mt), Some(sz), Some(hdr)) = (fields.next(), fields.next(), fields.next()) else {
            break;
        };
        let path = String::from_utf8_lossy(path).into_owned();
        let meta = serde_json::from_slice::<serde_json::Value>(hdr).ok();
        let payload = meta
            .as_ref()
            .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("session_meta"))
            .and_then(|v| v.get("payload"));
        let id = payload
            .and_then(|p| p.get("id"))
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| id_from_path(&path));
        let Some(id) = id else { continue };
        out.push(Conversation {
            id,
            cwd: payload.and_then(|p| p.get("cwd")).and_then(|s| s.as_str()).map(str::to_owned),
            summary: None,
            modified: String::from_utf8_lossy(mt).trim().parse::<u64>().ok().map(|s| s * 1000),
            size: String::from_utf8_lossy(sz).trim().parse::<u64>().ok(),
        });
    }

    out.sort_by_key(|c| std::cmp::Reverse(c.modified.unwrap_or(0)));
    out
}

/// Shell that emits a Codex conversation's id, size, then the tail of its file —
/// the same `id\0size\0body` framing as [`crate::transcript::transcript_script`].
///
/// With no id, the newest rollout whose `session_meta` names `cwd` is taken, so a
/// fresh session's transcript is "the one I am talking to". Emits nothing when
/// there is no such file, which parses as an empty transcript.
pub fn transcript_script(cwd: &str, id: Option<&str>, tail_bytes: u64) -> String {
    let pick = match id {
        Some(id) => {
            // Matched on the file name's suffix; the id is quoted as one word
            // inside the glob, never left for the shell to split.
            let pattern = format!("rollout-*-{id}.jsonl");
            format!(
                "f=$(find \"$root\" -type f -name {} 2>/dev/null | head -n 1)",
                rmux_transport::shell_quote(&pattern)
            )
        }
        None => {
            // `"cwd":"<cwd>"` as JSON writes it. Matched as a fixed string on the
            // first line only, newest file first.
            let needle = format!("\"cwd\":{}", serde_json::Value::String(cwd.to_owned()));
            // Read line by line, never word-split: a `$CODEX_HOME` may contain a
            // space. The pick is printed from inside the pipeline because a
            // variable assigned there would die with its subshell.
            format!(
                "f=$(find \"$root\" -type f -name 'rollout-*.jsonl' -exec ls -1t {{}} + 2>/dev/null | \
                 while IFS= read -r c; do \
                 if head -c 262144 \"$c\" 2>/dev/null | head -n 1 | grep -qF -- {}; then printf '%s' \"$c\"; break; fi; \
                 done)",
                rmux_transport::shell_quote(&needle)
            )
        }
    };
    format!(
        r#"root="{SESSIONS_ROOT}"
[ -d "$root" ] || exit 0
{pick}
[ -n "$f" ] && [ -f "$f" ] || exit 0
id=$(basename "$f" .jsonl)
size=$(stat -c %s "$f" 2>/dev/null || stat -f %z "$f" 2>/dev/null || echo 0)
printf '%s\0%s\0' "$id" "$size"
tail -c {tail_bytes} "$f""#
    )
}

/// Parse a Codex rollout, defensively — the framed output of
/// [`transcript_script`].
///
/// Only `event_msg` records of type `user_message` and `agent_message` carry the
/// conversation as the operator saw it; everything else (reasoning, token
/// counts, raw response items, turn context) is skipped rather than bound to.
/// A command Codex ran is shown as a tool entry from `exec_command_end` when one
/// is present. The leading partial line of a tailed read is dropped only when the
/// read was actually truncated, or a short file loses its first message.
pub fn parse(bytes: &[u8]) -> Transcript {
    let mut parts = bytes.splitn(3, |b| *b == 0);
    let (Some(name), Some(size), Some(body)) = (parts.next(), parts.next(), parts.next()) else {
        return Transcript::default();
    };
    let name = String::from_utf8_lossy(name).trim().to_owned();
    let total_bytes = String::from_utf8_lossy(size).trim().parse::<u64>().unwrap_or(0);
    let truncated = total_bytes > body.len() as u64;
    let session_id = id_from_path(&format!("{name}.jsonl")).unwrap_or(name);

    let text = String::from_utf8_lossy(body);
    let mut entries = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if truncated && i == 0 {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("event_msg") {
            continue;
        }
        let Some(p) = v.get("payload") else { continue };
        let kind = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let (speaker, text, tool) = match kind {
            "user_message" => (Speaker::User, str_field(p, "message"), None),
            "agent_message" => (Speaker::Assistant, str_field(p, "message"), None),
            "exec_command_end" => {
                let out = str_field(p, "aggregated_output");
                let out = if out.is_empty() { str_field(p, "stdout") } else { out };
                (Speaker::Tool, out, Some("shell".to_owned()))
            }
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        entries.push(Entry {
            speaker,
            text,
            tool,
            timestamp: v.get("timestamp").and_then(|t| t.as_str()).map(str::to_owned),
        });
    }

    Transcript {
        session_id,
        entries,
        total_bytes,
        read_bytes: body.len() as u64,
        ..Default::default()
    }
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key).and_then(|s| s.as_str()).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_fresh_session_launches_bare() {
        assert_eq!(launch_line(&Launch::default(), None), "codex");
    }

    #[test]
    fn resume_flags_and_profile_settings_are_quoted() {
        let line = launch_line(
            &Launch {
                resume: Some("019ac346-54cb-7341-a18e-d78087e9a395".into()),
                sandbox: Some(Sandbox::WorkspaceWrite),
                bypass: false,
                model: Some("gpt-5; rm -rf /".into()),
                base_url: Some("https://gw.example.test/v1".into()),
            },
            None,
        );
        assert!(line.starts_with("codex resume 019ac346-54cb-7341-a18e-d78087e9a395 "), "{line}");
        assert!(line.contains("-m 'gpt-5; rm -rf /'"), "model must be one word: {line}");
        assert!(line.contains("-c 'openai_base_url=\"https://gw.example.test/v1\"'"), "{line}");
        assert!(line.ends_with("--sandbox workspace-write"), "{line}");
        assert!(!line.contains(BYPASS_FLAG), "{line}");
    }

    #[test]
    fn bypass_is_off_unless_chosen_and_replaces_the_sandbox_when_it_is() {
        assert!(!launch_line(&Launch::default(), None).contains(BYPASS_FLAG));
        let line = launch_line(
            &Launch { bypass: true, sandbox: Some(Sandbox::ReadOnly), ..Default::default() },
            None,
        );
        assert!(line.ends_with(BYPASS_FLAG), "{line}");
        // Codex refuses the two together; the line must not carry both.
        assert!(!line.contains("--sandbox"), "{line}");
    }

    #[test]
    fn a_prompt_is_one_word_after_double_dash() {
        let line = launch_line(&Launch::default(), Some("-rf it's fine; curl x | sh"));
        assert!(line.starts_with("codex -- "), "{line}");
        let rendered = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("set -- {}; echo $#", &line["codex -- ".len()..]))
            .output()
            .expect("sh");
        assert_eq!(String::from_utf8_lossy(&rendered.stdout).trim(), "1", "{line}");
    }

    #[test]
    fn a_resume_id_that_looks_like_a_flag_is_guarded() {
        let line = launch_line(&Launch { resume: Some("-x".into()), ..Default::default() }, None);
        assert!(line.starts_with("codex resume -- "), "{line}");
    }

    #[test]
    fn the_login_never_waits_on_a_localhost_callback() {
        assert!(login_line().contains("--device-auth"));
    }

    // Screens below are shaped on Codex's own TUI as herdr's manifest describes
    // it: a `• Working (…)` status cell above the `›` composer.

    #[test]
    fn a_running_turn_reads_working() {
        let s = screen(
            "› fix the failing test\n\n• Ran cargo test\n  └ 3 passed\n\n• Working (14s • esc to interrupt)\n\n› \n  ? for shortcuts",
        );
        assert_eq!(detect(&s), State::Working);
    }

    #[test]
    fn a_timer_with_minutes_and_a_trailing_note_still_reads_working() {
        let s = screen("• Exploring the repo (1m 3s • esc to interrupt) · 2 background terminals\n› ");
        assert_eq!(detect(&s), State::Working);
        let s = screen("• Thinking (2h 4m 9s)\n\n› ");
        assert_eq!(detect(&s), State::Working);
    }

    #[test]
    fn a_queued_message_below_the_status_keeps_it_working() {
        let s = screen(
            "• Working (5s • esc to interrupt)\n• Queued follow-up inputs\n  also update the docs\n› ",
        );
        assert_eq!(detect(&s), State::Working);
    }

    #[test]
    fn a_finished_turn_reads_idle() {
        // The old timer is still in the scrollback, but a response was drawn
        // after it — that is a finished turn, not a running one.
        let s = screen(
            "• Working (14s • esc to interrupt)\n• Done. The test passes now.\n\n› \n  ? for shortcuts",
        );
        assert_eq!(detect(&s), State::Idle);
    }

    #[test]
    fn prose_with_parentheses_is_not_a_timer() {
        let s = screen("• I changed the parser (see 2)\n• And the docs (section s)\n› ");
        assert_eq!(detect(&s), State::Idle);
    }

    #[test]
    fn a_failed_reconnect_is_not_working() {
        let s = screen("Reconnect failed — check the endpoint, then relaunch (12s)\n› ");
        assert_eq!(detect(&s), State::Idle);
    }

    #[test]
    fn an_approval_prompt_reads_blocked() {
        let s = screen(
            "• Working (3s • esc to interrupt)\n\n› \n  Allow command?\n  $ rm -rf build\n  › 1. Yes  2. No\n  Press enter to confirm or esc to cancel",
        );
        assert_eq!(detect(&s), State::Blocked);
    }

    #[test]
    fn the_folder_trust_question_reads_blocked() {
        let s = screen(
            "> You are in /home/a/project\n\n  Do you trust the contents of this\n  directory? Working with untrusted contents comes with higher risk.\n\n› 1. Yes, continue\n  2. No, quit",
        );
        assert_eq!(detect(&s), State::Blocked);
        let s = screen(
            "  Folder access\n  Trust this folder?\n  Codex can read, edit, and run files here.\n\n  Trust and continue",
        );
        assert_eq!(detect(&s), State::Blocked);
    }

    #[test]
    fn the_update_gate_reads_blocked() {
        let s = screen(
            "  ✨ Update available! 0.98 -> 0.99\n\n› 1. Update now\n  2. Skip\n  3. Skip until next version\n\n  Press enter to continue",
        );
        assert_eq!(detect(&s), State::Blocked);
    }

    #[test]
    fn the_transcript_pager_says_nothing() {
        let s = screen(
            "› hi\n/ T R A N S C R I P T\n↑/↓ to scroll  pgup/pgdn to page  home/end to jump  q to quit  esc to edit prev",
        );
        assert_eq!(detect(&s), State::Unknown);
    }

    #[test]
    fn an_unrecognised_screen_is_unknown_not_idle() {
        assert_eq!(detect(&screen("loading…")), State::Unknown);
        assert_eq!(detect(&[]), State::Unknown);
    }

    #[test]
    fn listing_reads_the_meta_line_and_falls_back_to_the_file_name() {
        let meta = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"019ac346-54cb-7341-a18e-d78087e9a395","cwd":"/srv/a b","future":1}}"#;
        let mut raw = Vec::new();
        for (path, mt, hdr) in [
            ("/h/.codex/sessions/2025/11/27/rollout-2025-11-27T10-05-57-019ac346-54cb-7341-a18e-d78087e9a395.jsonl", "100", meta),
            ("/h/.codex/sessions/2025/11/28/rollout-2025-11-28T10-05-57-11111111-2222-3333-4444-555555555555.jsonl", "200", "{truncated"),
        ] {
            raw.extend_from_slice(format!("{path}\0{mt}\0{}\0{hdr}\0", 10).as_bytes());
        }
        let list = parse_all_conversations(&raw);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "11111111-2222-3333-4444-555555555555", "newest first, id from name");
        assert_eq!(list[0].cwd, None);
        assert_eq!(list[1].id, "019ac346-54cb-7341-a18e-d78087e9a395");
        assert_eq!(list[1].cwd.as_deref(), Some("/srv/a b"));
        assert_eq!(list[1].modified, Some(100_000));
    }

    #[test]
    fn a_rollout_parses_tolerantly_and_drops_only_a_truncated_first_line() {
        let body = concat!(
            r#"{"type":"session_meta","payload":{"id":"x"}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"fix it"}}"#, "\n",
            r#"{"type":"response_item","payload":{"type":"reasoning"}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count"}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"done","new":true}}"#, "\n",
        );
        let framed = |size: usize| {
            let mut v = format!("rollout-2025-11-27T10-05-57-019ac346-54cb-7341-a18e-d78087e9a395\0{size}\0").into_bytes();
            v.extend_from_slice(body.as_bytes());
            v
        };
        let whole = parse(&framed(body.len()));
        assert_eq!(whole.session_id, "019ac346-54cb-7341-a18e-d78087e9a395");
        assert_eq!(whole.entries.len(), 2);
        assert_eq!(whole.entries[0].speaker, Speaker::User);
        assert_eq!(whole.entries[1].text, "done");

        // Same bytes, but the file is bigger than what was read: the first line
        // is a fragment and is skipped — here it is the meta line, so nothing
        // that matters is lost.
        let tailed = parse(&framed(body.len() * 10));
        assert_eq!(tailed.entries.len(), 2);
        assert_eq!(tailed.total_bytes, (body.len() * 10) as u64);
        assert!(parse(b"").entries.is_empty());
    }

    #[test]
    fn the_transcript_script_quotes_the_id_and_the_cwd() {
        let s = transcript_script("/srv/it's here", Some("a b"), 100);
        assert!(s.contains("'rollout-*-a b.jsonl'"), "{s}");
        let s = transcript_script("/srv/it's here", None, 100);
        assert!(s.contains("grep -qF -- "), "{s}");
        assert!(s.contains(&rmux_transport::shell_quote(r#""cwd":"/srv/it's here""#)), "{s}");
        assert!(s.contains("${CODEX_HOME:-$HOME/.codex}/sessions"), "{s}");
    }
}
