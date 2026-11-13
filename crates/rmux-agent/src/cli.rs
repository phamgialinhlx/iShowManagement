//! `rmux` the way tmux is used: from any terminal, on this machine or any
//! server the operator added in the app.
//!
//! ```text
//! rmux                                  attach the most recent session, or start one
//! rmux ls [-t SERVER] [--json]          every session, here and on every server
//! rmux attach [-t [SERVER:]SESSION]     attach this terminal; C-b d detaches
//! rmux new [-s NAME] [-c DIR] [-t SERVER] [--claude|--pi|--codex] [-d]
//! rmux kill-session -t [SERVER:]SESSION
//! rmux rename-session [-t [SERVER:]SESSION] NEWNAME
//! rmux has-session -t [SERVER:]SESSION
//! rmux detach [-s SESSION]
//! ```
//!
//! This sits beside the agent-orchestration commands (`spawn`, `prompt`, …) in
//! one binary, and beside the primitives the desktop app drives (`attach
//! --session`, plain `list`, `kill`, `alias`), which keep their exact formats.
//! The tmux vocabulary is for a person at a terminal: plain-text output, errors
//! as `rmux: …` on stderr, exit 1 (2 for a malformed command line).
//!
//! **Remote work uses only primitives every agent build has** — plain `list`
//! (or `list --json` where the build has it), `kill`, `alias`, `attach
//! --session` — with targets resolved here. Only `new` needs the far side to
//! have this CLI, and an older build is told apart (`-V`) and named as the cause.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use rmux_bridge::protocol::Agent;
use serde_json::{Value, json};

use crate::attach::{self, AttachOptions};
use crate::detach::{self, DetachKey};
use crate::orchestrate::{self, Kind};
use crate::protocol::Hello;
use crate::servers::{self, ServerEntry, ServersFile, SshMode};
use crate::{alias, bridge, status};

/// The server name for this machine.
pub const LOCAL: &str = "local";

/// How long `ls` waits for one server before calling it unreachable. A little
/// over ssh's own `ConnectTimeout`, so ssh's message wins when it has one.
const HOST_TIMEOUT: Duration = Duration::from_secs(8);

// ---------------------------------------------------------------------------
// errors

#[derive(Debug)]
pub struct Fail {
    message: String,
    exit: u8,
}

impl Fail {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), exit: 1 }
    }
    fn usage(message: impl Into<String>) -> Self {
        Self { message: message.into(), exit: 2 }
    }
    /// Exit 1 with nothing printed — `has-session`'s "no".
    fn quiet() -> Self {
        Self { message: String::new(), exit: 1 }
    }
}

impl From<anyhow::Error> for Fail {
    fn from(e: anyhow::Error) -> Self {
        Self::new(e.to_string())
    }
}

impl From<orchestrate::CliError> for Fail {
    fn from(e: orchestrate::CliError) -> Self {
        Self::new(e.message)
    }
}

type R<T> = Result<T, Fail>;

// ---------------------------------------------------------------------------
// dispatch

/// The commands this module answers.
///
/// `attach` only *without* `--session`: `attach --session NAME` is the
/// primitive the desktop app (and a remote CLI attach) runs, and it keeps its
/// behaviour exactly — no detach key unless `--detach-key` is passed.
pub fn owns(command: Option<&str>, args: &[String]) -> bool {
    match command {
        None => true,
        Some(
            "ls" | "list-sessions" | "a" | "at" | "attach-session" | "new" | "new-session"
            | "kill-session" | "rename-session" | "has-session" | "has" | "detach"
            | "detach-client" | "-V" | "--version" | "-h" | "--help" | "help",
        ) => true,
        Some("attach") => !args.iter().any(|a| a == "--session" || a.starts_with("--session=")),
        Some(_) => false,
    }
}

pub async fn run(command: Option<&str>, args: &[String]) -> ExitCode {
    let result = match command {
        None => default().await,
        Some("ls" | "list-sessions") => ls(args).await,
        Some("attach" | "a" | "at" | "attach-session") => attach_cmd(args).await,
        Some("new" | "new-session") => new(args).await,
        Some("kill-session") => kill_session(args).await,
        Some("rename-session") => rename_session(args).await,
        Some("has-session" | "has") => has_session(args).await,
        Some("detach" | "detach-client") => detach_cmd(args).await,
        Some("-V" | "--version") => {
            println!("rmux {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Some(_) => {
            print!("{HELP}");
            Ok(ExitCode::SUCCESS)
        }
    };
    match result {
        Ok(code) => code,
        Err(fail) => {
            if !fail.message.is_empty() {
                eprintln!("rmux: {}", fail.message);
            }
            ExitCode::from(fail.exit)
        }
    }
}

pub const HELP: &str = "\
usage: rmux [command] [flags]

Sessions (tmux-style; SERVER is a server from the rmux app, `local`, or any
ssh destination such as user@host:port; SESSION is a name, alias or prefix):
  rmux                                    attach the most recent session, or start one
  ls | list-sessions [-t SERVER] [--json] list sessions here and on every server
  attach | a [-t [SERVER:]SESSION] [--detach-key KEY]
                                          attach this terminal (C-b d detaches,
                                          C-b C-b sends a literal C-b)
  new [-s NAME] [-c DIR] [-t SERVER] [--claude | --pi | --codex] [-d] [--detach-key KEY]
                                          start a session (a shell by default) and attach
  kill-session -t [SERVER:]SESSION        end a session and everything running in it
  rename-session [-t [SERVER:]SESSION] NEWNAME
  has-session -t [SERVER:]SESSION         exit 0 if it exists
  detach [-s SESSION]                     detach command-line clients of a session
  -V | --version

  KEY is \"C-b d\" by default, or $RMUX_DETACH_KEY; \"none\" turns it off.

Agent orchestration (JSON on stdout; see docs/orchestration.md):
  whoami | list --json | spawn | prompt | wait | read | wait-output
  keys | interrupt | close | children

Primitives the rmux app runs:
  attach --session NAME | list | kill NAME | alias NAME NEW | setenv | hook
  watch-status | daemon | bridge | version
";

// ---------------------------------------------------------------------------
// flags

struct Flags {
    values: Vec<(String, String)>,
    switches: Vec<String>,
    positional: Vec<String>,
}

impl Flags {
    /// `-t X`, `-tX`, `--long X`, `--long=X`, bare switches, `--` ends flags.
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> R<Self> {
        let mut out = Flags { values: Vec::new(), switches: Vec::new(), positional: Vec::new() };
        let mut i = 0;
        let mut done = false;
        while i < args.len() {
            let arg = &args[i];
            if done || arg == "-" || !arg.starts_with('-') {
                out.positional.push(arg.clone());
            } else if arg == "--" {
                done = true;
            } else {
                let (name, inline) = if arg.starts_with("--") {
                    match arg.split_once('=') {
                        Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
                        None => (arg.clone(), None),
                    }
                } else if let Some(short) = arg.get(..2).filter(|s| arg.len() > 2 && with_value.contains(s)) {
                    (short.to_owned(), Some(arg[2..].to_owned()))
                } else {
                    (arg.clone(), None)
                };
                if with_value.contains(&name.as_str()) {
                    let value = match inline {
                        Some(v) => v,
                        None => {
                            i += 1;
                            args.get(i).cloned().ok_or_else(|| Fail::usage(format!("{name} needs a value")))?
                        }
                    };
                    out.values.push((name, value));
                } else if switches.contains(&name.as_str()) && inline.is_none() {
                    out.switches.push(name);
                } else {
                    return Err(Fail::usage(format!("unknown option {arg} (see rmux --help)")));
                }
            }
            i += 1;
        }
        Ok(out)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }
    fn no_positional(&self) -> R<()> {
        match self.positional.first() {
            Some(extra) => Err(Fail::usage(format!("unexpected argument {extra:?}"))),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// sessions as rows

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// `local` or the server's label.
    pub server: String,
    /// The daemon's name for it — what everything is keyed by.
    pub session: String,
    pub alias: Option<String>,
    pub kind: String,
    pub status: String,
    pub cwd: Option<String>,
    pub attached: bool,
    pub age_seconds: u64,
}

impl Row {
    pub fn display(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.session)
    }
    fn to_json(&self) -> Value {
        json!({
            "server": self.server,
            "session": self.session,
            "alias": self.alias,
            "name": self.display(),
            "kind": self.kind,
            "status": self.status,
            "cwd": self.cwd,
            "attached": self.attached,
            "ageSeconds": self.age_seconds,
        })
    }
}

/// Rows from `list --json` (this build's own, or a remote build's).
pub fn rows_from_json(value: &Value, server: &str) -> Vec<Row> {
    let Some(sessions) = value.get("sessions").and_then(Value::as_array) else { return Vec::new() };
    sessions
        .iter()
        .filter_map(|s| {
            let session = s.get("session")?.as_str()?.to_owned();
            let text = |k: &str| s.get(k).and_then(Value::as_str).map(str::to_owned);
            Some(Row {
                server: server.to_owned(),
                kind: text("kind").unwrap_or_else(|| Kind::of(&session).as_str().to_owned()),
                status: text("status").unwrap_or_else(|| "unknown".into()),
                alias: text("alias"),
                cwd: text("cwd"),
                attached: s.get("attached").and_then(Value::as_bool).unwrap_or(false),
                age_seconds: s.get("ageSeconds").and_then(Value::as_u64).unwrap_or(0),
                session,
            })
        })
        .collect()
}

/// Rows from the tab-separated `list` every agent build prints.
///
/// Six columns (`name alias pid age attached command`) from builds with
/// aliases, five (`name pid age attached command`) from older ones — told
/// apart by which column holds `attached|detached`, the one field whose
/// vocabulary is fixed. Reading six as five would take the pid for an alias and
/// silently mislabel every row.
pub fn rows_from_tsv(text: &str, server: &str) -> Vec<Row> {
    let is_state = |f: &str| f == "attached" || f == "detached";
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let (alias, age, state) = if f.len() >= 5 && is_state(f[4]) {
                (Some(f[1]).filter(|a| *a != "-" && !a.is_empty()), f[3], f[4])
            } else if f.len() >= 4 && is_state(f[3]) {
                (None, f[2], f[3])
            } else {
                return None;
            };
            let session = f[0].to_owned();
            Some(Row {
                server: server.to_owned(),
                kind: Kind::of(&session).as_str().to_owned(),
                status: "unknown".into(),
                alias: alias.map(str::to_owned),
                cwd: None,
                attached: state == "attached",
                age_seconds: age.parse().unwrap_or(0),
                session,
            })
        })
        .collect()
}

/// Either format, as a remote build prints it for `list --json`: a build with
/// the orchestration CLI answers JSON; an older one ignores the flag and prints
/// its tab-separated list — so one round trip serves both.
pub fn rows_from_remote(stdout: &str, server: &str) -> Vec<Row> {
    let trimmed = stdout.trim_start();
    if trimmed.starts_with('{')
        && let Ok(value) = serde_json::from_str::<Value>(trimmed)
    {
        return rows_from_json(&value, server);
    }
    rows_from_tsv(stdout, server)
}

/// Which session `query` names: an alias first, then a daemon name, then a
/// unique prefix of either — tmux's order. Ambiguity is an error that lists the
/// matches, never a guess.
pub fn resolve<'a>(rows: &'a [Row], query: &str) -> Result<&'a Row, String> {
    if let Some(row) = rows.iter().find(|r| r.alias.as_deref() == Some(query)) {
        return Ok(row);
    }
    if let Some(row) = rows.iter().find(|r| r.session == query) {
        return Ok(row);
    }
    let matches: Vec<&Row> = rows
        .iter()
        .filter(|r| r.session.starts_with(query) || r.alias.as_deref().is_some_and(|a| a.starts_with(query)))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("can't find session: {query}{}", candidates(rows))),
        many => Err(format!(
            "{query} is ambiguous, it matches:\n{}",
            many.iter().map(|r| format!("  {}", describe(r))).collect::<Vec<_>>().join("\n")
        )),
    }
}

fn describe(r: &Row) -> String {
    match &r.alias {
        Some(a) => format!("{}:{a} ({})", r.server, r.session),
        None => format!("{}:{}", r.server, r.session),
    }
}

fn candidates(rows: &[Row]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "\nsessions:\n{}",
        rows.iter().map(|r| format!("  {}", describe(r))).collect::<Vec<_>>().join("\n")
    )
}

fn human_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// `server:name  kind  status  cwd  (attached|detached)  age`, aligned.
pub fn render(rows: &[Row]) -> String {
    let cells: Vec<[String; 6]> = rows
        .iter()
        .map(|r| {
            [
                format!("{}:{}", r.server, r.display()),
                r.kind.clone(),
                r.status.clone(),
                r.cwd.clone().unwrap_or_else(|| "-".into()),
                if r.attached { "(attached)" } else { "(detached)" }.to_owned(),
                human_age(r.age_seconds),
            ]
        })
        .collect();
    let mut widths = [0usize; 6];
    for row in &cells {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let mut out = String::new();
    for row in &cells {
        let mut line = String::new();
        for (i, c) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(c);
            } else {
                line.push_str(c);
                line.push_str(&" ".repeat(widths[i] - c.chars().count() + 2));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// where

fn home() -> R<PathBuf> {
    dirs::home_dir().ok_or_else(|| Fail::new("no home directory"))
}

#[derive(Clone, Debug)]
enum Where {
    Local,
    Remote(ServerEntry),
}

/// A server name: `local`, a server from `servers.json`, or a destination typed
/// verbatim (`user@host:port`), which goes to `ssh` unresolved.
fn server_for(name: &str, file: &ServersFile) -> R<Where> {
    if name == LOCAL {
        return Ok(Where::Local);
    }
    if let Some(s) = file.find(name) {
        return Ok(Where::Remote(s.clone()));
    }
    ServerEntry::from_destination(name)
        .map(Where::Remote)
        .ok_or_else(|| Fail::usage(format!("{name:?} is not a server or an ssh destination")))
}

/// Split `[SERVER:]SESSION`. A prefix naming a known server (or `local`) wins
/// at the first colon that makes one; otherwise the last colon splits off a
/// verbatim ssh destination (`me@host:2222:demo`). `None` means no server was
/// named — this machine, unless the caller decides otherwise.
fn split_target(raw: &str, file: &ServersFile) -> (Option<String>, String) {
    for (i, _) in raw.match_indices(':') {
        let left = &raw[..i];
        if !left.is_empty() && (left == LOCAL || file.find(left).is_some()) {
            return (Some(left.to_owned()), raw[i + 1..].to_owned());
        }
    }
    match raw.rsplit_once(':') {
        Some((left, right)) if !left.is_empty() => (Some(left.to_owned()), right.to_owned()),
        _ => (None, raw.to_owned()),
    }
}

/// Resolve a `-t` session target to where it lives and which row it is.
async fn find_target(raw: &str) -> R<(Where, Row)> {
    let file = servers::load(&home()?);
    let (server, session) = split_target(raw, &file);
    // An unknown prefix might just be a local alias with a colon in it.
    if server.as_deref().is_some_and(|s| s != LOCAL && file.find(s).is_none()) {
        let rows = local_rows().await?;
        if let Ok(row) = resolve(&rows, raw) {
            return Ok((Where::Local, row.clone()));
        }
    }
    if session.is_empty() {
        return Err(Fail::usage(format!("{raw:?} names a server but no session")));
    }
    let place = match server {
        Some(s) => server_for(&s, &file)?,
        None => Where::Local,
    };
    let rows = rows_at(&place, false).await?;
    let row = resolve(&rows, &session).map_err(Fail::new)?.clone();
    Ok((place, row))
}

async fn local_rows() -> R<Vec<Row>> {
    Ok(rows_from_json(&orchestrate::list_value().await?, LOCAL))
}

async fn rows_at(place: &Where, batch: bool) -> R<Vec<Row>> {
    match place {
        Where::Local => local_rows().await,
        Where::Remote(server) => {
            let out = ssh_capture(server, &servers::remote_line(&["list", "--json"]), batch).await?;
            Ok(rows_from_remote(&out, &server.label))
        }
    }
}

fn ensure_control_dir(home: &Path) {
    let dir = servers::control_dir(home);
    if std::fs::create_dir_all(&dir).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
}

/// Run a line on `server` and return its stdout, or a one-line reason.
async fn ssh_capture(server: &ServerEntry, line: &str, batch: bool) -> R<String> {
    let home = home()?;
    ensure_control_dir(&home);
    let args = servers::ssh_args(server, &home, SshMode { tty: false, batch }, line, Path::exists);
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // A GUI-less console child still gets a window of its own otherwise.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = match tokio::time::timeout(HOST_TIMEOUT, cmd.output()).await {
        Err(_) => return Err(Fail::new("unreachable (timed out)")),
        Ok(Err(e)) => return Err(Fail::new(format!("could not run ssh: {e}"))),
        Ok(Ok(o)) => o,
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if servers::looks_like_windows(&stdout) || servers::looks_like_windows(&stderr) {
        return Err(Fail::new("Windows host: session persistence is not supported there"));
    }
    if !output.status.success() {
        let reason = stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| l.strip_prefix("rmux: ").or_else(|| l.strip_prefix("rmux-agent: ")).unwrap_or(l))
            .next_back()
            .unwrap_or("failed")
            .to_owned();
        return Err(Fail::new(reason));
    }
    Ok(stdout)
}

/// Replace this process with `ssh` running `line` on `server` with a terminal.
///
/// `exec`, so ssh owns this terminal directly — raw mode, window changes and
/// `~.` are all ssh's, and nothing of ours sits on the keystroke path.
fn ssh_exec(server: &ServerEntry, line: &str, tty: bool) -> R<ExitCode> {
    let home = home()?;
    ensure_control_dir(&home);
    let args = servers::ssh_args(server, &home, SshMode { tty, batch: false }, line, Path::exists);
    let mut cmd = std::process::Command::new("ssh");
    cmd.args(&args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = cmd.exec();
        Err(Fail::new(format!("could not run ssh: {error}")))
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().map_err(|e| Fail::new(format!("could not run ssh: {e}")))?;
        Ok(if status.success() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
    }
}

// ---------------------------------------------------------------------------
// "most recently attached", and the CLI clients `detach` can reach

fn recent_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".rmux").join("recent.json"))
}

/// Remember `session` as the most recently attached. Every attach records it —
/// the app's too, which is tmux's meaning of "most recently used". Best-effort:
/// failing an attach over a convenience file would be absurd.
pub fn record_recent(session: &str) {
    let Some(path) = recent_path() else { return };
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let temp = dir.join(format!("recent.json.{}.tmp", std::process::id()));
    let body = json!({ "session": session, "at": now_ms() }).to_string();
    if std::fs::write(&temp, body).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }
    let _ = std::fs::rename(&temp, &path);
}

fn read_recent() -> Option<String> {
    let text = std::fs::read_to_string(recent_path()?).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("session")?.as_str().map(str::to_owned)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn clients_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".rmux").join("clients"))
}

/// A command-line attach client, registered while it runs so `rmux detach`
/// (typed inside the session) can signal it. Removed on drop.
pub struct ClientRecord {
    path: Option<PathBuf>,
}

impl ClientRecord {
    pub fn register(session: &str) -> Self {
        let path = clients_dir().and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            let path = dir.join(std::process::id().to_string());
            std::fs::write(&path, session).ok()?;
            Some(path)
        });
        Self { path }
    }
}

impl Drop for ClientRecord {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ---------------------------------------------------------------------------
// commands

/// `--detach-key`, else `$RMUX_DETACH_KEY`, else `C-b d`.
fn detach_key(flags: &Flags) -> R<Option<DetachKey>> {
    let env = std::env::var(detach::ENV).ok().filter(|v| !v.trim().is_empty());
    let spec = flags.value("--detach-key").map(str::to_owned).or(env).unwrap_or_else(|| detach::DEFAULT.to_owned());
    DetachKey::parse(&spec).map_err(Fail::usage)
}

fn key_spec(key: Option<DetachKey>) -> String {
    key.map(|k| k.spec()).unwrap_or_else(|| "none".to_owned())
}

/// The session this process runs inside, if any.
fn current_session() -> Option<String> {
    std::env::var("RMUX_SESSION").ok().filter(|s| !s.is_empty())
}

async fn attach_local(row: &Row, key: Option<DetachKey>) -> R<ExitCode> {
    // Attaching a session to itself feeds its output back in as input.
    if current_session().as_deref() == Some(row.session.as_str()) {
        return Err(Fail::new(format!("{} is the session this terminal is already in", row.display())));
    }
    let hello = Hello {
        session: row.session.clone(),
        cwd: None,
        program: None,
        args: Vec::new(),
        login_command: None,
        env: Default::default(),
        cols: 80,
        rows: 24,
    };
    let options = AttachOptions { detach: key, label: Some(row.display().to_owned()), binding: None };
    Ok(attach::attach_with(hello, true, options).await?)
}

/// Attach to a row wherever it lives.
async fn attach_row(place: &Where, row: &Row, key: Option<DetachKey>) -> R<ExitCode> {
    match place {
        Where::Local => attach_local(row, key).await,
        Where::Remote(server) => {
            let spec = key.map(|k| k.spec());
            ssh_exec(server, &servers::remote_attach_line(&row.session, spec.as_deref()), true)
        }
    }
}

/// The local session a bare `attach` means: the most recently attached one if
/// it is still running, else the only one. Never the session we are inside.
fn pick_recent(rows: &[Row], recent: Option<&str>, me: Option<&str>) -> Result<Option<Row>, String> {
    let rows: Vec<&Row> = rows.iter().filter(|r| Some(r.session.as_str()) != me).collect();
    if let Some(recent) = recent
        && let Some(row) = rows.iter().find(|r| r.session == recent)
    {
        return Ok(Some((*row).clone()));
    }
    match rows.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some((*one).clone())),
        many => Err(format!(
            "which session? none was attached recently; name one with -t:\n{}",
            many.iter().map(|r| format!("  {}", describe(r))).collect::<Vec<_>>().join("\n")
        )),
    }
}

async fn default() -> R<ExitCode> {
    let rows = local_rows().await?;
    let key = detach_key(&Flags { values: Vec::new(), switches: Vec::new(), positional: Vec::new() })?;
    let me = current_session();
    let pick = match pick_recent(&rows, read_recent().as_deref(), me.as_deref()) {
        Ok(pick) => pick,
        // Several and none recent: the newest, rather than refusing a bare `rmux`.
        Err(_) => rows
            .iter()
            .filter(|r| Some(r.session.as_str()) != me.as_deref())
            .min_by_key(|r| r.age_seconds)
            .cloned(),
    };
    match pick {
        Some(row) => attach_local(&row, key).await,
        None => new_local(None, None, Kind::Terminal, false, key).await,
    }
}

async fn ls(args: &[String]) -> R<ExitCode> {
    let flags = Flags::parse(args, &["-t"], &["--json", "-a"])?;
    flags.no_positional()?;
    let json_out = flags.has("--json");
    let file = servers::load(&home()?);

    // One place, or this machine plus every server, asked in parallel.
    let places: Vec<(String, Where)> = match flags.value("-t") {
        Some(name) => {
            let place = server_for(name, &file)?;
            let label = match &place {
                Where::Local => LOCAL.to_owned(),
                Where::Remote(s) => s.label.clone(),
            };
            vec![(label, place)]
        }
        None => std::iter::once((LOCAL.to_owned(), Where::Local))
            .chain(file.servers.iter().map(|s| (s.label.clone(), Where::Remote(s.clone()))))
            .collect(),
    };
    let single = flags.value("-t").is_some();
    let results = futures::future::join_all(places.iter().map(|(label, place)| async move {
        (label.clone(), rows_at(place, !single).await)
    }))
    .await;

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for (label, result) in results {
        match result {
            Ok(r) => rows.extend(r),
            Err(e) => errors.push((label, e.message)),
        }
    }

    if json_out {
        let value = json!({
            "sessions": rows.iter().map(Row::to_json).collect::<Vec<_>>(),
            "errors": errors.iter().map(|(s, m)| json!({ "server": s, "message": m })).collect::<Vec<_>>(),
        });
        println!("{value}");
    } else {
        print!("{}", render(&rows));
        let dim = std::io::IsTerminal::is_terminal(&std::io::stderr());
        for (server, message) in &errors {
            if dim {
                eprintln!("\x1b[2m{server}: {message}\x1b[0m");
            } else {
                eprintln!("{server}: {message}");
            }
        }
        if rows.is_empty() && errors.is_empty() {
            eprintln!("no sessions");
        }
    }
    // One named server that could not be reached is a failure; one of many is
    // a line in the listing.
    if single && !errors.is_empty() {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

async fn attach_cmd(args: &[String]) -> R<ExitCode> {
    let flags = Flags::parse(args, &["-t", "--detach-key"], &["-d", "-r"])?;
    flags.no_positional()?;
    let key = detach_key(&flags)?;
    match flags.value("-t") {
        Some(target) => {
            let (place, row) = find_target(target).await?;
            attach_row(&place, &row, key).await
        }
        None => {
            let rows = local_rows().await?;
            let me = current_session();
            match pick_recent(&rows, read_recent().as_deref(), me.as_deref()).map_err(Fail::new)? {
                Some(row) => attach_local(&row, key).await,
                None => Err(Fail::new("no sessions on this machine; start one with `rmux new`")),
            }
        }
    }
}

async fn new(args: &[String]) -> R<ExitCode> {
    let flags = Flags::parse(args, &["-s", "-c", "-t", "--detach-key"], &["--claude", "--pi", "--codex", "-d"])?;
    flags.no_positional()?;
    let kind = match (flags.has("--claude"), flags.has("--pi"), flags.has("--codex")) {
        (true, false, false) => Kind::Claude,
        (false, true, false) => Kind::Pi,
        (false, false, true) => Kind::Codex,
        (false, false, false) => Kind::Terminal,
        _ => return Err(Fail::usage("--claude, --pi and --codex are one or another")),
    };
    let key = detach_key(&flags)?;
    let file = servers::load(&home()?);
    let place = match flags.value("-t") {
        Some(name) => server_for(name.strip_suffix(':').unwrap_or(name), &file)?,
        None => Where::Local,
    };
    let name = flags.value("-s").map(str::to_owned);
    let cwd = flags.value("-c").map(str::to_owned);
    match place {
        Where::Local => new_local(name, cwd, kind, flags.has("-d"), key).await,
        Where::Remote(server) => {
            // Forwarded to the far side's own `new`, which mints the name, sets
            // the alias and attaches there — one implementation, not two.
            let spec = key_spec(key);
            let mut remote: Vec<&str> = vec!["new", "--detach-key", &spec];
            if let Some(n) = &name {
                remote.extend(["-s", n]);
            }
            if let Some(c) = &cwd {
                remote.extend(["-c", c]);
            }
            match kind {
                Kind::Claude => remote.push("--claude"),
                Kind::Pi => remote.push("--pi"),
                Kind::Codex => remote.push("--codex"),
                Kind::Terminal => {}
            }
            let detached = flags.has("-d");
            if detached {
                remote.push("-d");
                let out = ssh_capture(&server, &servers::remote_line_new_cli(&remote), false).await?;
                print!("{out}");
                return Ok(ExitCode::SUCCESS);
            }
            ssh_exec(&server, &servers::remote_line_new_cli(&remote), true)
        }
    }
}

async fn new_local(
    name: Option<String>,
    cwd: Option<String>,
    kind: Kind,
    detached: bool,
    key: Option<DetachKey>,
) -> R<ExitCode> {
    let rows = local_rows().await?;
    if let Some(n) = &name {
        let n = n.trim();
        if n.is_empty() {
            return Err(Fail::usage("-s needs a name"));
        }
        // Checked before anything starts: a session that ran but could not be
        // named is a nameless shell the operator did not ask for.
        if rows.iter().any(|r| r.session == n || r.alias.as_deref() == Some(n)) {
            return Err(Fail::new(format!("duplicate session: {n}")));
        }
    }
    let cwd = match cwd {
        Some(dir) => bridge::expand_home(&dir)?,
        None => orchestrate::caller_cwd()?,
    };
    let cwd = if Path::new(&cwd).is_absolute() {
        cwd
    } else {
        std::env::current_dir().map(|d| d.join(&cwd).to_string_lossy().into_owned()).unwrap_or(cwd)
    };
    // Must exist: creating it would turn a typo into a directory tree.
    if !Path::new(&cwd).is_dir() {
        return Err(Fail::usage(format!("{cwd} is not a directory")));
    }

    let line = match kind {
        Kind::Claude => Some(bridge::launch_line_for(Agent::Claude, None, None, &[])),
        Kind::Pi => Some(bridge::launch_line_for(Agent::Pi, None, name.as_deref(), &[])),
        Kind::Codex => Some(rmux_claude_core::codex::launch_line(&Default::default(), None)),
        Kind::Terminal => None,
    };
    // The app's naming scheme, so the desktop rail recognises what this is.
    let session = orchestrate::child_name(kind);
    let (cols, rows_n) = crate::tty::window_size().unwrap_or((120, 40));
    // From inside a session, the new one inherits its credential set exactly as
    // `rmux spawn` does — see `inherit`.
    let parent_binding = match current_session() {
        Some(p) => attach::binding_anywhere(&p).await,
        None => None,
    };
    let plan = crate::inherit::plan(
        parent_binding.as_ref(),
        rmux_claude_core::env_kind::for_session(&session),
        None,
    )
    .map_err(Fail::new)?;
    let binding = plan.binding();
    if let Err(e) = attach::spawn_detached_bound(&session, &cwd, line.as_deref(), cols, rows_n, binding.clone()).await {
        return Err(Fail::new(match (&plan, parent_binding.as_ref().and_then(|r| r.binding.as_ref())) {
            (crate::inherit::Plan::Default(b), Some(p)) => crate::inherit::no_default(&p.kind, &b.kind),
            _ => e.to_string(),
        }));
    }

    let live: Vec<String> = attach::list().await?.into_iter().map(|s| s.name).collect();
    // Recorded like `spawn` records one, which is what gives `ls` (and the
    // app) the folder on hosts with no `/proc` to read it back from.
    crate::children::record(
        crate::children::Child {
            session: session.clone(),
            parent: current_session(),
            kind: kind.as_str().to_owned(),
            cwd: cwd.clone(),
            name: name.clone().map(|n| n.trim().to_owned()).unwrap_or_else(|| session.clone()),
            created_at: now_ms(),
            profile: binding.as_ref().map(|b| b.set.clone()),
            chain: binding.as_ref().and_then(|b| b.chain.as_ref()).map(|c| c.id.clone()),
        },
        &live,
    )?;
    let label = match &name {
        Some(n) => {
            alias::set(&session, n.trim(), &live)?;
            n.trim().to_owned()
        }
        None => session.clone(),
    };

    if detached {
        println!("{label}");
        return Ok(ExitCode::SUCCESS);
    }
    let row = Row {
        server: LOCAL.into(),
        session,
        alias: name.map(|n| n.trim().to_owned()),
        kind: kind.as_str().into(),
        status: "unknown".into(),
        cwd: Some(cwd),
        attached: false,
        age_seconds: 0,
    };
    attach_local(&row, key).await
}

async fn kill_session(args: &[String]) -> R<ExitCode> {
    let flags = Flags::parse(args, &["-t"], &[])?;
    flags.no_positional()?;
    let target = flags.value("-t").ok_or_else(|| Fail::usage("kill-session needs -t [SERVER:]SESSION"))?;
    let (place, row) = find_target(target).await?;
    match place {
        Where::Local => {
            // `Kill` is answered during the handshake by whichever build owns
            // the name, and ends the process — not merely the map entry.
            attach::kill(&row.session).await?;
            for _ in 0..30 {
                if !attach::list().await?.iter().any(|s| s.name == row.session) {
                    return Ok(ExitCode::SUCCESS);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(Fail::new(format!("{} did not exit after kill", row.display())))
        }
        Where::Remote(server) => {
            ssh_capture(&server, &servers::remote_line(&["kill", &row.session]), false).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn rename_session(args: &[String]) -> R<ExitCode> {
    let flags = Flags::parse(args, &["-t"], &[])?;
    let [new_name] = flags.positional.as_slice() else {
        return Err(Fail::usage("rename-session takes one NEWNAME"));
    };
    let (place, row) = match flags.value("-t") {
        Some(t) => find_target(t).await?,
        None => {
            let me = current_session()
                .ok_or_else(|| Fail::usage("rename-session needs -t outside an rmux session"))?;
            let rows = local_rows().await?;
            let row = rows.iter().find(|r| r.session == me).cloned().ok_or_else(|| Fail::new(format!("can't find session: {me}")))?;
            (Where::Local, row)
        }
    };
    match place {
        Where::Local => {
            let live: Vec<String> = attach::list().await?.into_iter().map(|s| s.name).collect();
            alias::set(&row.session, new_name, &live)?;
        }
        Where::Remote(server) => {
            ssh_capture(&server, &servers::remote_line(&["alias", &row.session, new_name]), false).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn has_session(args: &[String]) -> R<ExitCode> {
    let flags = Flags::parse(args, &["-t"], &[])?;
    flags.no_positional()?;
    let target = flags.value("-t").ok_or_else(|| Fail::usage("has-session needs -t [SERVER:]SESSION"))?;
    match find_target(target).await {
        Ok(_) => Ok(ExitCode::SUCCESS),
        // tmux prints why on stderr and exits 1; an unreachable server is also
        // "no" here, but says so.
        Err(e) if e.exit == 1 => {
            eprintln!("rmux: {}", e.message.lines().next().unwrap_or_default());
            Err(Fail::quiet())
        }
        Err(e) => Err(e),
    }
}

async fn detach_cmd(args: &[String]) -> R<ExitCode> {
    let flags = Flags::parse(args, &["-s", "-t"], &[])?;
    flags.no_positional()?;
    let session = match flags.value("-s").or(flags.value("-t")) {
        Some(t) => {
            let rows = local_rows().await?;
            resolve(&rows, t).map_err(Fail::new)?.session.clone()
        }
        None => current_session().ok_or_else(|| {
            Fail::usage("detach needs -s SESSION outside an rmux session (or press C-b d)")
        })?,
    };
    let signalled = signal_clients(&session)?;
    if signalled == 0 {
        return Err(Fail::new(format!(
            "no command-line client is attached to {session} (the rmux app's own views are never detached)"
        )));
    }
    Ok(ExitCode::SUCCESS)
}

/// SIGUSR1 every live CLI client of `session`; clear records of dead ones.
fn signal_clients(session: &str) -> R<usize> {
    let Some(dir) = clients_dir() else { return Ok(0) };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(0) };
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut count = 0;
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(owner) = std::fs::read_to_string(entry.path()) else { continue };
        if !status::pid_alive(pid) {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        if owner.trim() != session {
            continue;
        }
        #[cfg(unix)]
        {
            // `pid` came from a file name we wrote and parsed as a number; 0
            // and 1 would signal a process group or init.
            if pid > 1 && unsafe { libc::kill(pid as i32, libc::SIGUSR1) } == 0 {
                count += 1;
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(session: &str, alias: Option<&str>) -> Row {
        Row {
            server: LOCAL.into(),
            session: session.into(),
            alias: alias.map(str::to_owned),
            kind: Kind::of(session).as_str().into(),
            status: "idle".into(),
            cwd: Some("/srv".into()),
            attached: false,
            age_seconds: 90,
        }
    }

    #[test]
    fn targets_resolve_alias_then_name_then_unique_prefix() {
        let rows = vec![
            row("term-mwc9x1k0-3f9a2b", Some("demo")),
            row("term-mwc9x1k0-77aa", None),
            row("claude-session-abc", Some("term-mwc9x1k0-77aa-ish")),
        ];
        // An alias wins even over a name it happens to prefix.
        assert_eq!(resolve(&rows, "demo").unwrap().session, "term-mwc9x1k0-3f9a2b");
        // An exact daemon name.
        assert_eq!(resolve(&rows, "term-mwc9x1k0-77aa").unwrap().session, "term-mwc9x1k0-77aa");
        // Unique prefix, of a name or of an alias.
        assert_eq!(resolve(&rows, "claude").unwrap().session, "claude-session-abc");
        assert_eq!(resolve(&rows, "de").unwrap().session, "term-mwc9x1k0-3f9a2b");
        // Ambiguous: lists every match, never picks one.
        let err = resolve(&rows, "term-mwc").unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("local:demo (term-mwc9x1k0-3f9a2b)") && err.contains("local:term-mwc9x1k0-77aa"), "{err}");
        // Missing: names the candidates.
        let err = resolve(&rows, "nope").unwrap_err();
        assert!(err.starts_with("can't find session: nope") && err.contains("local:demo"), "{err}");
    }

    #[test]
    fn targets_split_on_a_known_server_or_a_verbatim_destination() {
        let file = ServersFile {
            version: 1,
            servers: vec![ServerEntry {
                id: "s1".into(),
                label: "dev@10.0.0.107".into(),
                host: "10.0.0.107".into(),
                user: Some("dev".into()),
                port: None,
                identity: None,
            }],
        };
        assert_eq!(split_target("demo", &file), (None, "demo".into()));
        assert_eq!(split_target("local:demo", &file), (Some("local".into()), "demo".into()));
        assert_eq!(split_target("dev@10.0.0.107:demo", &file), (Some("dev@10.0.0.107".into()), "demo".into()));
        assert_eq!(split_target("10.0.0.107:demo", &file), (Some("10.0.0.107".into()), "demo".into()));
        assert_eq!(split_target("me@box:2222:demo", &file), (Some("me@box:2222".into()), "demo".into()));
        assert_eq!(split_target("devbox:", &file), (Some("devbox".into()), String::new()));
    }

    #[test]
    fn remote_lists_parse_in_every_format_an_agent_prints() {
        // Six columns: with aliases.
        let six = "term-1\twebapp\t4242\t3700\tdetached\t\nclaude-session-x\t-\t-\t12\tattached\tclaude\n";
        let rows = rows_from_remote(six, "dev");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].alias.as_deref(), Some("webapp"));
        assert_eq!(rows[0].age_seconds, 3700);
        assert!(!rows[0].attached);
        assert_eq!(rows[1].kind, "claude");
        assert!(rows[1].attached && rows[1].alias.is_none());
        // Five columns: an agent from before aliases. The pid is not an alias.
        let five = "term-2\t99\t60\tattached\tbash\n";
        let rows = rows_from_remote(five, "dev");
        assert_eq!(rows[0].alias, None);
        assert_eq!(rows[0].age_seconds, 60);
        // JSON: a build with the orchestration CLI.
        let json = r#"{"sessions":[{"session":"pi-pisession-1","alias":"p","kind":"pi","status":"working","cwd":"/w","attached":false,"ageSeconds":5}]}"#;
        let rows = rows_from_remote(json, "dev");
        assert_eq!((rows[0].status.as_str(), rows[0].cwd.as_deref(), rows[0].display()), ("working", Some("/w"), "p"));
        assert!(rows_from_remote("", "dev").is_empty());
        assert!(rows_from_remote("usage: rmux-agent <…>\n", "dev").is_empty());
    }

    #[test]
    fn ls_lines_read_like_tmux() {
        let mut a = row("term-1", Some("demo"));
        a.attached = true;
        let mut b = row("claude-session-x", None);
        b.server = "dev".into();
        b.age_seconds = 2 * 86_400;
        b.cwd = None;
        let text = render(&[a, b]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "local:demo            terminal  idle  /srv  (attached)  1m");
        assert_eq!(lines[1], "dev:claude-session-x  claude    idle  -     (detached)  2d");
    }

    #[test]
    fn a_bare_attach_takes_the_recent_session_else_the_only_one() {
        let rows = vec![row("term-a", None), row("term-b", None)];
        assert_eq!(pick_recent(&rows, Some("term-b"), None).unwrap().unwrap().session, "term-b");
        // Recent one gone, several left: ask, listing them.
        let err = pick_recent(&rows, Some("term-gone"), None).unwrap_err();
        assert!(err.contains("local:term-a") && err.contains("local:term-b"), "{err}");
        // The session we are inside never counts.
        assert_eq!(pick_recent(&rows, Some("term-a"), Some("term-a")).unwrap().unwrap().session, "term-b");
        assert!(pick_recent(&[], None, None).unwrap().is_none());
    }

    #[test]
    fn the_cli_owns_tmux_words_and_leaves_the_app_primitives_alone() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(owns(None, &[]));
        assert!(owns(Some("ls"), &[]));
        assert!(owns(Some("attach"), &args(&["-t", "demo"])));
        assert!(!owns(Some("attach"), &args(&["--session", "term-1", "--cols", "80"])));
        assert!(!owns(Some("list"), &[]));
        assert!(!owns(Some("kill"), &args(&["term-1"])));
        assert!(!owns(Some("spawn"), &[]));
    }

    #[test]
    fn short_flags_take_attached_or_separate_values() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let f = Flags::parse(&args(&["-tdev:demo", "-d"]), &["-t"], &["-d"]).unwrap();
        assert_eq!(f.value("-t"), Some("dev:demo"));
        assert!(f.has("-d"));
        let f = Flags::parse(&args(&["--detach-key=C-a d"]), &["--detach-key"], &[]).unwrap();
        assert_eq!(f.value("--detach-key"), Some("C-a d"));
        assert_eq!(Flags::parse(&args(&["-x"]), &[], &[]).err().unwrap().exit, 2);
    }
}
