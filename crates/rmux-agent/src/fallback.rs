//! The host half of fallback chains: finding the conversation, reading its
//! transcript tail, looking at the screen, and keeping `~/.rmux/fallback.json`.
//!
//! The decisions are in `rmux_claude_core::fallback` (pure, tested); the
//! daemon owns the sessions and does the switch (`daemon::switch_chain`). This
//! module is the I/O between them, and every path takes the home directory as
//! an argument so a test can run the whole thing in a sandbox without touching
//! the real `~/.claude` or `~/.rmux`.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use rmux_claude_core::fallback::{self, FallbackLog, FallbackRecord, LimitHit};

/// How much of a transcript is read per check. The limit record is the last or
/// nearly the last line; a real transcript reaches 228 MB, so only ever the tail.
const TRANSCRIPT_TAIL: u64 = 64 * 1024;
/// How much of the scrollback is read when there is no transcript.
const SCREEN_TAIL: usize = 16 * 1024;
/// Lines from the bottom of the screen that may hold a limit message.
const SCREEN_LINES: usize = 15;
/// After a (re)spawn, the screen is not read for this long: a resumed session
/// re-renders its history, which can contain the very limit that moved it.
pub const SCREEN_GRACE_SECS: u64 = 15;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The host's offset from UTC in seconds, for reading "resets 3pm".
pub fn utc_offset() -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: localtime_r writes only into the struct we hand it.
        unsafe {
            let t = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if !libc::localtime_r(&t, &mut tm).is_null() {
                return tm.tm_gmtoff;
            }
        }
    }
    0
}

/// The home the daemon reads Claude's files from and writes the log under.
pub fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The conversation a Claude process is running, from Claude's own
/// `~/.claude/sessions/<pid>.json` — the authoritative source, written by
/// Claude itself. Fields are picked one at a time.
pub fn conversation_of_pid(home: &Path, pid: u32) -> Option<String> {
    let dir = home.join(".claude").join("sessions");
    let direct = dir.join(format!("{pid}.json"));
    let read = |p: &Path| -> Option<String> {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
        let file_pid = v.get("pid").and_then(|p| p.as_u64());
        if file_pid.is_some_and(|p| p != u64::from(pid)) {
            return None;
        }
        v.get("sessionId").and_then(|s| s.as_str()).map(str::to_owned)
    };
    if let Some(id) = read(&direct) {
        return Some(id);
    }
    // Some releases name the file differently; scan for a matching `pid`.
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = entry.path();
        let Ok(raw) = std::fs::read(&p) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else { continue };
        if v.get("pid").and_then(|x| x.as_u64()) == Some(u64::from(pid))
            && let Some(id) = v.get("sessionId").and_then(|s| s.as_str())
        {
            return Some(id.to_owned());
        }
    }
    None
}

/// The conversation named by a `--resume <id>` on the launch line.
pub fn conversation_of_line(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    while let Some(w) = words.next() {
        if w == "--resume" || w == "-r" {
            return words.next().map(|id| id.trim_matches('\'').to_owned()).filter(|s| !s.is_empty());
        }
    }
    None
}

/// `~/.claude/projects/<any>/<conversation>.jsonl`. The project directory's name
/// is not computable (CLAUDE.md), so every project directory is looked in —
/// one `stat` each, never a read.
pub fn transcript_path(home: &Path, conversation: &str) -> Option<PathBuf> {
    if !rmux_claude_core::env_kind::valid_set_name(conversation) {
        return None;
    }
    let projects = home.join(".claude").join("projects");
    std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|e| e.path().join(format!("{conversation}.jsonl")))
        .find(|p| p.is_file())
}

/// The last `TRANSCRIPT_TAIL` bytes, with a cut first line dropped.
pub fn read_tail(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TRANSCRIPT_TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if start > 0 { text.split_once('\n').map(|(_, r)| r.to_owned()).unwrap_or_default() } else { text })
}

/// A detected limit, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    pub hit: LimitHit,
    /// `transcript` or `screen`.
    pub source: &'static str,
    /// When the limit happened, for the debounce.
    pub seen_at: u64,
    /// The conversation, when known — what the relaunch resumes.
    pub conversation: Option<String>,
}

/// Look for a limit in one chained Claude session.
///
/// **The transcript, when there is one, is the only source.** It is what Claude
/// recorded as an API error, with a timestamp; the screen is a rendering that
/// can contain the same words as prose. The screen is read only when no
/// transcript can be found (a Claude too old to write one where we look, or a
/// conversation not yet flushed), and then only its bottom lines, only after
/// the respawn grace.
pub fn detect(
    home: &Path,
    pid: Option<u32>,
    line: Option<&str>,
    scrollback: &[u8],
    bound_since: u64,
    now: u64,
    utc_offset: i64,
) -> Option<Detection> {
    let conversation = pid
        .and_then(|p| conversation_of_pid(home, p))
        .or_else(|| line.and_then(conversation_of_line));
    if let Some(path) = conversation.as_deref().and_then(|c| transcript_path(home, c)) {
        let tail = read_tail(&path)?;
        let found = fallback::newest_transcript_limit(&tail, bound_since, now, utc_offset)?;
        return Some(Detection {
            hit: found.hit,
            source: "transcript",
            seen_at: found.at.unwrap_or(now),
            conversation,
        });
    }
    if now < bound_since + SCREEN_GRACE_SECS {
        return None;
    }
    let tail = &scrollback[scrollback.len().saturating_sub(SCREEN_TAIL)..];
    let text = crate::orchestrate::screen_text(tail);
    let hit = fallback::match_screen(&text, SCREEN_LINES, now, utc_offset)?;
    Some(Detection { hit, source: "screen", seen_at: now, conversation })
}

// ── The log ──────────────────────────────────────────────────────────────────

pub fn log_path(home: &Path) -> PathBuf {
    home.join(".rmux").join("fallback.json")
}

pub fn read_log(home: &Path) -> FallbackLog {
    std::fs::read(log_path(home))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

/// Append one record: `0700` directory, `0600` file, temp-then-rename, pruned.
pub fn append_log(home: &Path, record: FallbackRecord) -> anyhow::Result<()> {
    let path = log_path(home);
    let dir = path.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let log = fallback::log_append(read_log(home), record, now());
    let temp = dir.join(format!(".fallback.{}.tmp", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // The mode is set at creation, before a byte is written.
            opts.mode(0o600);
        }
        let mut f = opts.open(&temp)?;
        std::io::Write::write_all(&mut f, &serde_json::to_vec_pretty(&log)?)?;
    }
    std::fs::rename(&temp, &path)?;
    Ok(())
}

// ── The commands ─────────────────────────────────────────────────────────────

/// `rmux-agent fallback-log [--json]`: every recorded switch, and which set
/// every live agent session is bound to now. Names and labels only.
pub async fn run_log(args: &[String]) -> anyhow::Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let home = home().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let log = read_log(&home);
    let mut bindings = Vec::new();
    for s in crate::attach::list().await.unwrap_or_default() {
        if rmux_claude_core::env_kind::for_session(&s.name).is_none() {
            continue;
        }
        if let Some(report) = crate::attach::binding_anywhere(&s.name).await {
            bindings.push(report);
        }
    }
    if json {
        println!("{}", serde_json::json!({ "switches": log.switches, "bindings": bindings }));
        return Ok(());
    }
    for r in &log.switches {
        let to = r.to_label.clone().or_else(|| r.to.clone()).unwrap_or_else(|| "(nowhere — end of chain)".into());
        let from = r.from_label.clone().unwrap_or_else(|| r.from.clone());
        println!("{}\t{}\t{} -> {}\t{}\t{}", r.at, r.session, from, to, r.reason, r.outcome);
    }
    for b in &bindings {
        if let Some(binding) = &b.binding {
            let label = b.labels.get(&binding.set).cloned().unwrap_or_else(|| binding.set.clone());
            println!("bound\t{}\t{}\t{}", b.session, binding.kind, label);
        }
    }
    Ok(())
}

/// `rmux-agent fallback-switch --session NAME --to MEMBER`.
pub async fn run_switch(args: &[String]) -> anyhow::Result<()> {
    let (mut session, mut member) = (None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--session" => session = it.next().cloned(),
            "--to" => member = it.next().cloned(),
            other => anyhow::bail!("fallback-switch takes --session and --to, not {other:?}"),
        }
    }
    let session = crate::alias::resolve(&session.ok_or_else(|| anyhow::anyhow!("--session is required"))?);
    let member = member.ok_or_else(|| anyhow::anyhow!("--to is required"))?;
    crate::attach::switch_member(&session, &member).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/rmux-fallback-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn the_log_is_0600_in_a_0700_directory_and_bounded() {
        let home = sandbox("log");
        for i in 0..3 {
            append_log(&home, FallbackRecord { session: format!("s{i}"), at: now(), ..Default::default() }).unwrap();
        }
        let log = read_log(&home);
        assert_eq!(log.switches.len(), 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(log_path(&home)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dir = std::fs::metadata(home.join(".rmux")).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir, 0o700);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_conversation_comes_from_claude_s_own_session_file() {
        let home = sandbox("conv");
        let dir = home.join(".claude/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("4242.json"), r#"{"pid":4242,"sessionId":"conv-abc","status":"idle"}"#).unwrap();
        std::fs::write(dir.join("other.json"), r#"{"pid":7,"sessionId":"conv-seven"}"#).unwrap();
        assert_eq!(conversation_of_pid(&home, 4242).as_deref(), Some("conv-abc"));
        assert_eq!(conversation_of_pid(&home, 7).as_deref(), Some("conv-seven"));
        assert_eq!(conversation_of_pid(&home, 9), None);
        assert_eq!(conversation_of_line("X=1 claude --resume 'id-1' --x").as_deref(), Some("id-1"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_transcript_limit_wins_and_the_screen_is_ignored_when_it_exists() {
        let home = sandbox("detect");
        let proj = home.join(".claude/projects/-srv-api");
        std::fs::create_dir_all(&proj).unwrap();
        let rec = serde_json::json!({
            "type":"assistant","isApiErrorMessage":true,"timestamp":"2026-11-06T21:20:00Z",
            "message":{"content":[{"type":"text","text":"Claude AI usage limit reached|1794003600"}]}
        });
        std::fs::write(proj.join("conv-1.jsonl"), format!("{{\"type\":\"user\"}}\n{rec}\n")).unwrap();
        let now = 1_794_000_100;
        let d = detect(&home, None, Some("claude --resume conv-1"), b"", 1_793_999_000, now, 0).unwrap();
        assert_eq!(d.source, "transcript");
        assert_eq!(d.seen_at, 1_794_000_000);
        assert_eq!(d.hit.resets_at, Some(1_794_003_600));
        // Bound after the record: nothing — and the screen is not consulted even
        // though it shows the message, because the transcript is authoritative.
        let screen = b"Claude AI usage limit reached|1794003600\r\n";
        assert!(detect(&home, None, Some("claude --resume conv-1"), screen, now - 10, now, 0).is_none());
        // No transcript: the screen is read, after the grace.
        assert!(detect(&home, None, Some("claude"), screen, now - 5, now, 0).is_none());
        let s = detect(&home, None, Some("claude"), screen, now - 60, now, 0).unwrap();
        assert_eq!(s.source, "screen");
        let _ = std::fs::remove_dir_all(&home);
    }
}
