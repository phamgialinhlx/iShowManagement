//! `rmux <command>` — the CLI an agent inside one rmux session uses to start and
//! coordinate others, herdr-style.
//!
//! ```text
//! rmux whoami
//! rmux list --json
//! rmux spawn --agent claude|pi|codex|shell [--cwd DIR] [--name LABEL] [--prompt TEXT|-]
//!            [--skip-permissions] [--timeout S]
//! rmux prompt SESSION TEXT|- [--wait [--until done|blocked|idle]] [--timeout S]
//! rmux wait SESSION [--until working|blocked|done|idle] [--timeout S]
//! rmux read SESSION [--lines N] [--source screen|transcript] [--json]
//! rmux wait-output SESSION (--regex R | --match TEXT) [--timeout S]
//! rmux keys SESSION KEY...
//! rmux interrupt SESSION
//! rmux close SESSION [--force]
//! rmux children
//! ```
//!
//! `rmux` is a shim for `rmux-agent` (see [`crate::session_env`]); every command
//! here is also `rmux-agent <command>`.
//!
//! ## Output contract
//!
//! Success is one JSON value on stdout (except `read`, which prints text unless
//! `--json`). Failure is `{"error":{"code","message"}}` on stderr with exit 1; a
//! malformed command line is code `usage`, exit 2. Codes are stable and named in
//! [`code`].
//!
//! ## What this is not
//!
//! It is **local**: it talks to the daemon over the same `0700`/`0600` Unix
//! socket the attach client uses, as the same uid, and opens no port. It is not
//! the Redstone bridge and adds nothing to the bridge's closed verb set — the
//! bridge's no-`exec` rule exists because the bridge is reachable from *off*
//! this machine. Typing into a shell here (`prompt`, `keys`) grants a caller
//! nothing it lacks: it is already a process on this host running as this user.
//!
//! ## One implementation of each thing
//!
//! Status comes from [`bridge::joined_sessions`] (the bridge's own join),
//! submitting a message from [`bridge::send_message`], launch lines from
//! [`bridge::launch_line_for`], keys from `rmux_claude_core::keys`. A second copy
//! of "how do you submit a message to Claude" has already cost one bug.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use rmux_bridge::protocol::{Agent, Role};
use rmux_claude_core::keys;
use serde_json::{Value, json};

use crate::protocol::SessionSummary;
use crate::status::Snapshot;
use crate::{alias, attach, bridge, children, status};

/// Stable error codes. Callers (and the skill) branch on these, so they are
/// part of the interface and never renamed.
pub mod code {
    pub const USAGE: &str = "usage";
    pub const NOT_FOUND: &str = "not_found";
    pub const AGENT_BLOCKED: &str = "agent_blocked";
    pub const AGENT_NOT_READY: &str = "agent_not_ready";
    pub const AGENT_PROMPT_STALLED: &str = "agent_prompt_stalled";
    pub const SPAWN_FAILED: &str = "spawn_failed";
    pub const TIMEOUT: &str = "timeout";
    pub const NOT_OWNER: &str = "not_owner";
    pub const PERMISSION_DENIED: &str = "permission_denied";
    pub const UNSUPPORTED: &str = "unsupported";
    pub const CLOSE_FAILED: &str = "close_failed";
    pub const FAILED: &str = "failed";
}

/// Startup budget for `spawn` (herdr's default).
const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);
/// Default ceiling for `wait`, `prompt --wait` and `wait-output`. Finite on
/// purpose: an agent's tool call that blocks forever is a hung agent. `0` asks
/// for no limit.
const WAIT_TIMEOUT: Duration = Duration::from_secs(600);
/// How long `prompt --wait` waits for *any* sign the prompt was taken up.
const STALL_WINDOW: Duration = Duration::from_secs(5);
/// Status poll interval. The status file and the daemon list are both local
/// reads, so this costs nothing worth saving.
const POLL: Duration = Duration::from_millis(200);
/// A spawn prompt above this goes through the composer, not argv: argv is
/// visible in `ps` to every account and `ARG_MAX` caps one argument at 128 KiB.
const MAX_ARGV_PROMPT: usize = 16 * 1024;
/// A shell is "at its prompt" once output has been quiet this long.
const SHELL_SETTLE: Duration = Duration::from_millis(400);
/// pi without its status extension has nothing to report readiness with; a
/// quiet screen this long is the fallback.
const PI_SETTLE: Duration = Duration::from_secs(2);
const SPAWN_COLS: u16 = 120;
const SPAWN_ROWS: u16 = 40;

// ---------------------------------------------------------------------------
// errors and output

#[derive(Debug)]
pub struct CliError {
    pub code: &'static str,
    pub message: String,
    /// Extra fields merged into the error object (`session`, `status`).
    pub extra: BTreeMap<&'static str, Value>,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), extra: BTreeMap::new() }
    }
    fn usage(message: impl Into<String>) -> Self {
        Self::new(code::USAGE, message)
    }
    fn with(mut self, key: &'static str, value: impl Into<Value>) -> Self {
        self.extra.insert(key, value.into());
        self
    }
    fn exit(&self) -> u8 {
        if self.code == code::USAGE { 2 } else { 1 }
    }
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("code".into(), self.code.into());
        obj.insert("message".into(), self.message.clone().into());
        for (k, v) in &self.extra {
            obj.insert((*k).into(), v.clone());
        }
        json!({ "error": obj })
    }
}

impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(code::FAILED, e.to_string())
    }
}

type CliResult = Result<Output, CliError>;

pub enum Output {
    Json(Value),
    Text(String),
}

/// The commands this module answers. `list` only with `--json`: plain `list`
/// is the tab-separated format the desktop app parses and must not change.
pub fn handles(command: &str, args: &[String]) -> bool {
    match command {
        "whoami" | "spawn" | "prompt" | "wait" | "read" | "wait-output" | "keys"
        | "interrupt" | "close" | "children" => true,
        "list" => args.iter().any(|a| a == "--json"),
        _ => false,
    }
}

/// Run one command and turn its result into output and an exit code.
pub async fn run(command: &str, args: &[String]) -> ExitCode {
    let result = match command {
        "whoami" => whoami(args),
        "list" => list(args).await,
        "spawn" => spawn(args).await,
        "prompt" => prompt(args).await,
        "wait" => wait(args).await,
        "read" => read(args).await,
        "wait-output" => wait_output(args).await,
        "keys" => send_keys(args).await,
        "interrupt" => interrupt(args).await,
        "close" => close(args).await,
        "children" => children_cmd(args).await,
        other => Err(CliError::usage(format!("unknown command {other}"))),
    };
    match result {
        Ok(Output::Json(v)) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Ok(Output::Text(t)) => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(t.as_bytes());
            if !t.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", e.to_json());
            ExitCode::from(e.exit())
        }
    }
}

// ---------------------------------------------------------------------------
// argument parsing

struct Args {
    positional: Vec<String>,
    values: BTreeMap<String, String>,
    switches: Vec<String>,
}

impl Args {
    /// `--flag value`, `--flag=value`, bare switches, and `--` to end flags (so
    /// a prompt may start with a dash). A lone `-` is a positional (stdin).
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Self, CliError> {
        let mut out = Args { positional: Vec::new(), values: BTreeMap::new(), switches: Vec::new() };
        let mut i = 0;
        let mut flags_done = false;
        while i < args.len() {
            let arg = &args[i];
            if flags_done || arg == "-" || !arg.starts_with("--") {
                out.positional.push(arg.clone());
            } else if arg == "--" {
                flags_done = true;
            } else {
                let (name, inline) = match arg.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_owned())),
                    None => (arg.as_str(), None),
                };
                if with_value.contains(&name) {
                    let value = match inline {
                        Some(v) => v,
                        None => {
                            i += 1;
                            args.get(i).cloned().ok_or_else(|| {
                                CliError::usage(format!("{name} needs a value"))
                            })?
                        }
                    };
                    out.values.insert(name.to_owned(), value);
                } else if switches.contains(&name) && inline.is_none() {
                    out.switches.push(name.to_owned());
                } else {
                    return Err(CliError::usage(format!("unknown option {arg}")));
                }
            }
            i += 1;
        }
        Ok(out)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }
    fn session(&self) -> Result<String, CliError> {
        let raw = self.positional.first().ok_or_else(|| CliError::usage("a SESSION is required"))?;
        // An alias is a name the operator gave; the daemon only knows keys.
        Ok(alias::resolve(raw))
    }
    fn timeout(&self, default: Duration) -> Result<Option<Duration>, CliError> {
        let Some(raw) = self.value("--timeout") else { return Ok(Some(default)) };
        let secs: f64 = raw
            .parse()
            .ok()
            .filter(|s: &f64| s.is_finite() && *s >= 0.0)
            .ok_or_else(|| CliError::usage(format!("--timeout takes seconds, not {raw:?}")))?;
        Ok(if secs == 0.0 { None } else { Some(Duration::from_secs_f64(secs)) })
    }
    fn lines(&self, default: usize) -> Result<usize, CliError> {
        match self.value("--lines") {
            None => Ok(default),
            Some(raw) => raw
                .parse()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| CliError::usage(format!("--lines takes a positive number, not {raw:?}"))),
        }
    }
}

/// Text from an argument, or from stdin when the argument is `-`.
fn text_arg(raw: &str) -> Result<String, CliError> {
    if raw != "-" {
        return Ok(raw.to_owned());
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| CliError::new(code::FAILED, format!("could not read stdin: {e}")))?;
    Ok(text)
}

// ---------------------------------------------------------------------------
// observing sessions

/// The herdr status vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Working,
    Blocked,
    Idle,
    Unknown,
}

impl State {
    /// Claude's `busy|shell|idle|waiting` (and the hook's subset) → herdr's.
    /// `shell` is Claude running a tool command, which is work; `waiting` is a
    /// dialog on screen, which is blocked. Anything unrecognised is `unknown` —
    /// mapping it onto idle would report a working session as finished.
    pub fn from_raw(raw: &str) -> Self {
        match raw {
            "busy" | "shell" => State::Working,
            "waiting" => State::Blocked,
            "idle" => State::Idle,
            _ => State::Unknown,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Blocked => "blocked",
            State::Idle => "idle",
            State::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Claude,
    Pi,
    Codex,
    Terminal,
}

impl Kind {
    /// From the daemon name — the same prefix rule the bridge trusts.
    pub fn of(session: &str) -> Self {
        if session.starts_with("claude-") {
            Kind::Claude
        } else if session.starts_with("pi-") {
            Kind::Pi
        } else if session.starts_with("codex-") {
            Kind::Codex
        } else {
            Kind::Terminal
        }
    }
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Pi => "pi",
            Kind::Codex => "codex",
            Kind::Terminal => "terminal",
        }
    }
    fn is_agent(self) -> bool {
        self != Kind::Terminal
    }
}

struct Observed {
    summary: SessionSummary,
    status: Option<Snapshot>,
}

impl Observed {
    fn kind(&self) -> Kind {
        Kind::of(&self.summary.name)
    }
    fn state(&self) -> State {
        self.status.as_ref().map(|s| State::from_raw(&s.status)).unwrap_or(State::Unknown)
    }
}

async fn observe_all() -> Result<Vec<Observed>, CliError> {
    Ok(bridge::joined_sessions()
        .await?
        .into_iter()
        .map(|(summary, status)| Observed { summary, status })
        .collect())
}

async fn observe(session: &str) -> Result<Option<Observed>, CliError> {
    Ok(observe_all().await?.into_iter().find(|o| o.summary.name == session))
}

async fn observe_or_missing(session: &str) -> Result<Observed, CliError> {
    observe(session).await?.ok_or_else(|| not_found(session))
}

fn not_found(session: &str) -> CliError {
    CliError::new(code::NOT_FOUND, format!("no running session called {session} on this host"))
        .with("session", session)
}

async fn live_names() -> Result<Vec<String>, CliError> {
    Ok(attach::list().await?.into_iter().map(|s| s.name).collect())
}

fn caller() -> Option<String> {
    std::env::var("RMUX_SESSION").ok().filter(|s| !s.is_empty())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The caller's working directory as *they* spell it: `$PWD` when it names the
/// same directory as `getcwd` (so a symlinked path such as macOS's `/tmp` stays
/// the path the agent and the operator see), else `getcwd`.
pub(crate) fn caller_cwd() -> Result<String, CliError> {
    let real = std::env::current_dir()
        .map_err(|e| CliError::new(code::FAILED, format!("no current directory: {e}")))?;
    if let Ok(pwd) = std::env::var("PWD")
        && std::path::Path::new(&pwd).is_absolute()
        && std::fs::canonicalize(&pwd).ok() == std::fs::canonicalize(&real).ok()
    {
        return Ok(pwd);
    }
    Ok(real.to_string_lossy().into_owned())
}

/// Best-effort working directory of a pid: Linux only, where the agent runs.
fn proc_cwd(pid: Option<u32>) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let pid = pid?;
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?.to_str().map(str::to_owned)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

// ---------------------------------------------------------------------------
// screen text

/// A terminal's raw scrollback as readable lines.
///
/// Carriage-return overwrites are collapsed first — within a line only what
/// follows the last bare `\r` survives, which is what a terminal shows for a
/// spinner or progress bar — then escape sequences are stripped by the bridge's
/// own [`bridge::readable`]. Best-effort: a TUI that redraws with cursor motion
/// still reads as successive frames. For a Claude/pi conversation,
/// `--source transcript` is the faithful view.
pub(crate) fn screen_text(bytes: &[u8]) -> String {
    let mut collapsed = Vec::with_capacity(bytes.len());
    for (i, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if i > 0 {
            collapsed.push(b'\n');
        }
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let visible = match line.iter().rposition(|b| *b == b'\r') {
            Some(p) => &line[p + 1..],
            None => line,
        };
        collapsed.extend_from_slice(visible);
    }
    bridge::readable(&collapsed)
}

/// The last `n` non-trailing-blank lines of `text`.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let end = lines.iter().rposition(|l| !l.is_empty()).map(|p| p + 1).unwrap_or(0);
    let start = end.saturating_sub(n);
    lines[start..end].join("\n")
}

async fn screen_of(session: &str) -> Result<String, CliError> {
    match attach::read(session).await? {
        Some(bytes) => Ok(screen_text(&bytes)),
        None => Err(not_found(session)),
    }
}

// ---------------------------------------------------------------------------
// last-prompt bookkeeping, so `wait --until done` after a bare `prompt` works

fn prompted_path() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".rmux").join("prompted.json"))
}

fn last_prompt_at(session: &str) -> Option<u64> {
    let text = std::fs::read_to_string(prompted_path()?).ok()?;
    let map: BTreeMap<String, u64> = serde_json::from_str(&text).ok()?;
    map.get(session).copied()
}

/// Remember when `session` was last prompted. Pruned against `live` on every
/// write, `0600`, temp-then-rename — the `aliases.json` discipline.
fn record_prompt(session: &str, at: u64, live: &[String]) {
    let Some(path) = prompted_path() else { return };
    let mut map: BTreeMap<String, u64> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    map.retain(|k, _| live.iter().any(|l| l == k));
    map.insert(session.to_owned(), at);
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let temp = dir.join(format!("prompted.json.{}.tmp", std::process::id()));
    let Ok(text) = serde_json::to_string(&map) else { return };
    if std::fs::write(&temp, text).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }
    let _ = std::fs::rename(&temp, &path);
}

// ---------------------------------------------------------------------------
// waiting

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Until {
    Working,
    Blocked,
    Idle,
    Done,
}

impl Until {
    fn parse(raw: &str) -> Result<Self, CliError> {
        match raw {
            "working" => Ok(Until::Working),
            "blocked" => Ok(Until::Blocked),
            "idle" => Ok(Until::Idle),
            "done" => Ok(Until::Done),
            other => Err(CliError::usage(format!(
                "--until takes working, blocked, idle or done, not {other:?}"
            ))),
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Until::Working => "working",
            Until::Blocked => "blocked",
            Until::Idle => "idle",
            Until::Done => "done",
        }
    }
}

/// One step of the wait: has `until` been reached, given what was seen?
///
/// Pure so the `done` rule is tested without a Claude. **`done` is idle after
/// work**: either `working` was observed during this wait, or the status file
/// changed (`updated_at`) at or after `since` — the moment of the last prompt,
/// or the start of the call. Idle with an older `updated_at` is the agent that
/// has not yet picked the prompt up, and is not done.
fn reached(until: Until, state: State, updated_at: Option<u64>, since: u64, seen_working: bool) -> bool {
    match until {
        Until::Working => state == State::Working,
        Until::Blocked => state == State::Blocked,
        Until::Idle => state == State::Idle,
        Until::Done => {
            state == State::Idle && (seen_working || updated_at.is_some_and(|u| u >= since))
        }
    }
}

/// Poll until `until` holds. `stall` (for `prompt --wait`) is the window in
/// which some activity — working, blocked, or a status change since `since` —
/// must be seen, else `agent_prompt_stalled`.
async fn wait_until(
    session: &str,
    until: Until,
    since: u64,
    timeout: Option<Duration>,
    stall: Option<Duration>,
) -> Result<State, CliError> {
    let started = Instant::now();
    let mut seen_working = false;
    let mut last = State::Unknown;
    loop {
        let Some(obs) = observe(session).await? else {
            return Err(CliError::new(code::NOT_FOUND, format!("{session} ended while waiting"))
                .with("session", session)
                .with("status", last.as_str()));
        };
        let state = obs.state();
        let updated_at = obs.status.as_ref().and_then(|s| s.updated_at);
        last = state;
        if state == State::Working {
            seen_working = true;
        }

        if reached(until, state, updated_at, since, seen_working) {
            return Ok(state);
        }
        // Waiting for `done` while a dialog is up would wait for a human; say
        // so now rather than at the timeout.
        if until == Until::Done && state == State::Blocked {
            return Err(CliError::new(
                code::AGENT_BLOCKED,
                format!("{session} is waiting on a dialog; read it and ask the operator"),
            )
            .with("session", session)
            .with("status", state.as_str()));
        }
        if let Some(window) = stall {
            let active = seen_working
                || state == State::Blocked
                || updated_at.is_some_and(|u| u >= since);
            if !active && started.elapsed() >= window {
                return Err(CliError::new(
                    code::AGENT_PROMPT_STALLED,
                    format!(
                        "{session} showed no activity within {}s of the prompt; read it before sending again",
                        window.as_secs()
                    ),
                )
                .with("session", session)
                .with("status", state.as_str()));
            }
        }
        if timeout.is_some_and(|t| started.elapsed() >= t) {
            return Err(CliError::new(
                code::TIMEOUT,
                format!("{session} did not reach {} in time", until.as_str()),
            )
            .with("session", session)
            .with("status", state.as_str()));
        }
        tokio::time::sleep(POLL).await;
    }
}

// ---------------------------------------------------------------------------
// commands

fn whoami(args: &[String]) -> CliResult {
    Args::parse(args, &[], &["--json"])?;
    let cwd = caller_cwd().ok();
    Ok(Output::Json(json!({
        "session": caller(),
        "host": bridge::hostname(),
        "cwd": cwd,
    })))
}

async fn list(args: &[String]) -> CliResult {
    Args::parse(args, &[], &["--json"])?;
    Ok(Output::Json(list_value().await?))
}

/// `list --json`'s value: also what the tmux-style `rmux ls` renders for this
/// machine, so the two cannot disagree about a session's status or folder.
pub(crate) async fn list_value() -> Result<Value, CliError> {
    let aliases = alias::load();
    let all = observe_all().await?;
    let live: Vec<String> = all.iter().map(|o| o.summary.name.clone()).collect();
    let tree = children::read_pruned(&live)?;

    let sessions: Vec<Value> = all
        .iter()
        .map(|o| {
            let name = &o.summary.name;
            let child = tree.children.iter().find(|c| &c.session == name);
            let cwd = o
                .status
                .as_ref()
                .and_then(|s| s.cwd.clone())
                .or_else(|| child.map(|c| c.cwd.clone()))
                .or_else(|| proc_cwd(o.summary.pid));
            let kind = o.kind();
            json!({
                "session": name,
                "alias": aliases.get(name),
                "kind": kind.as_str(),
                "status": o.state().as_str(),
                "cwd": cwd,
                "parent": child.and_then(|c| c.parent.clone()),
                "attached": o.summary.attached,
                "pid": o.summary.pid,
                "ageSeconds": o.summary.age_seconds,
                "conversation": o.status.as_ref().map(|s| s.session_id.clone()),
            })
        })
        .collect();
    Ok(json!({ "sessions": sessions }))
}

/// A fresh id in the desktop app's shape (`<stem>-<base36 ms>-<n>`), with a
/// random tail instead of the app's per-process counter: several CLI processes
/// mint at once and a counter starting at 1 in each would collide.
fn mint_id(stem: &str) -> String {
    let ms = now_ms();
    let mut rand = [0u8; 4];
    let random = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut rand))
        .map(|_| u32::from_le_bytes(rand))
        .unwrap_or_else(|_| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() ^ std::process::id())
                .unwrap_or(0)
        });
    format!("{stem}-{}-{}", base36(ms), base36(random as u64))
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// The daemon name for a new child of `kind`, matching the app's
/// `reattachName`: Claude `claude-<id>`, pi `pi-<id>`, Codex `codex-<id>`, a shell its id verbatim
/// (whose own prefix is `term-`).
pub(crate) fn child_name(kind: Kind) -> String {
    match kind {
        Kind::Claude => format!("claude-{}", mint_id("session")),
        Kind::Pi => format!("pi-{}", mint_id("pisession")),
        Kind::Codex => format!("codex-{}", mint_id("codexsession")),
        Kind::Terminal => mint_id("term"),
    }
}

/// The flag that turns Claude's permission checks off.
const SKIP_PERMISSIONS: &str = "--dangerously-skip-permissions";

/// May a child be started with permission checks off?
///
/// **Only when the caller's own session was.** CLAUDE.md treats the flag as a
/// deliberate judgement about one piece of work, made by the operator at launch;
/// letting an agent that runs *with* checks spawn one *without* them would be
/// that agent granting itself latitude nobody granted it. So the child may have
/// at most the parent's latitude, the flag must be asked for explicitly (a child
/// never inherits it silently), and with no parent session there is no latitude
/// to inherit at all. This is a guardrail against escalation by a cooperating
/// agent, not a security boundary: anything running as this user could run
/// `claude --dangerously-skip-permissions` directly.
fn check_skip_permissions(caller: Option<&str>, parent_command: Option<&str>) -> Result<(), CliError> {
    let Some(caller) = caller else {
        return Err(CliError::new(
            code::PERMISSION_DENIED,
            "--skip-permissions is only allowed from inside an rmux session that was itself \
             started with it (RMUX_SESSION is not set)",
        ));
    };
    // Claude's flag and Codex's are the same latitude — "no checks on what this
    // agent runs" — so either one in the parent's command permits either in
    // the child. Neither grants more than the parent was given.
    let parent_has_it = parent_command.is_some_and(|c| {
        c.split_whitespace().any(|w| w == SKIP_PERMISSIONS || w == rmux_claude_core::codex::BYPASS_FLAG)
    });
    if !parent_has_it {
        return Err(CliError::new(
            code::PERMISSION_DENIED,
            format!(
                "--skip-permissions refused: {caller} was not started with {SKIP_PERMISSIONS}, \
                 so it cannot grant a child more latitude than it has"
            ),
        ));
    }
    Ok(())
}

async fn spawn(args: &[String]) -> CliResult {
    let a = Args::parse(
        args,
        &["--agent", "--cwd", "--name", "--prompt", "--profile", "--timeout"],
        &["--skip-permissions", "--json"],
    )?;
    if let Some(extra) = a.positional.first() {
        return Err(CliError::usage(format!("unexpected argument {extra:?}")));
    }
    let kind = match a.value("--agent") {
        Some("claude") => Kind::Claude,
        Some("pi") => Kind::Pi,
        Some("codex") => Kind::Codex,
        Some("shell") | Some("terminal") => Kind::Terminal,
        Some(other) => {
            return Err(CliError::usage(format!(
                "--agent takes claude, pi, codex or shell, not {other:?}"
            )));
        }
        None => return Err(CliError::usage("--agent claude|pi|codex|shell is required")),
    };
    // `--profile` names a credential set the app delivered to this host (a
    // profile id, or `default`/`none`). Without it the child inherits the
    // parent's set — see `inherit`.
    let profile = a.value("--profile").map(str::to_owned);
    let skip = a.has("--skip-permissions");
    // For Codex the same switch means `--dangerously-bypass-approvals-and-sandbox`,
    // under the same parent-latitude rule.
    if skip && !matches!(kind, Kind::Claude | Kind::Codex) {
        return Err(CliError::usage("--skip-permissions applies to --agent claude or codex only"));
    }
    let timeout = a.timeout(SPAWN_TIMEOUT)?;
    let label = a.value("--name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned);
    let prompt = match a.value("--prompt") {
        Some(raw) => Some(text_arg(raw)?).filter(|p| !p.trim().is_empty()),
        None => None,
    };

    // The caller's cwd by default — herdr's rule: work happens where you are
    // unless the operator asked for somewhere else.
    let cwd = match a.value("--cwd") {
        Some(dir) => bridge::expand_home(dir)?,
        None => caller_cwd()?,
    };
    let cwd = if std::path::Path::new(&cwd).is_absolute() {
        cwd
    } else {
        std::env::current_dir().map(|d| d.join(&cwd).to_string_lossy().into_owned()).unwrap_or(cwd)
    };
    // Must exist: creating it would turn a typo into a directory tree.
    if !std::path::Path::new(&cwd).is_dir() {
        return Err(CliError::usage(format!("{cwd} is not a directory")));
    }

    let parent = caller();
    let before = attach::list().await?;
    if skip {
        let parent_command = parent
            .as_deref()
            .and_then(|p| before.iter().find(|s| s.name == p))
            .and_then(|s| s.command.as_deref());
        check_skip_permissions(parent.as_deref(), parent_command)?;
    }

    // Short prompts are a launch argument (reliable — no guessing when the
    // composer is drawn); long ones are typed in once the agent is ready.
    let (launch_prompt, typed_prompt) = match (&prompt, kind) {
        (Some(p), Kind::Claude | Kind::Pi | Kind::Codex) if p.len() <= MAX_ARGV_PROMPT => {
            (Some(p.as_str()), None)
        }
        (Some(p), _) => (None, Some(p.clone())),
        (None, _) => (None, None),
    };
    let line = match kind {
        Kind::Claude => {
            let extra: Vec<String> = if skip { vec![SKIP_PERMISSIONS.to_owned()] } else { Vec::new() };
            Some(bridge::launch_line_for(Agent::Claude, launch_prompt, None, &extra))
        }
        Kind::Pi => Some(bridge::launch_line_for(Agent::Pi, launch_prompt, label.as_deref(), &[])),
        // The same line the desktop app builds — one definition. No profile:
        // `--profile` was refused above, so the host's own Codex login is used.
        Kind::Codex => Some(rmux_claude_core::codex::launch_line(
            &rmux_claude_core::codex::Launch { bypass: skip, ..Default::default() },
            launch_prompt,
        )),
        Kind::Terminal => None,
    };

    let session = child_name(kind);
    let parent_binding = match parent.as_deref() {
        Some(p) => attach::binding_anywhere(p).await,
        None => None,
    };
    let plan = crate::inherit::plan(
        parent_binding.as_ref(),
        rmux_claude_core::env_kind::for_session(&session),
        profile.as_deref(),
    )
    .map_err(CliError::usage)?;
    let binding = plan.binding();
    let spawned_at = now_ms();
    if let Err(e) =
        attach::spawn_detached_bound(&session, &cwd, line.as_deref(), SPAWN_COLS, SPAWN_ROWS, binding.clone()).await
    {
        let message = match (&plan, parent_binding.as_ref().and_then(|r| r.binding.as_ref())) {
            (crate::inherit::Plan::Default(b), Some(p)) => crate::inherit::no_default(&p.kind, &b.kind),
            _ => e.to_string(),
        };
        return Err(CliError::new(code::UNSUPPORTED, message));
    }

    // Recorded before the readiness wait, so a child that never becomes ready
    // is still one the caller owns and can close.
    let live = live_names().await?;
    children::record(
        children::Child {
            session: session.clone(),
            parent: parent.clone(),
            kind: kind.as_str().to_owned(),
            cwd: cwd.clone(),
            name: label.clone().unwrap_or_else(|| session.clone()),
            created_at: now_ms(),
            profile: binding.as_ref().map(|b| b.set.clone()),
            chain: binding.as_ref().and_then(|b| b.chain.as_ref()).map(|c| c.id.clone()),
        },
        &live,
    )?;
    if let Some(label) = &label
        && let Err(e) = alias::set(&session, label, &live)
    {
        // A session that started but could not be named is still a session.
        tracing::warn!(error = %e, "could not name the spawned session");
    }

    let state = wait_ready(&session, kind, timeout).await?;

    if let Some(text) = typed_prompt {
        let sent_at = now_ms();
        deliver(&session, kind, &text).await?;
        if kind.is_agent() {
            record_prompt(&session, sent_at, &live);
        }
    } else if launch_prompt.is_some() {
        // The launch prompt counts as "the last prompt" for `wait --until done`,
        // stamped at launch: a quick turn may already be over by the time the
        // agent reads as ready.
        record_prompt(&session, spawned_at, &live);
    }

    Ok(Output::Json(json!({
        "session": session,
        "kind": kind.as_str(),
        "cwd": cwd,
        "name": label,
        "parent": parent,
        "profile": binding.as_ref().map(|b| b.set.clone()),
        "chain": binding.as_ref().and_then(|b| b.chain.as_ref()).map(|c| c.id.clone()),
        "status": state.as_str(),
    })))
}

/// Wait until a freshly spawned session can take input.
async fn wait_ready(session: &str, kind: Kind, timeout: Option<Duration>) -> Result<State, CliError> {
    let started = Instant::now();
    let mut last_len = 0usize;
    let mut quiet_since = Instant::now();
    loop {
        let Some(obs) = observe(session).await? else {
            // Exited. Its last words are usually the reason (`command not found`).
            let tail = attach::read_scrollback(session)
                .await
                .ok()
                .flatten()
                .map(|b| tail_lines(&screen_text(&b), 8))
                .unwrap_or_default();
            return Err(CliError::new(
                code::SPAWN_FAILED,
                if tail.is_empty() {
                    format!("{session} exited during startup")
                } else {
                    format!("{session} exited during startup:\n{tail}")
                },
            )
            .with("session", session));
        };

        let screen = attach::read(session).await.ok().flatten().unwrap_or_default();
        if screen.len() != last_len {
            last_len = screen.len();
            quiet_since = Instant::now();
        }

        let ready = match kind {
            // The agent's own status is the authority; Claude publishes it from
            // startup. The screen markers cover a Claude too old to publish.
            Kind::Claude | Kind::Pi if obs.status.is_some() => Some(obs.state()),
            Kind::Claude => {
                let text = screen_text(&screen);
                if text.contains("Do you trust") {
                    Some(State::Blocked)
                } else if text.contains("for shortcuts") {
                    Some(State::Idle)
                } else {
                    None
                }
            }
            Kind::Pi => (last_len > 0 && quiet_since.elapsed() >= PI_SETTLE).then_some(State::Unknown),
            // Codex publishes no status, and this side has no terminal emulator —
            // only the raw replay collapsed to text. So only the unmistakable
            // startup blocker is read (the folder-trust question, which must go
            // back to the caller as `agent_not_ready`), and readiness otherwise
            // means "output has settled", reported as `unknown` rather than a
            // guessed `idle`. The desktop app reads Codex's real rendered screen.
            Kind::Codex => {
                let text = screen_text(&screen);
                let lines: Vec<String> = text.lines().map(str::to_owned).collect();
                let recent = &lines[lines.len().saturating_sub(40)..];
                if rmux_claude_core::codex::detect(recent) == rmux_claude_core::codex::State::Blocked {
                    Some(State::Blocked)
                } else {
                    (last_len > 0 && quiet_since.elapsed() >= PI_SETTLE).then_some(State::Unknown)
                }
            }
            Kind::Terminal => {
                (last_len > 0 && quiet_since.elapsed() >= SHELL_SETTLE).then_some(State::Unknown)
            }
        };
        match ready {
            // herdr: blocked during startup is `agent_not_ready`, returned at
            // once, with the session kept so it can be read and answered.
            Some(State::Blocked) => {
                return Err(CliError::new(
                    code::AGENT_NOT_READY,
                    format!(
                        "{session} started but is waiting on a dialog (often the folder-trust \
                         question); read it and ask the operator"
                    ),
                )
                .with("session", session)
                .with("status", "blocked"));
            }
            Some(state) => return Ok(state),
            None => {}
        }
        if timeout.is_some_and(|t| started.elapsed() >= t) {
            return Err(CliError::new(code::TIMEOUT, format!("{session} did not become ready in time"))
                .with("session", session)
                .with("status", obs.state().as_str()));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Why `wait` is refused for Codex rather than left to time out.
///
/// Codex publishes no status file, and this side reads only the raw replay — so
/// a wait here could only ever end in a timeout or a guess, and a guessed
/// "done" is the one answer an orchestrating agent must never be given.
fn codex_has_no_status(session: &str) -> CliError {
    CliError::new(
        code::UNSUPPORTED,
        "Codex publishes no status this CLI can wait on (the rmux app reads its screen); \
         use wait-output with a pattern, or read --source screen",
    )
    .with("session", session)
}

/// Type `text` into a session and submit it — the agent encoding for Claude
/// and pi, text then Enter for a shell.
async fn deliver(session: &str, kind: Kind, text: &str) -> Result<(), CliError> {
    let ok = if kind.is_agent() {
        bridge::send_message(session, text).await?
    } else {
        // A shell runs each line it is given; a trailing Enter submits the last.
        let body = text.trim_end_matches('\n').replace('\n', "\r");
        if attach::write(session, body.as_bytes()).await? {
            tokio::time::sleep(keys::SUBMIT_SETTLE).await;
            attach::write(session, b"\r").await?
        } else {
            false
        }
    };
    if ok { Ok(()) } else { Err(not_found(session)) }
}

async fn prompt(args: &[String]) -> CliResult {
    let a = Args::parse(args, &["--until", "--timeout"], &["--wait", "--json"])?;
    let session = a.session()?;
    let raw = a.positional.get(1).ok_or_else(|| CliError::usage("prompt needs TEXT (or - for stdin)"))?;
    if a.positional.len() > 2 {
        return Err(CliError::usage("prompt takes one TEXT argument; quote it, or pass - and use stdin"));
    }
    let wait = a.has("--wait");
    let until = match a.value("--until") {
        Some(_) if !wait => return Err(CliError::usage("--until needs --wait")),
        Some(u) => Until::parse(u)?,
        None => Until::Done,
    };
    if until == Until::Working {
        return Err(CliError::usage("prompt --wait --until takes done, blocked or idle"));
    }
    let timeout = a.timeout(WAIT_TIMEOUT)?;
    let text = text_arg(raw)?;
    if text.trim().is_empty() {
        return Err(CliError::usage("an empty prompt would submit nothing"));
    }

    let obs = observe_or_missing(&session).await?;
    let kind = obs.kind();
    if wait && !kind.is_agent() {
        return Err(CliError::new(
            code::UNSUPPORTED,
            "a terminal has no agent status to wait on; use wait-output",
        )
        .with("session", session.as_str()));
    }
    if wait && kind == Kind::Codex {
        return Err(codex_has_no_status(&session));
    }
    // Refused *before* any byte is written: text typed into an open dialog
    // answers it.
    if kind.is_agent() && obs.state() == State::Blocked {
        return Err(CliError::new(
            code::AGENT_BLOCKED,
            format!("{session} is waiting on a dialog; nothing was sent. Read it and ask the operator"),
        )
        .with("session", session.as_str())
        .with("status", "blocked"));
    }

    let sent_at = now_ms();
    deliver(&session, kind, &text).await?;
    if kind.is_agent() {
        record_prompt(&session, sent_at, &live_names().await.unwrap_or_default());
    }

    let status = if wait {
        wait_until(&session, until, sent_at, timeout, Some(STALL_WINDOW)).await?
    } else {
        observe(&session).await?.map(|o| o.state()).unwrap_or(State::Unknown)
    };
    Ok(Output::Json(json!({
        "session": session,
        "submitted": true,
        "status": status.as_str(),
    })))
}

async fn wait(args: &[String]) -> CliResult {
    let a = Args::parse(args, &["--until", "--timeout"], &["--json"])?;
    let session = a.session()?;
    let until = Until::parse(a.value("--until").unwrap_or("done"))?;
    let timeout = a.timeout(WAIT_TIMEOUT)?;
    let started = now_ms();

    let obs = observe_or_missing(&session).await?;
    if !obs.kind().is_agent() {
        return Err(CliError::new(
            code::UNSUPPORTED,
            "a terminal has no agent status to wait on; use wait-output",
        )
        .with("session", session.as_str()));
    }
    if obs.kind() == Kind::Codex {
        return Err(codex_has_no_status(&session));
    }
    // `done` counts work since the last prompt when there was one, so
    // `prompt` followed later by `wait` does not miss a turn that already ended.
    let since = last_prompt_at(&session).unwrap_or(started);
    let state = wait_until(&session, until, since, timeout, None).await?;
    Ok(Output::Json(json!({
        "session": session,
        "reached": until.as_str(),
        "status": state.as_str(),
    })))
}

async fn read(args: &[String]) -> CliResult {
    let a = Args::parse(args, &["--lines", "--source"], &["--json"])?;
    let session = a.session()?;
    let source = a.value("--source").unwrap_or("screen");
    let (text, truncated) = match source {
        "screen" | "visible" | "recent" => {
            let lines = a.lines(50)?;
            (tail_lines(&screen_of(&session).await?, lines), false)
        }
        "transcript" => {
            let lines = a.lines(10)?;
            let obs = observe_or_missing(&session).await?;
            let kind = obs.kind();
            if !kind.is_agent() {
                return Err(CliError::new(code::UNSUPPORTED, "a terminal has no transcript; use --source screen")
                    .with("session", session.as_str()));
            }
            let Some(conversation) = obs.status.as_ref().map(|s| s.session_id.clone()) else {
                return Err(CliError::new(
                    code::UNSUPPORTED,
                    format!("{session} has not published a conversation id yet; use --source screen"),
                )
                .with("session", session.as_str()));
            };
            if kind == Kind::Codex {
                // Codex's rollout is readable (the desktop app does), but this
                // verb reads by a published conversation id, and Codex
                // publishes none — so it says so instead of guessing a file.
                return Err(CliError::new(
                    code::UNSUPPORTED,
                    "Codex publishes no conversation id to read by; use --source screen",
                )
                .with("session", session.as_str()));
            }
            let (messages, truncated) = if kind == Kind::Pi {
                bridge::pi_read_conversation(&conversation, None)?
            } else {
                bridge::read_conversation(&conversation, None)?
            };
            let shown: Vec<_> = messages.into_iter().filter(|m| m.role != Role::System).collect();
            let start = shown.len().saturating_sub(lines);
            let text = shown[start..]
                .iter()
                .map(|m| {
                    let role = match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::Tool => "tool",
                        Role::System => "system",
                    };
                    format!("[{role}]\n{}", m.text.trim_end())
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            (text, truncated || start > 0)
        }
        other => {
            return Err(CliError::usage(format!("--source takes screen or transcript, not {other:?}")));
        }
    };
    if a.has("--json") {
        Ok(Output::Json(json!({
            "session": session,
            "source": if source == "transcript" { "transcript" } else { "screen" },
            "text": text,
            "truncated": truncated,
        })))
    } else {
        Ok(Output::Text(text))
    }
}

async fn wait_output(args: &[String]) -> CliResult {
    let a = Args::parse(args, &["--regex", "--match", "--timeout"], &["--json"])?;
    let session = a.session()?;
    let timeout = a.timeout(WAIT_TIMEOUT)?;
    let pattern = match (a.value("--regex"), a.value("--match")) {
        (Some(r), None) => regex::Regex::new(r)
            .map_err(|e| CliError::usage(format!("--regex does not compile: {e}")))?,
        (None, Some(m)) => regex::Regex::new(&regex::escape(m)).map_err(|e| CliError::usage(e.to_string()))?,
        _ => return Err(CliError::usage("wait-output needs exactly one of --regex or --match")),
    };

    // herdr semantics: the existing buffer is searched at once, so output that
    // arrived before the call still matches — otherwise `prompt` then
    // `wait-output` races the command it just ran.
    let started = Instant::now();
    loop {
        let text = screen_of(&session).await?;
        if let Some(m) = pattern.find(&text) {
            let line_start = text[..m.start()].rfind('\n').map(|p| p + 1).unwrap_or(0);
            let line_end = text[m.end()..].find('\n').map(|p| m.end() + p).unwrap_or(text.len());
            return Ok(Output::Json(json!({
                "session": session,
                "matched": m.as_str(),
                "line": text[line_start..line_end].trim_end(),
            })));
        }
        if timeout.is_some_and(|t| started.elapsed() >= t) {
            return Err(CliError::new(code::TIMEOUT, format!("no match in {session} in time"))
                .with("session", session.as_str()));
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn send_keys(args: &[String]) -> CliResult {
    let a = Args::parse(args, &[], &["--json"])?;
    let session = a.session()?;
    let names = &a.positional[1..];
    if names.is_empty() {
        return Err(CliError::usage("keys needs at least one KEY (enter, esc, tab, up, down, ctrl-c, y, 1, …)"));
    }
    // Every key validated before any is written: half a sequence delivered to a
    // TUI has already moved a highlight or dismissed a dialog.
    let mut bytes = Vec::with_capacity(names.len());
    for name in names {
        bytes.push(keys::named(name).ok_or_else(|| CliError::usage(format!("unknown key {name:?}")))?);
    }
    observe_or_missing(&session).await?;
    for (i, key) in bytes.iter().enumerate() {
        if i > 0 {
            // Separate writes, spaced: `esc` then `[` in one write is Alt-[ to
            // a TUI, not two keys.
            tokio::time::sleep(keys::SUBMIT_SETTLE).await;
        }
        if !attach::write(&session, key).await? {
            return Err(not_found(&session));
        }
    }
    Ok(Output::Json(json!({ "session": session, "sent": names.len() })))
}

async fn interrupt(args: &[String]) -> CliResult {
    let a = Args::parse(args, &[], &["--json"])?;
    let session = a.session()?;
    let obs = observe_or_missing(&session).await?;
    // Esc stops an agent's turn (it is what Claude means by "esc to
    // interrupt"); a shell's foreground job wants Ctrl-C.
    let key: &[u8] = if obs.kind().is_agent() { keys::interrupt() } else { b"\x03" };
    if !attach::write(&session, key).await? {
        return Err(not_found(&session));
    }
    Ok(Output::Json(json!({ "session": session, "interrupted": true })))
}

async fn close(args: &[String]) -> CliResult {
    let a = Args::parse(args, &[], &["--force", "--json"])?;
    let session = a.session()?;
    let force = a.has("--force");
    let obs = observe_or_missing(&session).await?;

    let live = live_names().await?;
    let tree = children::read_pruned(&live)?;
    let me = caller();
    let owned = me.as_deref().is_some_and(|me| children::descends_from(&tree, &session, me));
    if !owned && !force {
        let message = match me {
            Some(me) => format!(
                "{session} was not spawned by {me} (see `rmux children`); only sessions you \
                 started can be closed"
            ),
            None => format!(
                "not inside an rmux session (RMUX_SESSION is unset), so no session is yours to \
                 close; {session} was left running"
            ),
        };
        return Err(CliError::new(code::NOT_OWNER, message).with("session", session.as_str()));
    }

    // `kill` is answered during the handshake and resolves the owning build,
    // so this ends the process — not merely the map entry.
    attach::kill(&session).await?;
    let pid = obs.summary.pid;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let still_listed = live_names().await?.iter().any(|n| n == &session);
        let still_running = pid.is_some_and(status::pid_alive);
        if !still_listed && !still_running {
            break;
        }
        if Instant::now() >= deadline {
            return Err(CliError::new(code::CLOSE_FAILED, format!("{session} did not exit after kill"))
                .with("session", session.as_str()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = children::read_pruned(&live_names().await.unwrap_or_default());
    Ok(Output::Json(json!({ "session": session, "closed": true })))
}

async fn children_cmd(args: &[String]) -> CliResult {
    Args::parse(args, &[], &["--json"])?;
    let tree = children::read_pruned(&live_names().await?)?;
    Ok(Output::Json(serde_json::to_value(tree).map_err(anyhow::Error::from)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_vocabulary_maps_onto_herdr_s() {
        assert_eq!(State::from_raw("busy"), State::Working);
        assert_eq!(State::from_raw("shell"), State::Working);
        assert_eq!(State::from_raw("waiting"), State::Blocked);
        assert_eq!(State::from_raw("idle"), State::Idle);
        // Never a guess: an unknown status is not "finished".
        assert_eq!(State::from_raw("compacting"), State::Unknown);
    }

    #[test]
    fn done_needs_work_after_the_prompt() {
        let since = 1_000;
        // Idle, last change before the prompt: has not picked it up yet.
        assert!(!reached(Until::Done, State::Idle, Some(999), since, false));
        // Idle after a change at/after the prompt: did the work and settled.
        assert!(reached(Until::Done, State::Idle, Some(1_500), since, false));
        // Idle with no timestamp, but working was seen in this wait.
        assert!(reached(Until::Done, State::Idle, None, since, true));
        // Still working is not done.
        assert!(!reached(Until::Done, State::Working, Some(2_000), since, true));
        assert!(reached(Until::Blocked, State::Blocked, None, since, false));
        assert!(reached(Until::Idle, State::Idle, Some(1), since, false));
    }

    #[test]
    fn skip_permissions_is_bounded_by_the_parent() {
        let err = check_skip_permissions(None, None).unwrap_err();
        assert_eq!(err.code, code::PERMISSION_DENIED);
        let err = check_skip_permissions(Some("claude-a"), Some("CLAUDE_CODE_DISABLE_MOUSE=1 claude")).unwrap_err();
        assert_eq!(err.code, code::PERMISSION_DENIED);
        // A prompt that merely *mentions* the flag inside quotes is one word
        // with other text, not the flag.
        let err = check_skip_permissions(Some("claude-a"), Some("claude 'use --dangerously-skip-permissions?'"));
        assert!(err.is_err());
        assert!(check_skip_permissions(Some("claude-a"), Some("claude --dangerously-skip-permissions")).is_ok());
    }

    #[test]
    fn minted_names_follow_the_app_s_reattach_scheme() {
        let c = child_name(Kind::Claude);
        assert!(c.starts_with("claude-session-"), "{c}");
        assert_eq!(Kind::of(&c), Kind::Claude);
        let p = child_name(Kind::Pi);
        assert!(p.starts_with("pi-pisession-"), "{p}");
        assert_eq!(Kind::of(&p), Kind::Pi);
        let x = child_name(Kind::Codex);
        assert!(x.starts_with("codex-codexsession-"), "{x}");
        assert_eq!(Kind::of(&x), Kind::Codex);
        // And the host scopes the OpenAI credentials to exactly that name.
        assert_eq!(rmux_claude_core::env_kind::for_session(&x), Some("openai"));
        let t = child_name(Kind::Terminal);
        assert!(t.starts_with("term-"), "{t}");
        assert_eq!(Kind::of(&t), Kind::Terminal);
        assert_ne!(child_name(Kind::Terminal), child_name(Kind::Terminal));
        // Nothing an alias or the TSV `list` cannot carry.
        assert!(c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'));
    }

    #[test]
    fn screen_text_collapses_carriage_return_overwrites() {
        let raw = b"\x1b[32mbuilding\x1b[0m 10%\r building 100%\r\ndone\r\n";
        assert_eq!(screen_text(raw), " building 100%\ndone\n");
        assert_eq!(tail_lines("a\nb\nc\n\n\n", 2), "b\nc");
    }

    #[test]
    fn arguments_parse_flags_anywhere_and_respect_double_dash() {
        let args: Vec<String> =
            ["s", "--wait", "--timeout=5", "--", "--not-a-flag"].iter().map(|s| s.to_string()).collect();
        let a = Args::parse(&args, &["--timeout"], &["--wait"]).unwrap();
        assert_eq!(a.positional, vec!["s", "--not-a-flag"]);
        assert!(a.has("--wait"));
        assert_eq!(a.value("--timeout"), Some("5"));
        let bad = Args::parse(&["--bogus".to_string()], &[], &[]).err().unwrap();
        assert_eq!(bad.exit(), 2);
    }
}
