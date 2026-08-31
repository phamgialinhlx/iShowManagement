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
    /// Which agent this status describes — `claude` for the native session files,
    /// or whatever a `~/.rmux/status` writer stamped (`pi`, `codex`, …). Absent on
    /// the Claude stream so an older client that ignores the field sees no change;
    /// present for every agent-agnostic file, which is the whole point of the
    /// second source.
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
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
    /// Carried so the closing `gone` line can still name the agent whose session
    /// ended — the file is already gone by then, so there is nothing left to read
    /// it from.
    agent: Option<String>,
}

/// The fields we read out of one session file.
struct Parsed {
    session_id: String,
    status: String,
    cwd: Option<String>,
    pid: Option<u32>,
    updated_at: Option<u64>,
    /// `Some` only for the agent-agnostic `~/.rmux/status` files, which name the
    /// agent that wrote them. The Claude native files are always Claude, so this
    /// is left `None` there and filled in by the snapshot.
    agent: Option<String>,
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
    Some(Parsed { session_id, status, cwd, pid, updated_at, agent: None })
}

/// `~/.rmux/status` — the agent-agnostic status directory.
///
/// Claude writes an authoritative `~/.claude/sessions/<pid>.json`, and nothing
/// else does. Every other coding agent rmux runs — pi today, Codex tomorrow —
/// has no such file, so the rail and the bridge reported them as `Unknown`
/// forever, which is the one thing the rail exists not to do. This directory is
/// where a per-agent hook (pi's `session_start`/`agent_settled` extension, a
/// Codex `Stop` hook) drops the same fact Claude publishes for itself, written
/// through one Rust definition — [`write_status`] — so the schema has a single
/// owner rather than one copy per language.
fn rmux_status_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".rmux").join("status"))
}

/// Read one agent-agnostic status file.
///
/// The same tolerant, field-at-a-time reading as [`parse_file`], for the same
/// reason: a hook that gains a field must not blank the whole directory. The
/// filename stem backs the id when the body omits `sessionId`, so a half-written
/// file still keys onto something rather than vanishing.
fn parse_rmux_file(path: &Path) -> Option<Parsed> {
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let session_id = v
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(str::to_owned)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))?;
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("idle").to_owned();
    let cwd = v.get("cwd").and_then(|s| s.as_str()).map(str::to_owned);
    let pid = v.get("pid").and_then(serde_json::Value::as_u64).map(|p| p as u32);
    let updated_at = read_updated_at(&v);
    let agent = v.get("agent").and_then(|s| s.as_str()).map(str::to_owned);
    Some(Parsed { session_id, status, cwd, pid, updated_at, agent })
}

/// One status fact, on its way into `~/.rmux/status`.
///
/// This is the type the `rmux-agent hook` subcommand fills from a hook's
/// arguments and hands here. Keeping the whole write behind one function is the
/// point: a pi extension in TypeScript, a Codex hook in bash and a future
/// in-process caller must not each invent the directory mode, the filename
/// scheme or the field names — they shell out to `rmux-agent hook`, which is
/// this.
pub struct StatusWrite {
    /// The agent reporting — `pi`, `codex`, …. Stamped into the file so a reader
    /// can prefer the agent's own word for what it is.
    pub agent: String,
    /// The session id the client keys on. For pi this is its `<id>.jsonl` id, the
    /// same string [`crate::bridge`] lists a conversation under, so a status file
    /// and a transcript line join without a pid.
    pub session_id: String,
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    /// `busy | idle | waiting | shell`, or `gone` — which **removes** the file
    /// rather than leaving a marker, so the stream's own vanish path reports the
    /// end exactly once and a crash that skips this leaves nothing stale to age
    /// out.
    pub status: String,
}

/// Milliseconds since the epoch, or 0 if the clock is before it (it is not).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A filename-safe rendering of a session id.
///
/// Session ids are agent-chosen and usually already `[A-Za-z0-9_-]`, but a `/`
/// or a `..` in one would escape the status directory — so anything that is not
/// plainly safe becomes `-`. Collisions between two ids that differ only in a
/// sanitised character are accepted: they would be two files for what is almost
/// certainly one malformed id, which is strictly better than a path traversal.
fn status_key(session_id: &str) -> String {
    let mut key: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    if key.is_empty() {
        key.push('-');
    }
    key
}

/// Write (or, for `gone`, remove) one agent-agnostic status file.
///
/// `0700` directory, `0600` file, temp-then-rename so a reader never sees a
/// half-written line — the same handling every credential-adjacent path in rmux
/// uses, applied here because a status file names a working directory and a pid,
/// which is more than a stranger on a shared host should be handed.
pub fn write_status(w: &StatusWrite) -> anyhow::Result<()> {
    let dir = rmux_status_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let path = dir.join(format!("{}.json", status_key(&w.session_id)));

    // `gone` is an erasure, not a value: drop the file and let the watcher's
    // vanish path announce the end. Removing a file that is not there is success.
    if w.status == "gone" {
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e.into());
        }
        return Ok(());
    }

    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    let mut body = serde_json::Map::new();
    body.insert("agent".into(), w.agent.clone().into());
    body.insert("sessionId".into(), w.session_id.clone().into());
    if let Some(cwd) = &w.cwd {
        body.insert("cwd".into(), cwd.clone().into());
    }
    if let Some(pid) = w.pid {
        body.insert("pid".into(), pid.into());
    }
    body.insert("status".into(), w.status.clone().into());
    body.insert("statusUpdatedAt".into(), now_ms().into());
    let bytes = serde_json::to_vec(&serde_json::Value::Object(body))?;

    // Temp beside the target (same directory → same filesystem → atomic rename),
    // named by pid so two concurrent writers do not clobber one temp.
    let tmp = dir.join(format!(".{}.{}.tmp", status_key(&w.session_id), std::process::id()));
    std::fs::write(&tmp, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    match std::fs::rename(&tmp, &path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// Is a pid still alive?
///
/// `kill(pid, 0)` sends no signal: `0` means alive and ours, `EPERM` means alive
/// but owned by someone else, `ESRCH` means dead.
///
/// Backs both the off-Linux `pid_is_claude` fallback and the liveness check for
/// the agent-agnostic `~/.rmux/status` files — those name any agent, not just
/// `claude`, so a `/proc/<pid>/comm == "claude"` test is the wrong question for
/// them. Compiled on every target now that it has a production caller on Linux.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
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

/// One coding-agent session's live status, as the bridge needs to report it.
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
    /// The agent this describes — `claude` for the native session files, or the
    /// name a `~/.rmux/status` writer stamped (`pi`, …). The bridge already
    /// derives a session's *kind* from its name prefix, so this is advisory; it
    /// lets a caller that has only a status prefer the agent's own word.
    pub agent: Option<String>,
}

/// Read one status directory into snapshots, dropping dead sessions.
///
/// Shared by the two sources so "what does a live session look like" has one
/// definition. `parse` reads the directory's schema, `is_live` is its liveness
/// question — `pid_is_claude` for Claude's files (a reused pid is a *different*
/// program), plain `pid_alive` for the agent-agnostic ones (any agent, so the
/// identity of the pid is not ours to assert).
fn snapshot_dir(
    dir: &Path,
    parse: fn(&Path) -> Option<Parsed>,
    is_live: &dyn Fn(u32) -> bool,
    default_agent: Option<&str>,
    out: &mut Vec<Snapshot>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // A partial write parses as nothing and is skipped, exactly as in `scan`.
        let Some(parsed) = parse(&path) else { continue };
        let status = effective_status(&parsed, is_live);
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
            agent: parsed.agent.or_else(|| default_agent.map(str::to_owned)),
        });
    }
}

/// Every coding-agent session on this host, right now.
///
/// A one-shot read of the directories [`watch_status`] streams, for the callers
/// that want an answer rather than a subscription — the bridge builds its
/// session list from this on every `listSessions`.
///
/// **Two sources, one merge.** Claude publishes its own
/// `~/.claude/sessions/<pid>.json`; every other agent's hook drops a file in
/// `~/.rmux/status`. A session that somehow appears in both (a future Claude
/// hook beside the native file) is de-duplicated by `sessionId` with the native
/// file winning, because it carries the finer `busy | shell | idle | waiting`
/// vocabulary the hook only approximates.
///
/// **The same parser and the same liveness check as the stream.** A second
/// reader of these files would be a second opinion about what "busy" means, and
/// the two would disagree within a release; that is exactly the duplication the
/// whole `rmux-claude-core` split exists to avoid.
///
/// A missing directory is an empty list, not an error: an agent may simply never
/// have run here, which is a fact about the host rather than a failure.
pub fn snapshot() -> Vec<Snapshot> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let claude_dir = home.join(".claude").join("sessions");

    let mut out = Vec::new();
    // Claude first, so it wins the de-dup below.
    snapshot_dir(&claude_dir, parse_file, &pid_is_claude, Some("claude"), &mut out);
    let claude_count = out.len();

    if let Some(rmux_dir) = rmux_status_dir() {
        let mut extra = Vec::new();
        snapshot_dir(&rmux_dir, parse_rmux_file, &pid_alive, None, &mut extra);
        // Drop any agent-agnostic file that names a session Claude already
        // reported for itself — the native reading is authoritative. Owned keys,
        // so the set does not borrow `out` across the `extend` that follows.
        let seen: std::collections::HashSet<String> =
            out[..claude_count].iter().map(|s| s.session_id.clone()).collect();
        out.extend(extra.into_iter().filter(|s| !seen.contains(&s.session_id)));
    }
    out
}

fn emit<W: Write>(out: &mut W, update: &Update) -> std::io::Result<()> {
    // Serialising an `Update` cannot fail (all fields are plain strings/ints), so
    // an empty line here would only ever mean a bug — never a runtime condition.
    let line = serde_json::to_string(update).unwrap_or_default();
    writeln!(out, "{line}")
}

/// One pass over one directory: emit a line for every session whose status has
/// changed since the last pass, and a `gone` line for every file that has since
/// disappeared.
///
/// Generalised over its source so the Claude native files and the agent-agnostic
/// `~/.rmux/status` files run the identical diff/vanish machinery — `parse`
/// reads the schema, `is_live` asks the right liveness question, `default_agent`
/// labels a file that did not name itself. Each source keeps its **own** `known`
/// map: sharing one would make every rmux file look "vanished" from the Claude
/// directory on every pass and stream a spurious `gone`.
fn scan<W: Write>(
    dir: &Path,
    known: &mut HashMap<PathBuf, Known>,
    out: &mut W,
    parse: fn(&Path) -> Option<Parsed>,
    is_live: &dyn Fn(u32) -> bool,
    default_agent: Option<&str>,
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
            let Some(parsed) = parse(&path) else { continue };
            let status = effective_status(&parsed, is_live);
            let agent = parsed.agent.clone().or_else(|| default_agent.map(str::to_owned));

            let changed = known.get(&path).map(|k| k.status != status).unwrap_or(true);
            if changed {
                emit(
                    out,
                    &Update {
                        session_id: parsed.session_id.clone(),
                        agent: agent.clone(),
                        cwd: parsed.cwd.clone(),
                        pid: parsed.pid,
                        status: status.clone(),
                        updated_at: parsed.updated_at,
                    },
                )?;
                known.insert(path, Known { session_id: parsed.session_id, status, agent });
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
                    agent: k.agent,
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

/// Stream coding-agent status changes as NDJSON on stdout until the pipe closes.
///
/// Two directories, one stream: Claude's own `~/.claude/sessions` and the
/// agent-agnostic `~/.rmux/status`. Each has its own watcher, backstop and
/// `known` map — they are independent sources that happen to share a wire
/// format, and one shared `known` would cross-report the other's files as
/// vanished. A single wake channel is enough: any event on either directory just
/// means "scan both again".
pub async fn watch_status() -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let claude_dir = home.join(".claude").join("sessions");
    let rmux_dir = rmux_status_dir();

    // The client waits for this to know the subcommand exists and the stream is
    // live — distinct from an older agent, which exits with an "unknown option"
    // error on stderr and a non-zero code.
    {
        let mut out = std::io::stdout();
        writeln!(out, "{}", serde_json::json!({ "ready": true }))?;
        out.flush()?;
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let mut claude_watcher = spawn_watcher(&claude_dir, tx.clone());
    let mut rmux_watcher = rmux_dir.as_ref().and_then(|d| spawn_watcher(d, tx.clone()));
    let mut claude_known: HashMap<PathBuf, Known> = HashMap::new();
    let mut rmux_known: HashMap<PathBuf, Known> = HashMap::new();

    loop {
        {
            let mut out = std::io::stdout();
            scan(&claude_dir, &mut claude_known, &mut out, parse_file, &pid_is_claude, Some("claude"))?;
            if let Some(dir) = &rmux_dir {
                scan(dir, &mut rmux_known, &mut out, parse_rmux_file, &pid_alive, None)?;
            }
        }

        // Either directory may have only just been created (a host where the
        // agent had never run when the stream opened). Re-arm as each appears.
        if claude_watcher.is_none() && claude_dir.exists() {
            claude_watcher = spawn_watcher(&claude_dir, tx.clone());
        }
        if let Some(dir) = &rmux_dir
            && rmux_watcher.is_none()
            && dir.exists()
        {
            rmux_watcher = spawn_watcher(dir, tx.clone());
        }

        // Shorter backstop until *both* live directories are watched — an
        // unwatched one is being carried by the re-scan alone.
        let all_watched = claude_watcher.is_some() && (rmux_dir.is_none() || rmux_watcher.is_some());
        let backstop = if all_watched { BACKSTOP_WATCHED } else { BACKSTOP_UNWATCHED };
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
        scan(&dir, &mut known, &mut out, parse_file, &all_claude, Some("claude")).unwrap();
        let first = lines(&out);
        assert_eq!(first.len(), 1, "one update for the new file: {first:?}");
        assert_eq!(first[0]["sessionId"], "sess-1");
        assert_eq!(first[0]["status"], "busy");
        // Carried through as a number, not dropped — the watermark depends on it.
        assert_eq!(first[0]["updatedAt"], 1_785_474_593_641u64);

        // A second pass with nothing changed must be silent, or the "push" is a
        // poll wearing a different hat.
        let mut out2 = Vec::new();
        scan(&dir, &mut known, &mut out2, parse_file, &all_claude, Some("claude")).unwrap();
        assert!(lines(&out2).is_empty(), "unchanged status must not re-emit");
    }

    #[test]
    fn only_a_status_change_re_emits() {
        let dir = tempdir();
        let pid = std::process::id();
        write_session(&dir, "1.json", "sess-1", pid, "busy");

        let mut known = HashMap::new();
        let mut sink = Vec::new();
        scan(&dir, &mut known, &mut sink, parse_file, &all_claude, Some("claude")).unwrap();

        write_session(&dir, "1.json", "sess-1", pid, "idle");
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, parse_file, &all_claude, Some("claude")).unwrap();
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
        scan(&dir, &mut known, &mut sink, parse_file, &all_claude, Some("claude")).unwrap();

        std::fs::remove_file(dir.join("1.json")).unwrap();
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, parse_file, &all_claude, Some("claude")).unwrap();
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
        scan(&dir, &mut known, &mut out, parse_file, &pid_is_claude, Some("claude")).unwrap();
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
        scan(&dir, &mut known, &mut out, parse_file, &|_| false, Some("claude")).unwrap();
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
        scan(&dir, &mut known, &mut out, parse_file, &all_claude, Some("claude")).unwrap();
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

    // --- the agent-agnostic `~/.rmux/status` source ---------------------------

    fn write_rmux(dir: &Path, file: &str, agent: &str, session_id: &str, pid: u32, status: &str) {
        let body = serde_json::json!({
            "agent": agent,
            "sessionId": session_id,
            "cwd": "/srv/pi-app",
            "pid": pid,
            "status": status,
            "statusUpdatedAt": 1_785_474_593_641u64,
        });
        std::fs::write(dir.join(file), serde_json::to_vec(&body).unwrap()).unwrap();
    }

    #[test]
    fn an_rmux_status_file_is_parsed_and_scanned_like_a_claude_one() {
        // The whole point: an agent that is not Claude gets a real status instead
        // of `Unknown`. Same diff machinery, a different schema and liveness.
        let dir = tempdir();
        write_rmux(&dir, "pi-1.json", "pi", "sess-pi", std::process::id(), "busy");

        let mut known = HashMap::new();
        let mut out = Vec::new();
        scan(&dir, &mut known, &mut out, parse_rmux_file, &all_claude, None).unwrap();
        let got = lines(&out);
        assert_eq!(got.len(), 1, "one update for the pi file: {got:?}");
        assert_eq!(got[0]["sessionId"], "sess-pi");
        assert_eq!(got[0]["status"], "busy");
        // The agent names itself — `default_agent` was None, so the file's own
        // `agent` is what surfaces. This is what lets the rail label a pi dot.
        assert_eq!(got[0]["agent"], "pi");

        // Unchanged → silent, exactly like the Claude source.
        let mut out2 = Vec::new();
        scan(&dir, &mut known, &mut out2, parse_rmux_file, &all_claude, None).unwrap();
        assert!(lines(&out2).is_empty(), "unchanged pi status must not re-emit");
    }

    #[test]
    fn an_rmux_file_without_a_session_id_falls_back_to_the_filename() {
        // A half-written file with no body id still keys onto its filename rather
        // than vanishing — the client at least has a stable handle.
        let dir = tempdir();
        std::fs::write(dir.join("codex-7.json"), br#"{"agent":"codex","status":"idle"}"#).unwrap();
        let parsed = parse_rmux_file(&dir.join("codex-7.json")).unwrap();
        assert_eq!(parsed.session_id, "codex-7");
        assert_eq!(parsed.agent.as_deref(), Some("codex"));
    }

    #[test]
    fn a_dead_pid_in_an_rmux_file_reads_gone() {
        // Liveness for these files is plain `pid_alive` — any agent, so "is it
        // still claude?" is the wrong question. A dead pid is still gone.
        let dir = tempdir();
        write_rmux(&dir, "pi-1.json", "pi", "sess-pi", 0x7FFF_FFF0, "busy");
        let parsed = parse_rmux_file(&dir.join("pi-1.json")).unwrap();
        assert_eq!(effective_status(&parsed, &pid_alive), "gone");
    }

    #[test]
    fn write_status_round_trips_through_parse_rmux_file() {
        // The write side and the read side share one schema; prove they agree by
        // writing with `write_status` and reading with `parse_rmux_file`, against
        // a real (isolated) HOME.
        let _serial = SERIAL.lock().unwrap();
        let home = tempdir();
        let _guard = EnvGuard::set("HOME", home.to_str().unwrap());

        write_status(&StatusWrite {
            agent: "pi".into(),
            session_id: "sess-abc".into(),
            cwd: Some("/srv/app".into()),
            pid: Some(std::process::id()),
            status: "waiting".into(),
        })
        .unwrap();

        let path = home.join(".rmux").join("status").join("sess-abc.json");
        let parsed = parse_rmux_file(&path).unwrap();
        assert_eq!(parsed.session_id, "sess-abc");
        assert_eq!(parsed.agent.as_deref(), Some("pi"));
        assert_eq!(parsed.status, "waiting");
        assert_eq!(parsed.cwd.as_deref(), Some("/srv/app"));
        assert!(parsed.updated_at.is_some(), "a real timestamp is stamped");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a status file names a cwd and pid — 0600");
        }
    }

    #[test]
    fn write_status_gone_removes_the_file() {
        // `gone` is an erasure so the stream's vanish path — not a stale value —
        // announces the end exactly once.
        let _serial = SERIAL.lock().unwrap();
        let home = tempdir();
        let _guard = EnvGuard::set("HOME", home.to_str().unwrap());

        let base = StatusWrite {
            agent: "pi".into(),
            session_id: "sess-x".into(),
            cwd: None,
            pid: Some(std::process::id()),
            status: "busy".into(),
        };
        write_status(&base).unwrap();
        let path = home.join(".rmux").join("status").join("sess-x.json");
        assert!(path.exists());

        write_status(&StatusWrite { status: "gone".into(), ..clone_write(&base) }).unwrap();
        assert!(!path.exists(), "gone must remove the file");
        // Removing a file that is already gone is success, not an error.
        write_status(&StatusWrite { status: "gone".into(), ..clone_write(&base) }).unwrap();
    }

    #[test]
    fn status_key_cannot_escape_the_directory() {
        // A `/` or `..` in an id must not write outside the status directory.
        assert_eq!(status_key("../../etc/passwd"), "------etc-passwd");
        assert_eq!(status_key("a/b"), "a-b");
        assert_eq!(status_key("ok_id-1"), "ok_id-1");
        assert_eq!(status_key(""), "-");
    }

    fn clone_write(w: &StatusWrite) -> StatusWrite {
        StatusWrite {
            agent: w.agent.clone(),
            session_id: w.session_id.clone(),
            cwd: w.cwd.clone(),
            pid: w.pid,
            status: w.status.clone(),
        }
    }

    /// HOME is process-global; the tests that repoint it hold this so they do not
    /// race each other (or any future test that reads the home directory).
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set an env var for the duration of a test and restore it after — HOME is
    /// process-global, so these tests hold the `SERIAL` guard above.
    struct EnvGuard {
        key: String,
        previous: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            EnvGuard { key: key.to_owned(), previous }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var(&self.key, v) },
                None => unsafe { std::env::remove_var(&self.key) },
            }
        }
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
