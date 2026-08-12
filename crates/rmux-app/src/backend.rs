//! Backend service layer: connect targets, provision `rmux-agent`, list and
//! kill sessions, stream `watch-status`.
//!
//! All SSH/agent work runs on an embedded multi-thread tokio runtime; calls
//! from gpui code return futures channels so the UI thread never blocks. The
//! session listing and the status-watcher loop are close ports of the old
//! `src-tauri` services — `agent_sessions` reads `/proc` because older daemons
//! are precisely the ones whose sessions are worth adopting, and the watcher
//! reconnects with backoff because the stream closing is not an error (the
//! sessions on the far side are still running).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use futures::channel::{mpsc, oneshot};
use parking_lot::Mutex;
use rmux_agent::provision::{self, DirectorySource, Installed};
use rmux_ssh::{SshTarget, config};
use rmux_transport::{
    CommandSpec, LocalTarget, NoConsoleWindow, ResolvedCommand, Target, TargetId, Tty, shell_quote,
};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};

/// GPUI globals are marker-trait gated; `Backend` lives in the app context for
/// the process lifetime once `main` installs it.
impl gpui::Global for Backend {}

/// The in-flight or settled result of connecting one target. Shared so repeat
/// callers ride the same attempt instead of provisioning twice.
pub type EnsureFuture = oneshot::Receiver<Result<ReadyServer, String>>;

/// A connected, agent-equipped target, ready to run sessions. Cheap to share.
#[derive(Clone)]
pub struct ReadyServer {
    target: ReadyTarget,
    installed: Installed,
}

#[derive(Clone)]
enum ReadyTarget {
    Local(Arc<LocalTarget>),
    Ssh(Arc<SshTarget>),
}

impl ReadyTarget {
    fn as_target(&self) -> &dyn Target {
        match self {
            ReadyTarget::Local(t) => t.as_ref(),
            ReadyTarget::Ssh(t) => t.as_ref(),
        }
    }
}

impl ReadyServer {
    /// The locally-spawnable argv that opens (or re-attaches) `session` in a
    /// PTY. The frontend never speaks the agent's frame protocol — the attach
    /// binary relays raw PTY bytes over ssh.
    pub fn attach_argv(
        &self,
        session: &str,
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<ResolvedCommand> {
        let spec = self.installed.attach_spec(session, cwd, cols, rows);
        self.target.as_target().build_command(&spec)
    }
}

/// One row of `rmux-agent list`, enriched with resource use and location.
/// Port of the old `src-tauri` `AgentSession`.
#[derive(Clone, Debug)]
pub struct AgentSession {
    pub name: String,
    /// A display name set on the host, if one was. Shown instead of `name` —
    /// the person who renamed it may have been at a different computer.
    pub alias: Option<String>,
    pub pid: Option<u32>,
    pub age_seconds: u64,
    /// A client is currently attached — the difference between a session
    /// someone is using and one left behind.
    pub attached: bool,
    pub command: String,
    pub memory: Option<u64>,
    pub cpu: Option<f32>,
    /// Where the session is actually working, read from `/proc/<pid>/cwd`.
    /// Absent on a host without procfs (macOS hosts) — groups under "(other)".
    pub cwd: Option<String>,
}

/// Is this daemon session a Claude conversation rather than a shell? Decided
/// from the command the daemon reports — an adopted session's name was chosen
/// by whoever started it and carries no reliable prefix.
///
/// The command is a **login line**, not a program name: rmux starts Claude as
/// `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 CLAUDE_CODE_DISABLE_MOUSE=1 claude`,
/// and a host commonly holds sessions started by an older rmux with a different
/// prefix. So leading `VAR=value` assignments are skipped and the first real
/// word is compared — by basename, since the launcher may be an absolute path.
/// Only the program word counts, which keeps `git commit -m "claude"` a shell.
pub fn is_claude_session(command: &str) -> bool {
    command
        .split_whitespace()
        .find(|word| !is_env_assignment(word))
        .is_some_and(|program| program.rsplit('/').next() == Some("claude"))
}

/// A leading `VAR=value` word, as a shell reads it: the name before `=` must be
/// a valid identifier, so a bare path or `--flag=x` is not mistaken for one.
fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else { return false };
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// One line of the `watch-status` stream.
#[derive(Clone, Debug)]
pub enum StatusLine {
    /// The agent understands the subcommand; status is now live for this host.
    Ready,
    /// A Claude session changed state.
    Update(StatusUpdate),
    /// The agent is too old for `watch-status`; this host stays static.
    Unsupported,
    /// The stream dropped; the watcher is backing off before reconnecting.
    Offline,
}

/// `{"sessionId", "cwd", "pid", "status", "updatedAt"}` — matches the agent's
/// private `Update` (rmux-agent/src/status.rs); the client defines its own.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusUpdate {
    pub session_id: String,
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    /// `busy | shell | idle | waiting | gone`
    pub status: String,
    pub updated_at: Option<u64>,
}

/// The app global, set in `main`. Owns the tokio runtime and the agent binary
/// source; caches one connected target per `TargetId`.
pub struct Backend {
    rt: tokio::runtime::Runtime,
    binaries: Arc<DirectorySource>,
    servers: Arc<Mutex<HashMap<TargetId, ServerSlot>>>,
    /// One watcher per host; a finished handle means the watcher stopped
    /// (unsupported host) and may be restarted.
    watchers: Mutex<HashMap<TargetId, tokio::task::JoinHandle<()>>>,
}

#[derive(Clone)]
enum ServerSlot {
    /// A connect is in progress. A second caller during this window spawns its
    /// own attempt — `SshTarget::connect` and `provision::ensure` are
    /// idempotent, so racing is just a brief redundancy, and the cache ends up
    /// `Ready` either way.
    Connecting,
    Ready(ReadyServer),
    Failed(String),
}

impl Backend {
    pub fn new() -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("rmux-backend")
            .build()
            .context("build tokio runtime")?;
        Ok(Self {
            rt,
            binaries: Arc::new(binary_source()),
            servers: Arc::new(Mutex::new(HashMap::new())),
            watchers: Mutex::new(HashMap::new()),
        })
    }

    /// A directory of `rmux-agent-<triple>` binaries for development, before a
    /// bundle exists. Takes priority over the default layout.
    pub const AGENT_DIR_ENV: &'static str = "RMUX_AGENT_DIR";

    /// SSH-config hosts, for the connect picker. Sync — parsing
    /// `~/.ssh/config` is a file read, not a round trip.
    pub fn hosts() -> Vec<config::ConfigHost> {
        config::list_hosts()
    }

    /// Connect + provision `id`, caching the result. A `Ready` hit returns
    /// immediately; a `Failed` hit is retried; anything else spawns a fresh
    /// attempt (see `ServerSlot::Connecting`).
    pub fn ensure(&self, id: TargetId) -> EnsureFuture {
        if let Some(ServerSlot::Ready(server)) = self.servers.lock().get(&id).cloned() {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Ok(server));
            return rx;
        }
        self.servers.lock().insert(id.clone(), ServerSlot::Connecting);
        let (tx, rx) = oneshot::channel();
        let binaries = self.binaries.clone();
        let servers = self.servers.clone();
        let id2 = id.clone();
        self.rt.spawn(async move {
            let result = connect_and_provision(id, binaries).await;
            let mut guard = servers.lock();
            match &result {
                Ok(server) => {
                    guard.insert(id2, ServerSlot::Ready(server.clone()));
                }
                Err(e) => {
                    guard.insert(id2, ServerSlot::Failed(e.clone()));
                }
            }
            let _ = tx.send(result);
        });
        rx
    }

    /// Clear a failed slot so the next `ensure` retries.
    pub fn retry(&self, id: &TargetId) {
        let mut guard = self.servers.lock();
        if matches!(guard.get(id), Some(ServerSlot::Failed(_))) {
            guard.remove(id);
        }
    }

    /// Sessions `rmux-agent` is holding on the host, enriched with resource
    /// use and cwd.
    pub fn list(&self, id: &TargetId) -> oneshot::Receiver<Result<Vec<AgentSession>, String>> {
        let (tx, rx) = oneshot::channel();
        let Some(server) = self.ready(id) else {
            let _ = tx.send(Err("server not connected".into()));
            return rx;
        };
        self.rt.spawn(async move {
            let program = server.installed.program.clone();
            let res = agent_sessions(server.target.as_target(), &program)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
        rx
    }

    /// Kill a session on the host (the daemon ends it; the shell dies with it).
    pub fn kill(&self, id: &TargetId, session: &str) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let Some(server) = self.ready(id) else {
            let _ = tx.send(Err("server not connected".into()));
            return rx;
        };
        let session = session.to_owned();
        self.rt.spawn(async move {
            let spec = CommandSpec::new(&server.installed.program)
                .arg("kill")
                .arg(&session)
                .tty(Tty::None);
            let res = server
                .target
                .as_target()
                .exec(&spec)
                .await
                .and_then(|out| out.stdout_or_err().map(|_| ()))
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
        rx
    }

    /// Stream `watch-status` updates for a host into `sink`. Idempotent — one
    /// watcher per host, reconnecting with backoff; the receiver just sees
    /// `Offline` between attempts. No-op until the server is connected (the UI
    /// calls it once a rail server's `ensure` resolves).
    pub fn watch_status(&self, id: &TargetId, sink: mpsc::UnboundedSender<StatusLine>) {
        {
            let guard = self.watchers.lock();
            if guard.get(id).map(|h| !h.is_finished()).unwrap_or(false) {
                return;
            }
        }
        let Some(server) = self.ready(id) else { return };
        let label = id.label();
        let handle = self.rt.spawn(async move {
            run_watcher(server, sink, &label).await;
        });
        self.watchers.lock().insert(id.clone(), handle);
    }

    fn ready(&self, id: &TargetId) -> Option<ReadyServer> {
        match self.servers.lock().get(id) {
            Some(ServerSlot::Ready(server)) => Some(server.clone()),
            _ => None,
        }
    }
}

async fn connect_and_provision(
    id: TargetId,
    binaries: Arc<DirectorySource>,
) -> Result<ReadyServer, String> {
    match &id {
        TargetId::Local => {
            let target = LocalTarget::new();
            let installed =
                provision::ensure(&target, binaries.as_ref()).await.map_err(|e| e.to_string())?;
            Ok(ReadyServer { target: ReadyTarget::Local(Arc::new(target)), installed })
        }
        TargetId::Ssh(host) => {
            let target = SshTarget::new(host.clone());
            target.connect().await.map_err(|e| format!("connect {}: {e}", host.label()))?;
            let installed = provision::ensure(&target, binaries.as_ref())
                .await
                .map_err(|e| format!("provision agent: {e}"))?;
            Ok(ReadyServer { target: ReadyTarget::Ssh(Arc::new(target)), installed })
        }
    }
}

/// Locate the prebuilt agent binaries. In a bundle both sit next to the
/// executable; a development build points `RMUX_AGENT_DIR` at a directory of
/// `rmux-agent-<triple>` binaries, or uses `target/agents` where
/// `scripts/build-agents.sh` writes them.
fn binary_source() -> DirectorySource {
    if let Ok(dir) = std::env::var(Backend::AGENT_DIR_ENV) {
        if !dir.is_empty() {
            let dir = PathBuf::from(&dir);
            let local = dir.join("rmux-agent");
            return DirectorySource { local: local.exists().then_some(local), dir };
        }
    }
    let exe_dir =
        std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf()));
    let mut source = provision::default_source(None, exe_dir.as_deref());
    let target_agents = Path::new("target/agents");
    if target_agents.exists() {
        let local = target_agents.join("rmux-agent");
        if local.exists() {
            source.local = Some(local);
        }
        source.dir = target_agents.to_path_buf();
    }
    source
}

// ── Session listing (port of the old src-tauri `agent_sessions`) ────────────

/// Sessions `rmux-agent` is holding on the host, with what they are consuming.
///
/// The list comes from the daemon rather than from `ps`, because that is the
/// only thing that knows a session's *name* — from the outside a Claude
/// session is a login shell, indistinguishable from any other. `ps` is then
/// asked for the resource figures in the same round trip. The working
/// directory is read from `/proc/<pid>/cwd` rather than asked of the daemon,
/// deliberately: a host routinely runs several agent builds at once (an
/// upgrade leaves the old daemon serving its live sessions), so anything the
/// *protocol* reports is unavailable for exactly the older sessions most worth
/// adopting. `readlink` on another user's process fails silently — their
/// directory is not ours to report.
async fn agent_sessions(target: &dyn Target, program: &str) -> anyhow::Result<Vec<AgentSession>> {
    let line = format!(
        "{} list 2>/dev/null || true; echo __PS__; ps -eo pid=,rss=,pcpu= 2>/dev/null || true; \
         echo __CWD__; for d in /proc/[0-9]*; do \
         printf '%s\\t%s\\n' \"${{d##*/}}\" \"$(readlink \"$d/cwd\" 2>/dev/null)\"; \
         done 2>/dev/null || true",
        shell_quote(program)
    );
    let out = target.exec(&CommandSpec::login_shell().arg("-c").arg(line)).await?;
    let (listing, rest) = out.stdout.split_once("__PS__").unwrap_or((out.stdout.as_str(), ""));
    let (ps, cwds) = rest.split_once("__CWD__").unwrap_or((rest, ""));

    let mut where_ = HashMap::new();
    for row in cwds.lines() {
        if let Some((pid, dir)) = row.split_once('\t')
            && let Ok(pid) = pid.trim().parse::<u32>()
        {
            let dir = dir.trim();
            if !dir.is_empty() {
                where_.insert(pid, dir.to_owned());
            }
        }
    }

    let mut usage = HashMap::new();
    for row in ps.lines() {
        let mut f = row.split_whitespace();
        if let (Some(pid), Some(rss), Some(cpu)) = (f.next(), f.next(), f.next())
            && let (Ok(pid), Ok(rss)) = (pid.parse::<u32>(), rss.parse::<u64>())
        {
            // `ps` reports RSS in kilobytes; the UI gets bytes like every other
            // figure here, so one place decides the unit.
            usage.insert(pid, (rss * 1024, cpu.parse::<f32>().unwrap_or(0.0)));
        }
    }

    let mut sessions = Vec::new();
    for row in listing.lines().filter(|l| !l.trim().is_empty()) {
        let Some(parsed) = parse_agent_row(row) else { continue };
        let (memory, cpu) = parsed.pid.and_then(|p| usage.get(&p)).copied().unzip();
        let cwd = parsed.pid.and_then(|p| where_.get(&p)).cloned();
        sessions.push(AgentSession { memory, cpu, cwd, ..parsed });
    }
    Ok(sessions)
}

/// One row of `rmux-agent list`, tolerant of both column counts.
///
/// The agent gained an alias column, and the host may be running **either**
/// build: a rebuilt client talks to whichever daemon still owns a live
/// session, and old daemons deliberately keep serving until their sessions
/// end. The two are told apart by whether the second field parses as a number:
/// an alias never does, because a numeric one would be refused as ambiguous
/// with a pid at exactly this point.
fn parse_agent_row(row: &str) -> Option<AgentSession> {
    let f: Vec<&str> = row.split('\t').collect();
    let name = f.first()?.trim();
    if name.is_empty() {
        return None;
    }

    // `-` is the agent's "absent" marker in every column.
    let dash = |v: &str| {
        let v = v.trim();
        (!v.is_empty() && v != "-").then(|| v.to_owned())
    };

    // Six columns: name, alias, pid, age, attached, command.
    // Five columns: name, pid, age, attached, command.
    let six = f.len() >= 6 && f.get(1).is_some_and(|v| v.trim().parse::<u32>().is_err());
    let (alias, rest) = if six { (dash(f[1]), &f[2..]) } else { (None, &f[1..]) };

    Some(AgentSession {
        name: name.to_owned(),
        alias,
        pid: rest.first().and_then(|v| v.trim().parse::<u32>().ok()),
        age_seconds: rest.get(1).and_then(|v| v.trim().parse().ok()).unwrap_or(0),
        attached: rest.get(2).is_some_and(|v| v.trim() == "attached"),
        command: rest.get(3).unwrap_or(&"").trim().to_owned(),
        memory: None,
        cpu: None,
        cwd: None,
    })
}

// ── watch-status (port of the old src-tauri watcher client) ─────────────────

const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Reconnecting read loop for one host. The stream closing (ssh dropped, the
/// laptop slept) is not an error — reconnect after a backoff, because the
/// sessions on the far side are still running.
async fn run_watcher(server: ReadyServer, mut sink: mpsc::UnboundedSender<StatusLine>, label: &str) {
    let mut backoff = BACKOFF_START;
    loop {
        match watch_once(&server, &mut sink).await {
            // The agent is too old for the subcommand; tell the UI once and
            // stop — the fingerprinted agent path means an upgrade reinstalls
            // before this would ever succeed.
            Outcome::Unsupported => {
                let _ = sink.unbounded_send(StatusLine::Unsupported);
                return;
            }
            Outcome::Ended => backoff = BACKOFF_START,
            Outcome::FailedToStart => {}
        }
        let _ = sink.unbounded_send(StatusLine::Offline);
        log::info!(target: "rmux", "watch-status on {label}: reconnecting in {backoff:?}");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

enum Outcome {
    /// The stream started (`ready` seen) and later closed.
    Ended,
    /// The agent does not understand the subcommand.
    Unsupported,
    /// Could not spawn or connect at all.
    FailedToStart,
}

async fn watch_once(server: &ReadyServer, sink: &mut mpsc::UnboundedSender<StatusLine>) -> Outcome {
    let spec = CommandSpec::new(&server.installed.program).arg("watch-status").tty(Tty::None);
    let Ok(resolved) = server.target.as_target().build_command(&spec) else {
        return Outcome::FailedToStart;
    };

    let mut cmd = tokio::process::Command::new(&resolved.program);
    cmd.args(&resolved.args);
    for (key, value) in &resolved.env {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.no_console_window();
    // Aborting this task (app teardown) must take the ssh child with it rather
    // than leaking a connection.
    cmd.kill_on_drop(true);

    let Ok(mut child) = cmd.spawn() else {
        return Outcome::FailedToStart;
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Outcome::FailedToStart;
    };

    // Watch stderr for the old-agent usage error; any other stderr (an ssh
    // notice) falls through as a plain reconnect.
    let unsupported = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = unsupported.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("usage:") || line.contains("unknown option") {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });

    let mut saw_ready = false;
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if value.get("ready").and_then(serde_json::Value::as_bool) == Some(true) {
            saw_ready = true;
            let _ = sink.unbounded_send(StatusLine::Ready);
            continue;
        }
        if value.get("unsupported").and_then(serde_json::Value::as_bool) == Some(true) {
            return Outcome::Unsupported;
        }
        if let Ok(update) = serde_json::from_str::<StatusUpdate>(&line) {
            let _ = sink.unbounded_send(StatusLine::Update(update));
        }
    }

    if unsupported.load(std::sync::atomic::Ordering::Relaxed) && !saw_ready {
        Outcome::Unsupported
    } else {
        Outcome::Ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real rows from `rmux-agent list` on a live host (six columns, `-` alias).
    /// The command is the whole login line, env prefix included — the shape that
    /// made a `starts_with("claude ")` test call every session a shell.
    const ROWS: &str = "\
claude-session-msh0egew-2\t-\t3697\t532582\tdetached\tCLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 CLAUDE_CODE_DISABLE_MOUSE=1 claude
claude-session-msk9lhcl-1\t-\t2629221\t350515\tdetached\tCLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 CLAUDE_CODE_DISABLE_MOUSE=1 claude --resume a3bb3aa8
term-msmmqx5q-2\t-\t3967760\t207496\tdetached\t";

    #[test]
    fn parses_six_column_rows() {
        let rows: Vec<_> = ROWS.lines().map(|r| parse_agent_row(r).unwrap()).collect();
        assert_eq!(rows[0].name, "claude-session-msh0egew-2");
        assert_eq!(rows[0].alias, None);
        assert_eq!(rows[0].pid, Some(3697));
        assert_eq!(rows[0].age_seconds, 532582);
        assert!(!rows[0].attached);
        assert_eq!(rows[2].name, "term-msmmqx5q-2");
        assert_eq!(rows[2].command, "");
    }

    /// The five-column form an older daemon still serving live sessions emits.
    #[test]
    fn parses_five_column_rows_and_aliases() {
        let old = parse_agent_row("term-1\t42\t10\tattached\tbash").unwrap();
        assert_eq!(old.alias, None);
        assert_eq!(old.pid, Some(42));
        assert!(old.attached);
        assert_eq!(old.command, "bash");

        let named = parse_agent_row("term-1\tdeploy box\t42\t10\tdetached\tbash").unwrap();
        assert_eq!(named.alias.as_deref(), Some("deploy box"));
        assert_eq!(named.pid, Some(42));
    }

    #[test]
    fn classifies_claude_behind_its_env_prefix() {
        for row in ROWS.lines().take(2) {
            assert!(is_claude_session(&parse_agent_row(row).unwrap().command));
        }
        assert!(is_claude_session("claude"));
        assert!(is_claude_session("claude --resume abc"));
        assert!(is_claude_session("/usr/local/bin/claude --resume abc"));
    }

    #[test]
    fn shells_are_not_claude() {
        let shell = parse_agent_row(ROWS.lines().nth(2).unwrap()).unwrap();
        assert!(!is_claude_session(&shell.command));
        // The program word is what counts — an argument that says "claude" does not.
        assert!(!is_claude_session("git commit -m claude"));
        assert!(!is_claude_session("bash -lc claude"));
        assert!(!is_claude_session("claude-code-wrapper"));
        // `--flag=x` is an argument, not a `VAR=value` assignment to skip past.
        assert!(!is_claude_session("myprog --opt=1 claude"));
    }
}
