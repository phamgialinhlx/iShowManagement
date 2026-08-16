//! Watch Claude Code's own session files and stream status changes to the client.
//!
//! ## Why this exists
//!
//! rmux used to learn "is this session working?" by polling the terminal screen
//! several times a second **per pane** and parsing it (`rmux_claude::parse_state`).
//! That poll — one IPC round trip plus a React re-render for every rendered pane,
//! every 400 ms, and never pausing when the window was hidden — kept the client's
//! CPU package awake and warmed the laptop out of all proportion to its ~1%
//! average CPU. Measured with `powermetrics`: ~75–82 wakeups/s across the renderer
//! and core *even while backgrounded*, scaling linearly with pane count.
//!
//! Claude Code already writes an authoritative signal: `~/.claude/sessions/<pid>.json`,
//! updated live with `status ∈ busy | shell | idle | waiting`. It lives on the
//! host, where this agent runs, so watching it here and pushing only the *changes*
//! to the client replaces the busy-poll with an event stream — the client is told
//! when something changes and does nothing in between.
//!
//! ## What it emits
//!
//! One JSON object per line (NDJSON) on stdout, which the client reads over the
//! same ssh transport every other agent subcommand uses. A first `{"ready":true}`
//! line marks the stream live even before any session exists, so the client can
//! tell "supported, nothing running" apart from an older agent that errors on an
//! unknown subcommand. Thereafter one line per status change, and a final
//! `"gone"` when a session's file disappears or its process dies.
//!
//! ## Why it need not touch the daemon
//!
//! The files are host-global and self-describing — each carries `sessionId` and
//! `cwd`. So this needs neither the daemon's session map nor any pid→file
//! resolution (which would have dragged in the multi-daemon union problem): it
//! watches the directory and reports every Claude it sees, keyed by `sessionId`,
//! and the client matches those to its own sessions.
//!
//! ## Never bound to Claude's schema
//!
//! Fields are picked out one at a time, exactly like `rmux_claude::transcript` —
//! a strict deserialise would turn every Claude Code release that adds a field
//! into "no sessions have any status".

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

/// How often to re-scan as a safety net behind the file watcher.
///
/// inotify can miss events, the directory may not exist yet, and a recorded pid
/// can die without the file changing — so a periodic re-scan re-checks all three.
/// Longer when the watcher is live (it only backstops), shorter when it is not
/// (it is then the only source of updates). This is host-side work — one
/// `readdir` — and invisible to the client, which only ever receives diffs.
const BACKSTOP_WATCHED: Duration = Duration::from_secs(5);
const BACKSTOP_UNWATCHED: Duration = Duration::from_secs(2);

/// One status update, sent to the client as a single NDJSON line.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Update {
    /// Claude's own conversation id — the exact key the client maps to a session.
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    /// `busy | shell | idle | waiting` from Claude, or `gone` when the file has
    /// disappeared or its process is no longer alive.
    status: String,
    /// `statusUpdatedAt` — the host-clock millisecond epoch of the last status
    /// change. The client uses it as a skew-free watermark to tell an "unseen"
    /// finished session (changed since you last looked) from one you have
    /// acknowledged — the read/unread axis the old inventory kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<u64>,
}

/// What was last reported for a file, so only genuine changes are emitted.
struct Known {
    session_id: String,
    status: String,
}

/// The fields we read out of one session file.
struct Parsed {
    session_id: String,
    status: String,
    cwd: Option<String>,
    pid: Option<u32>,
    updated_at: Option<u64>,
}

/// Read `statusUpdatedAt` (or a legacy `updatedAt`), tolerating both a JSON number
/// — which is what Claude actually writes (`1785474593641`) — and a numeric
/// string, so a schema tweak either way keeps working. `as_str()` alone silently
/// dropped the field, because the real value is a number.
fn read_updated_at(v: &serde_json::Value) -> Option<u64> {
    let field = v.get("statusUpdatedAt").or_else(|| v.get("updatedAt"))?;
    field.as_u64().or_else(|| field.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Read the fields we care about, tolerating partial writes and unknown shapes.
fn parse_file(path: &Path) -> Option<Parsed> {
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    // sessionId is the load-bearing field — without it there is nothing the
    // client can key on, so a file lacking it is skipped entirely.
    let session_id = v.get("sessionId")?.as_str()?.to_owned();
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("idle").to_owned();
    let cwd = v.get("cwd").and_then(|s| s.as_str()).map(str::to_owned);
    let pid = v.get("pid").and_then(serde_json::Value::as_u64).map(|p| p as u32);
    let updated_at = read_updated_at(&v);
    Some(Parsed { session_id, status, cwd, pid, updated_at })
}

/// Is a pid still alive?
///
/// `kill(pid, 0)` sends no signal: `0` means alive and ours, `EPERM` means alive
/// but owned by someone else, `ESRCH` means dead.
///
/// Compiled only off Linux (where it backs the `pid_is_claude` fallback) and in
/// tests. On Linux the `/proc`-based `pid_is_claude` is the whole check, so this
/// would be dead code there.
#[cfg(all(unix, any(not(target_os = "linux"), test)))]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(all(not(unix), any(not(target_os = "linux"), test)))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// Whether `pid` is a live `claude` process — the **recycled-pid guard**.
///
/// A crashed Claude can leave its session file behind reading `busy` forever, and
/// once the OS reuses that pid for another program `kill(pid, 0)` still reports
/// "alive" — so the stale file's last status would light a rail dot for a session
/// that has actually ended. Checking that the pid is not merely alive but still
/// *`claude`* closes both cases. This is ported from the old tmux inventory, which
/// guarded on `comm == "claude"` and had a dedicated recycled-pid test.
///
/// The agent only ever runs on Linux, so `/proc` is always present in production.
#[cfg(target_os = "linux")]
fn pid_is_claude(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        // `comm` is the bare command name; normalise like the old `comm_base`
        // (drop any path and a login shell's leading '-') before comparing.
        Ok(comm) => comm.trim().rsplit('/').next().unwrap_or("").trim_start_matches('-') == "claude",
        // No such pid — dead.
        Err(_) => false,
    }
}

/// Off Linux — where the unit tests build — there is no `/proc`, so fall back to
/// plain liveness. The recycled-pid guard proper is exercised with an injected
/// checker in the tests, which is platform-independent.
#[cfg(not(target_os = "linux"))]
fn pid_is_claude(pid: u32) -> bool {
    pid_alive(pid)
}

/// The status to report — `gone` when the file's pid is no longer a live Claude
/// (dead, or its pid reused by another process), otherwise Claude's own status.
fn effective_status(parsed: &Parsed, is_claude: &dyn Fn(u32) -> bool) -> String {
    match parsed.pid {
        Some(pid) if !is_claude(pid) => "gone".to_owned(),
        _ => parsed.status.clone(),
    }
}

/// One Claude session's live status, as the bridge needs to report it.
///
/// The same fields the NDJSON stream carries, handed back in memory instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub session_id: String,
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    /// `busy | shell | idle | waiting`, or `gone`.
    pub status: String,
    pub updated_at: Option<u64>,
}

/// Every Claude session on this host, right now.
///
/// A one-shot read of the directory [`watch_status`] streams, for the callers
/// that want an answer rather than a subscription — the bridge builds its
/// session list from this on every `listSessions`.
///
/// **The same parser and the same liveness check as the stream.** A second
/// reader of Claude's session files would be a second opinion about what "busy"
/// means, and the two would disagree within a release; that is exactly the
/// duplication the whole `rmux-claude-core` split exists to avoid.
///
/// A missing directory is an empty list, not an error: Claude may simply never
/// have run here, which is a fact about the host rather than a failure.
pub fn snapshot() -> Vec<Snapshot> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let dir = home.join(".claude").join("sessions");

    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // A partial write parses as nothing and is skipped, exactly as in `scan`.
        let Some(parsed) = parse_file(&path) else { continue };
        let status = effective_status(&parsed, &pid_is_claude);
        // A dead session is left out rather than reported as `gone`. Its pid may
        // already have been reused, and a caller shown one would be pointed at
        // some other process entirely — the same reasoning as the daemon's own
        // session summary.
        if status == "gone" {
            continue;
        }
        out.push(Snapshot {
            session_id: parsed.session_id,
            cwd: parsed.cwd,
            pid: parsed.pid,
            status,
            updated_at: parsed.updated_at,
        });
    }
    out
}

fn emit<W: Write>(out: &mut W, update: &Update) -> std::io::Result<()> {
    // Serialising an `Update` cannot fail (all fields are plain strings/ints), so
    // an empty line here would only ever mean a bug — never a runtime condition.
    let line = serde_json::to_string(update).unwrap_or_default();
    writeln!(out, "{line}")
}

/// One pass over the directory: emit a line for every session whose status has
/// changed since the last pass, and a `gone` line for every file that has since
/// disappeared.
fn scan<W: Write>(
    dir: &Path,
    known: &mut HashMap<PathBuf, Known>,
    out: &mut W,
    is_claude: &dyn Fn(u32) -> bool,
) -> std::io::Result<()> {
    let mut seen: HashSet<PathBuf> = HashSet::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            seen.insert(path.clone());

            // A partial write parses as nothing; the next pass (woken by the
            // watcher's close event, or the backstop) picks up the final state.
            let Some(parsed) = parse_file(&path) else { continue };
            let status = effective_status(&parsed, is_claude);

            let changed = known.get(&path).map(|k| k.status != status).unwrap_or(true);
            if changed {
                emit(
                    out,
                    &Update {
                        session_id: parsed.session_id.clone(),
                        cwd: parsed.cwd.clone(),
                        pid: parsed.pid,
                        status: status.clone(),
                        updated_at: parsed.updated_at,
                    },
                )?;
                known.insert(path, Known { session_id: parsed.session_id, status });
            }
        }
    }

    // Files gone since last pass — the session ended. Reported once, using the
    // sessionId we recorded, so the client can clear the rail dot.
    let vanished: Vec<PathBuf> =
        known.keys().filter(|p| !seen.contains(*p)).cloned().collect();
    for path in vanished {
        if let Some(k) = known.remove(&path)
            && k.status != "gone"
        {
            emit(
                out,
                &Update {
                    session_id: k.session_id,
                    cwd: None,
                    pid: None,
                    status: "gone".to_owned(),
                    updated_at: None,
                },
            )?;
        }
    }

    out.flush()
}

/// Watch `~/.claude/sessions` on the host, best-effort. Returns `None` when the
/// directory does not exist yet (the backstop scan then carries the load until it
/// appears, and the loop re-arms).
fn spawn_watcher(
    dir: &Path,
    tx: tokio::sync::mpsc::Sender<()>,
) -> Option<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};

    let mut watcher = notify::recommended_watcher(move |_res| {
        // Capacity 1 with a dropped overflow is deliberate: a burst of inotify
        // events only ever means "scan again", and one pending wake does that.
        let _ = tx.try_send(());
    })
    .ok()?;
    watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
    Some(watcher)
}

/// Stream Claude session status changes as NDJSON on stdout until the pipe closes.
pub async fn watch_status() -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let dir = home.join(".claude").join("sessions");

    // The client waits for this to know the subcommand exists and the stream is
    // live — distinct from an older agent, which exits with an "unknown option"
    // error on stderr and a non-zero code.
    {
        let mut out = std::io::stdout();
        writeln!(out, "{}", serde_json::json!({ "ready": true }))?;
        out.flush()?;
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let mut watcher = spawn_watcher(&dir, tx.clone());
    let mut known: HashMap<PathBuf, Known> = HashMap::new();

    loop {
        {
            let mut out = std::io::stdout();
            scan(&dir, &mut known, &mut out, &pid_is_claude)?;
        }

        // The directory may have only just been created (a host where Claude had
        // never run when the stream opened). Re-arm the watcher when it appears.
        if watcher.is_none() && dir.exists() {
            watcher = spawn_watcher(&dir, tx.clone());
        }

        let backstop = if watcher.is_some() { BACKSTOP_WATCHED } else { BACKSTOP_UNWATCHED };
        tokio::select! {
            _ = rx.recv() => {}
            _ = tokio::time::sleep(backstop) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(dir: &Path, file: &str, session_id: &str, pid: u32, status: &str) {
        let body = serde_json::json!({
            "pid": pid,
            "sessionId": session_id,
            "cwd": "/srv/app",
            "status": status,
            // A number, like Claude actually writes it — the client reads this as
            // its skew-free watermark.
            "statusUpdatedAt": 1_785_474_593_641u64,
        });
        std::fs::write(dir.join(file), serde_json::to_vec(&body).unwrap()).unwrap();
    }

    fn lines(bytes: &[u8]) -> Vec<serde_json::Value> {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Treat every pid as a live Claude — isolates the scan/diff logic from the
    /// recycled-pid guard, which has its own tests below.
    fn all_claude(_pid: u32) -> bool {
        true
    }

    #[test]
    fn a_new_file_is_reported_once_and_not_again_unchanged() {
        let dir = tempdir();
        write_session(&dir, "1.json", "sess-1", std::process::id(), "busy");

        let mut known = HashMap::new();
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, &all_claude).unwrap();
        let first = lines(&out);
        assert_eq!(first.len(), 1, "one update for the new file: {first:?}");
        assert_eq!(first[0]["sessionId"], "sess-1");
        assert_eq!(first[0]["status"], "busy");
        // Carried through as a number, not dropped — the watermark depends on it.
        assert_eq!(first[0]["updatedAt"], 1_785_474_593_641u64);

        // A second pass with nothing changed must be silent, or the "push" is a
        // poll wearing a different hat.
        let mut out2 = Vec::new();
        scan(&dir, &mut known, &mut out2, &all_claude).unwrap();
        assert!(lines(&out2).is_empty(), "unchanged status must not re-emit");
    }

    #[test]
    fn only_a_status_change_re_emits() {
        let dir = tempdir();
        let pid = std::process::id();
        write_session(&dir, "1.json", "sess-1", pid, "busy");

        let mut known = HashMap::new();
        let mut sink = Vec::new();
        scan(&dir, &mut known, &mut sink, &all_claude).unwrap();

        write_session(&dir, "1.json", "sess-1", pid, "idle");
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, &all_claude).unwrap();
        let got = lines(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["status"], "idle");
    }

    #[test]
    fn a_removed_file_reports_gone() {
        let dir = tempdir();
        write_session(&dir, "1.json", "sess-1", std::process::id(), "busy");

        let mut known = HashMap::new();
        let mut sink = Vec::new();
        scan(&dir, &mut known, &mut sink, &all_claude).unwrap();

        std::fs::remove_file(dir.join("1.json")).unwrap();
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, &all_claude).unwrap();
        let got = lines(&out);
        assert_eq!(got.len(), 1, "the session ending is one event: {got:?}");
        assert_eq!(got[0]["sessionId"], "sess-1");
        assert_eq!(got[0]["status"], "gone");
    }

    #[test]
    fn a_dead_pid_reads_gone_even_with_the_file_present() {
        let dir = tempdir();
        // A pid that is almost certainly not running. Uses the real checker, which
        // off Linux is liveness and on Linux is a `/proc` miss — dead either way.
        // Reused-pid flakiness would only ever make this test *lenient*.
        write_session(&dir, "1.json", "sess-1", 0x7FFF_FFF0, "busy");

        let mut known = HashMap::new();
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, &pid_is_claude).unwrap();
        let got = lines(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["status"], "gone", "a stale busy file must not read as working");
    }

    #[test]
    fn a_recycled_pid_reads_gone_even_while_alive() {
        // The recycled-pid guard: the pid is alive but is no longer `claude` — the
        // old Claude exited and the OS handed its number to another program. A
        // liveness-only check would read this as still working; the identity check
        // must report `gone`. Mirrors the old inventory's
        // `stale_and_recycled_pids_excluded`.
        let dir = tempdir();
        write_session(&dir, "1.json", "sess-1", std::process::id(), "busy");

        let mut known = HashMap::new();
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, &|_| false).unwrap();
        let got = lines(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["status"], "gone", "a pid reused by a non-claude process is not working");
    }

    #[test]
    fn a_file_without_a_session_id_is_skipped() {
        let dir = tempdir();
        std::fs::write(dir.join("junk.json"), br#"{"status":"busy"}"#).unwrap();
        // A half-written file that is not yet valid JSON at all.
        std::fs::write(dir.join("partial.json"), br#"{"sessionId":"x","stat"#).unwrap();

        let mut known = HashMap::new();
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, &all_claude).unwrap();
        assert!(lines(&out).is_empty(), "unusable files produce no updates");
    }

    #[test]
    fn updated_at_reads_both_a_number_and_a_numeric_string() {
        // Claude writes a number; `as_str()` alone silently dropped it. A numeric
        // string is accepted too, so a schema tweak either way keeps the watermark.
        let num = serde_json::json!({ "statusUpdatedAt": 1234u64 });
        assert_eq!(read_updated_at(&num), Some(1234));
        let s = serde_json::json!({ "statusUpdatedAt": "1234" });
        assert_eq!(read_updated_at(&s), Some(1234));
        let legacy = serde_json::json!({ "updatedAt": 99u64 });
        assert_eq!(read_updated_at(&legacy), Some(99));
        let absent = serde_json::json!({ "status": "idle" });
        assert_eq!(read_updated_at(&absent), None);
    }

    #[test]
    fn liveness_and_identity_reject_pid_zero() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
        // Portable on any host: pid 0 is never a live claude.
        assert!(!pid_is_claude(0));
    }

    fn tempdir() -> PathBuf {
        // A unique directory under the system temp dir, without pulling in a
        // crate: the pid plus a counter is enough to keep parallel tests apart.
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmux-status-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
