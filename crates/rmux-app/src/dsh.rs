//! DeepSeek Harness (`dsh`) as an rmux session.
//!
//! dsh ships no terminal UI: its interface is a Web UI served by `dsh web`. So a
//! dsh session is two halves that live in different places:
//!
//! - **The server runs on the target under `rmux-agent`**, as session
//!   `dsh-<id>`, exactly like a shell or a Claude run — so it survives quitting
//!   rmux, and closing the session kills it (`terminal_close` by name). It is
//!   started through the login shell (`--login-command`), because `node`/`npx`
//!   come from a version manager whose PATH exists only there.
//! - **The view is desktop-only** (`src-tauri/src/dsh.rs`): a loopback forward
//!   plus a webview navigated to it. That is why the two commands here are
//!   `Web::No` — rmux-web cannot reach the dev host's loopback, and its pane says
//!   so instead of drawing a broken frame.
//!
//! ## Where the URL comes from
//!
//! `dsh web` prints exactly one startup line, `dsh web: http://127.0.0.1:<port>/
//! ?token=<43 chars>` (measured against `@deepseek-ai/dsh` 0.2; its own e2e test
//! waits on the same regex). The token is a per-process launch credential that
//! the root `GET /?token=` exchanges for an authority-bound `HttpOnly` cookie, so
//! rmux reads it off the session's own output: the agent replays the daemon's
//! scrollback on attach, which carries the line back after an app restart. The
//! token is held **here, in memory**, and never handed to the UI — the desktop
//! side builds the URL itself.
//!
//! The port is chosen *on the host* by node (`listen(0)` on loopback), since node
//! is required anyway, and `--host 127.0.0.1` is passed explicitly even though
//! it is dsh's default: an agent UI on a LAN address is not a default to inherit.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_claude::ClaudeSession;
use rmux_term::{TermSize, TerminalEvent};
use serde::{Deserialize, Serialize};

use crate::claude::{ClaudeStore, StartedSession};
use crate::ctx::Ctx;
use crate::host::Sink;
use crate::registry::{Call, Registry, Web};
use crate::terminal::TargetRef;

/// The npm package `npx` runs when `dsh` is not installed.
pub const NPM_PACKAGE: &str = "@deepseek-ai/dsh";

/// dsh's `engines.node` (`apps/cli/package.json`: `>=24.9.0`).
pub const MIN_NODE: (u32, u32) = (24, 9);

/// Lines the launch script prints so the app can tell its failures apart.
const MARK: &str = "rmux-dsh:";

/// The POSIX script the session runs.
///
/// Every failure prints a `rmux-dsh:` marker **and** a human sentence, because
/// the console is shown while starting: the operator reads the sentence, the
/// app reads the marker. Nothing here is interpolated from outside — the script
/// is a constant — so there is nothing to quote.
pub fn launch_script() -> String {
    let (major, minor) = MIN_NODE;
    format!(
        r#"printf '%s\n' '{MARK} starting DeepSeek Harness'
if ! command -v node >/dev/null 2>&1; then
  printf '%s\n' '{MARK} no-node' 'DeepSeek Harness needs Node.js {major}.{minor} or newer on this machine, and none is on the login PATH.' 'Install it (for example: nvm install {major}), then restart this session.'
  exit 127
fi
if ! node -e 'const [a,b]=process.versions.node.split(".").map(Number);process.exit(a>{major}||(a==={major}&&b>={minor})?0:1)'; then
  printf '{MARK} old-node %s\n' "$(node --version)"
  printf '%s\n' 'DeepSeek Harness needs Node.js {major}.{minor} or newer. Upgrade (for example: nvm install {major}), then restart this session.'
  exit 1
fi
P=$(node -e 'const s=require("net").createServer();s.listen(0,"127.0.0.1",()=>{{process.stdout.write(String(s.address().port));s.close()}})') || {{ printf '%s\n' '{MARK} no-port'; exit 1; }}
if command -v dsh >/dev/null 2>&1; then
  dsh web --no-open --host 127.0.0.1 --port "$P"
else
  printf '%s\n' '{MARK} fetching' 'dsh is not installed here; running it with npx (the first run downloads {NPM_PACKAGE}).'
  npx -y {NPM_PACKAGE} web --no-open --host 127.0.0.1 --port "$P"
fi
printf '{MARK} exited %s\n' "$?"
"#
    )
}

/// The `--login-command` value: the script under `sh`, whatever the login
/// shell is. The login shell still runs first (`-l -i`), so PATH is its PATH;
/// `sh` only guarantees the script is parsed as POSIX on a fish or zsh host.
pub fn launch_line() -> String {
    format!("sh -c {}", rmux_transport::shell_quote(&launch_script()))
}

/// Where the Web UI is listening on the target, and its launch token.
///
/// Deliberately not `Serialize`: the token is a credential and the UI never
/// needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

/// Find the newest `dsh web: <url>` startup line in `text`.
///
/// Tolerant of what a pty adds — `\r`, colour, a line split across chunks
/// (callers feed a rolling tail) — and strict about what it accepts: the URL
/// must be `http://<loopback>:<port>/?token=<base64url>`. A non-loopback host
/// is refused, because forwarding to it would mean the server was listening on
/// something other than what rmux asked for.
pub fn parse_endpoint(text: &str) -> Option<Endpoint> {
    const PREFIX: &str = "dsh web: ";
    let mut found = None;
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        let after = &rest[at + PREFIX.len()..];
        let url: String = after
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '\u{1b}')
            .collect();
        if let Some(endpoint) = parse_url(&url) {
            found = Some(endpoint);
        }
        rest = after;
    }
    found
}

fn parse_url(url: &str) -> Option<Endpoint> {
    let rest = url.strip_prefix("http://")?;
    let (authority, tail) = rest.split_once('/')?;
    let (host, port) = authority.rsplit_once(':')?;
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return None;
    }
    let port: u16 = port.parse().ok().filter(|p| *p != 0)?;
    let query = tail.split_once('?').map(|(_, q)| q)?;
    let token = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="))
        .filter(|t| t.len() >= 16 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))?;
    Some(Endpoint { port, token: token.to_owned() })
}

/// Why the server is not (or no longer) usable, read from the script's markers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    NoNode,
    OldNode(String),
    NoPort,
    Exited(Option<i32>),
}

impl Problem {
    pub fn message(&self) -> String {
        let (major, minor) = MIN_NODE;
        match self {
            Problem::NoNode => format!(
                "Node.js was not found on this machine's login PATH. DeepSeek Harness needs Node.js \
                 {major}.{minor} or newer — install it (for example `nvm install {major}`) and restart \
                 this session."
            ),
            Problem::OldNode(v) => format!(
                "Node.js {v} is too old: DeepSeek Harness needs {major}.{minor} or newer. Upgrade (for \
                 example `nvm install {major}`) and restart this session."
            ),
            Problem::NoPort => "node could not reserve a loopback port on this machine.".to_owned(),
            Problem::Exited(Some(code)) => {
                format!("dsh exited (status {code}). The console shows its last output.")
            }
            Problem::Exited(None) => "dsh exited. The console shows its last output.".to_owned(),
        }
    }
}

/// The newest marker in `text`, if any. `starting`/`fetching` are progress, not
/// problems, and a later `starting` clears an earlier failure (a restart).
pub fn parse_problem(text: &str) -> Option<Problem> {
    let mut problem = None;
    for line in text.split(['\n', '\r']) {
        let Some(at) = line.find(MARK) else { continue };
        let mut words = line[at + MARK.len()..].split_whitespace();
        match words.next() {
            Some("starting") => problem = None,
            Some("no-node") => problem = Some(Problem::NoNode),
            Some("old-node") => problem = Some(Problem::OldNode(words.next().unwrap_or("?").to_owned())),
            Some("no-port") => problem = Some(Problem::NoPort),
            Some("exited") => problem = Some(Problem::Exited(words.next().and_then(|c| c.parse().ok()))),
            _ => {}
        }
    }
    problem
}

/// What one dsh session's output has told us so far.
#[derive(Default)]
struct Watch {
    /// A rolling tail, so a line split across two chunks still parses.
    tail: String,
    endpoint: Option<Endpoint>,
    problem: Option<Problem>,
    fetching: bool,
    exited: bool,
}

const TAIL: usize = 16 * 1024;

impl Watch {
    fn feed(&mut self, bytes: &[u8]) {
        self.tail.push_str(&String::from_utf8_lossy(bytes));
        if let Some(endpoint) = parse_endpoint(&self.tail) {
            self.endpoint = Some(endpoint);
        }
        if self.tail.contains("rmux-dsh: fetching") {
            self.fetching = true;
        }
        if let Some(problem) = parse_problem(&self.tail) {
            self.problem = Some(problem);
        }
        if self.tail.len() > TAIL {
            let mut cut = self.tail.len() - TAIL / 2;
            while !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.tail.drain(..cut);
        }
    }
}

/// Every dsh session's watch, keyed by the attach id (`ClaudeStore` id).
#[derive(Default)]
pub struct DshStore {
    watches: Mutex<HashMap<String, Arc<Mutex<Watch>>>>,
}

impl DshStore {
    /// The parsed endpoint for an attach id — the desktop view's only input.
    pub fn endpoint(&self, id: &str) -> Option<Endpoint> {
        let watch = self.watches.lock().get(id).cloned()?;
        let w = watch.lock();
        // A server that has since exited is not an endpoint to forward to.
        if w.exited || matches!(w.problem, Some(Problem::Exited(_))) {
            return None;
        }
        w.endpoint.clone()
    }

    fn watch(&self, id: &str) -> Arc<Mutex<Watch>> {
        Arc::clone(self.watches.lock().entry(id.to_owned()).or_default())
    }

    pub fn forget(&self, id: &str) {
        self.watches.lock().remove(id);
    }
}

/// Read the session's output for as long as it runs. Independent of any view:
/// the endpoint is known whether or not a pane is mounted.
fn watch_output(session: &ClaudeSession, watch: Arc<Mutex<Watch>>) {
    let (backlog, mut receiver) = session.terminal().attach();
    watch.lock().feed(&backlog);
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(TerminalEvent::Output(chunk)) => watch.lock().feed(&chunk),
                Ok(TerminalEvent::Exited { .. }) => {
                    watch.lock().exited = true;
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    watch.lock().exited = true;
                    break;
                }
                Err(_) => {}
            }
        }
    });
}

/// Start (or reattach to) the dsh server for `session_name` and stream its
/// console. Reattaching is the agent's job: an existing `dsh-<id>` is joined,
/// and its scrollback replay carries the startup line back to [`Watch`].
#[allow(clippy::too_many_arguments)]
pub async fn dsh_start(
    ctx: &Ctx,
    store: &ClaudeStore,
    dsh: &DshStore,
    target: TargetRef,
    cwd: Option<String>,
    session_name: String,
    model_profile: Option<String>,
    cols: u16,
    rows: u16,
    output: Arc<dyn Sink>,
) -> Result<StartedSession, String> {
    let resolved = crate::claude::resolve(store, &target).await?;
    if resolved.platform() == Some(rmux_transport::Platform::Windows) {
        return Err(
            "DeepSeek Harness runs under the rmux agent, which is not available on Windows hosts \
             yet — start it on a macOS or Linux host"
                .to_owned(),
        );
    }
    // Credentials first, scoped to `deepseek`, and before the attach: the daemon
    // applies a kind's set when it *creates* the session, so a profile chosen
    // after the server is up takes effect on its next start. Applying with no
    // profile still sends the clears — selecting nothing is an instruction to
    // undo, not an absence (CLAUDE.md, model profiles).
    let applied = crate::model_profile::apply_to_target(
        ctx,
        resolved.as_ref(),
        rmux_claude::profile::ProfileKind::Deepseek,
        model_profile.as_deref(),
    )
    .await?;

    let installed = crate::agent::ensure_agent(ctx, resolved.as_ref()).await?;

    let spec = installed
        .attach_spec(&session_name, cwd.as_deref(), cols, rows)
        .arg("--login-command")
        .arg(launch_line())
        .args(applied.attach_args);
    let session = Arc::new(
        ClaudeSession::start_with_spec(resolved.as_ref(), spec, cwd.as_deref(), TermSize { cols, rows })
            .map_err(|e| e.to_string())?,
    );

    let id = crate::claude::insert_session(store, Arc::clone(&session));
    watch_output(&session, dsh.watch(&id));
    crate::claude::stream_claude(session, output);
    Ok(StartedSession { id })
}

/// What the pane needs to decide what to draw. No token, ever.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DshState {
    /// `starting` · `fetching` (npx is downloading dsh) · `ready` · `failed`.
    pub phase: &'static str,
    /// The port on the *target*, once known — shown, not secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

pub fn dsh_state(store: &ClaudeStore, dsh: &DshStore, id: &str) -> Result<DshState, String> {
    // Unknown id: the attach process is gone (app restarted, stale handle). The
    // pane falls back to `dsh_start`, which reattaches by name.
    crate::claude::session_by_id(store, id)?;
    let watch = dsh.watch(id);
    let w = watch.lock();
    Ok(state_of(&w))
}

fn state_of(w: &Watch) -> DshState {
    let remote_port = w.endpoint.as_ref().map(|e| e.port);
    if let Some(problem) = &w.problem {
        return DshState { phase: "failed", remote_port, problem: Some(problem.message()) };
    }
    if w.exited {
        return DshState { phase: "failed", remote_port, problem: Some(Problem::Exited(None).message()) };
    }
    match (&w.endpoint, w.fetching) {
        (Some(_), _) => DshState { phase: "ready", remote_port, problem: None },
        (None, true) => DshState { phase: "fetching", remote_port, problem: None },
        (None, false) => DshState { phase: "starting", remote_port, problem: None },
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshStartArgs {
    pub target: TargetRef,
    #[serde(default)]
    pub cwd: Option<String>,
    pub session_name: String,
    #[serde(default)]
    pub model_profile: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub output: String,
}

#[derive(Deserialize)]
pub struct DshIdArgs {
    pub id: String,
}

pub fn register(r: &mut Registry) {
    r.add("dsh_start", Web::No, |call: Call, a: DshStartArgs| async move {
        let output = call.sink(&a.output).map_err(|e| e.to_string())?;
        let ctx = call.ctx();
        dsh_start(
            ctx,
            &ctx.store::<ClaudeStore>(),
            &ctx.store::<DshStore>(),
            a.target,
            a.cwd,
            a.session_name,
            a.model_profile,
            a.cols,
            a.rows,
            output,
        )
        .await
    });
    r.add("dsh_state", Web::No, |call: Call, a: DshIdArgs| async move {
        let ctx = call.ctx();
        dsh_state(&ctx.store::<ClaudeStore>(), &ctx.store::<DshStore>(), &a.id)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a real `npx -y @deepseek-ai/dsh web --no-open --port 3987`
    /// on macOS (node 24.20), token replaced by one of the same shape.
    const REAL_OUTPUT: &str = "npm warn deprecated node-domexception@1.0.0: Use your platform's native DOMException instead\r\n\
dsh web: http://127.0.0.1:3987/?token=Qm9vdHN0cmFwVG9rZW5Gb3JUZXN0aW5nT25seTAxMjM\r\n";

    #[test]
    fn the_real_startup_line_yields_port_and_token() {
        let e = parse_endpoint(REAL_OUTPUT).expect("endpoint");
        assert_eq!(e.port, 3987);
        assert_eq!(e.token, "Qm9vdHN0cmFwVG9rZW5Gb3JUZXN0aW5nT25seTAxMjM");
        assert_eq!(e.token.len(), 43, "dsh mints 43-char base64url tokens");
    }

    #[test]
    fn a_line_split_across_chunks_still_parses() {
        let mut w = Watch::default();
        let (a, b) = REAL_OUTPUT.split_at(REAL_OUTPUT.find("?tok").unwrap());
        w.feed(a.as_bytes());
        assert!(w.endpoint.is_none());
        w.feed(b.as_bytes());
        assert_eq!(w.endpoint.as_ref().map(|e| e.port), Some(3987));
        assert_eq!(state_of(&w).phase, "ready");
    }

    #[test]
    fn colour_and_restarts_take_the_newest_line() {
        let text = "\x1b[32mdsh web: http://127.0.0.1:41000/?token=AAAAAAAAAAAAAAAAAAAAAA\x1b[0m\r\n\
                    rmux-dsh: exited 0\r\nrmux-dsh: starting DeepSeek Harness\r\n\
                    dsh web: http://127.0.0.1:41001/?token=BBBBBBBBBBBBBBBBBBBBBB\r\n";
        let e = parse_endpoint(text).unwrap();
        assert_eq!((e.port, e.token.as_str()), (41001, "BBBBBBBBBBBBBBBBBBBBBB"));
        // A later `starting` clears the earlier exit.
        assert_eq!(parse_problem(text), None);
    }

    #[test]
    fn a_non_loopback_or_tokenless_url_is_refused() {
        assert_eq!(parse_endpoint("dsh web: http://10.0.0.5:3080/?token=AAAAAAAAAAAAAAAAAAAAAA"), None);
        assert_eq!(parse_endpoint("dsh web: http://127.0.0.1:3080/"), None);
        assert_eq!(parse_endpoint("dsh web: http://127.0.0.1:3080/?token=short"), None);
        assert_eq!(parse_endpoint("dsh web: https://127.0.0.1:3080/?token=AAAAAAAAAAAAAAAAAAAAAA"), None);
        assert_eq!(parse_endpoint("dsh web: http://127.0.0.1:0/?token=AAAAAAAAAAAAAAAAAAAAAA"), None);
    }

    #[test]
    fn script_markers_become_problems_with_reasons() {
        assert_eq!(parse_problem("rmux-dsh: starting\r\nrmux-dsh: no-node\r\n"), Some(Problem::NoNode));
        assert_eq!(
            parse_problem("rmux-dsh: old-node v22.16.0\n"),
            Some(Problem::OldNode("v22.16.0".into()))
        );
        assert_eq!(parse_problem("rmux-dsh: exited 130\n"), Some(Problem::Exited(Some(130))));
        assert!(Problem::NoNode.message().contains("24.9"));

        let mut w = Watch::default();
        w.feed(b"rmux-dsh: starting DeepSeek Harness\r\nrmux-dsh: fetching\r\n");
        assert_eq!(state_of(&w).phase, "fetching");
        w.feed(b"rmux-dsh: no-node\r\n");
        let s = state_of(&w);
        assert_eq!(s.phase, "failed");
        assert!(s.problem.unwrap().contains("Node.js"));
    }

    #[test]
    fn the_launch_binds_loopback_and_never_opens_a_browser() {
        let script = launch_script();
        assert!(script.contains("web --no-open --host 127.0.0.1 --port \"$P\""), "{script}");
        assert!(script.contains("npx -y @deepseek-ai/dsh web --no-open --host 127.0.0.1"), "{script}");
        assert!(script.contains("command -v dsh"), "{script}");
        // The line is one sh -c with the whole script quoted: the login shell
        // (zsh, fish) never parses the script itself.
        let line = launch_line();
        assert!(line.starts_with("sh -c '"), "{line}");
    }

    #[test]
    fn the_launch_script_is_valid_posix_sh() {
        // `sh -n` parses without running.
        let out = std::process::Command::new("sh").arg("-n").arg("-c").arg(launch_script()).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn the_tail_stays_bounded_and_keeps_the_endpoint() {
        let mut w = Watch::default();
        w.feed(REAL_OUTPUT.as_bytes());
        for _ in 0..200 {
            w.feed(&[b'x'; 1024]);
        }
        assert!(w.tail.len() <= TAIL);
        assert_eq!(w.endpoint.as_ref().map(|e| e.port), Some(3987));
    }
}
