//! Terminal IPC.
//!
//! Output crosses to the webview over a **raw** channel
//! ([`Sink::send_bytes`] — `InvokeResponseBody::Raw` on desktop), not as JSON. That matters: Tauri's default
//! serialisation turns a `Vec<u8>` into a JSON array of decimal numbers, roughly
//! a 4x size increase plus parse cost on every chunk — on the hottest path in the
//! app, where a `cat` of a large file can push megabytes through in a second.
//!
//! Terminals outlive the views that show them. Closing a tab drops the
//! subscription; the PTY keeps running and a later `terminal_attach` catches up
//! from the scrollback.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_ssh::SshTarget;
use rmux_term::{TermSize, Terminal, TerminalEvent};
use rmux_ssh::MoshInstallOutcome;
use rmux_transport::{
    CommandSpec, LocalTarget, LowLatencyOutcome, Target, TargetId,
    local::terminal_env,
};
use serde::{Deserialize, Serialize};

use crate::ctx::Ctx;
use crate::host::Sink;
use crate::registry::{Call, Registry, Web};

/// Live terminals and the targets they run on.
#[derive(Default)]
pub struct TerminalStore {
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    /// Held so an SSH target's ControlMaster stays up for the life of the app.
    /// Dropping it would tear down the multiplexed connection and make the next
    /// terminal on that host re-authenticate.
    targets: Mutex<HashMap<TargetId, Arc<dyn Target>>>,
}

impl TerminalStore {
    /// The cached target for `id`, if this store has one.
    ///
    /// Taken before a disconnect evicts it: once the caches are dropped there is
    /// nothing left to ask to close its connection.
    pub fn cached_target(&self, id: &TargetId) -> Option<Arc<dyn Target>> {
        self.targets.lock().get(id).cloned()
    }

    /// Forget this target's cached handle.
    ///
    /// Returns whether there was one. Dropping rmux's handle is only half of a
    /// disconnect — the transport is closed by the caller, because a cache that
    /// merely forgets leaves the connection up if any other clone survives.
    pub fn evict_target(&self, id: &TargetId) -> bool {
        self.targets.lock().remove(id).is_some()
    }

    /// Insert a resolved handle directly. Tests only: the real path needs a
    /// live host, which a unit test must not require.
    pub fn insert_for_test(&self, id: TargetId, value: Arc<dyn Target>) {
        self.targets.lock().insert(id, value);
    }
}

/// Which machine to open a terminal on. Defined in [`crate::target`];
/// re-exported so `crate::terminal::TargetRef` names the same type everywhere.
pub use crate::target::TargetRef;

/// Terminal lifecycle, sent as JSON on a separate channel from the byte stream.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Lifecycle {
    Exited { code: i32 },
    /// The view fell too far behind and output was dropped. Surfaced rather than
    /// hidden: the terminal contents are now missing bytes, so the display is
    /// unreliable and the user should be told instead of quietly shown a
    /// corrupted screen.
    Lagged { chunks: u64 },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedTerminal {
    pub id: String,
    pub cols: u16,
    pub rows: u16,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Resolve (and cache) a target, bringing up SSH multiplexing on first use.
async fn resolve_target(
    store: &TerminalStore,
    target: &TargetRef,
) -> Result<Arc<dyn Target>, String> {
    let id = target.id();

    if let Some(existing) = store.targets.lock().get(&id) {
        return Ok(Arc::clone(existing));
    }

    let resolved: Arc<dyn Target> = match &id {
        TargetId::Local => Arc::new(LocalTarget::new()),
        TargetId::Ssh(host) => {
            let ssh = SshTarget::new(host.clone());
            // Establish the master connection now so any credential prompt
            // happens here — once — rather than separately for every terminal.
            ssh.connect().await.map_err(err)?;
            Arc::new(ssh)
        }
    };

    store.targets.lock().insert(id, Arc::clone(&resolved));
    Ok(resolved)
}

/// What a low-latency request would get on this connection, and why.
///
/// Asked by the TRANSPORT section of Session Settings *when it is opened*, so a
/// control that cannot work is **absent** rather than present and dead. It is
/// on demand and never a poller: the answer is cached per host for the run, so
/// reopening the section costs nothing and a host is asked at most once.
///
/// Resolving the target first is deliberate — the probe runs over the
/// ControlMaster that is already open, so it costs no handshake of its own.
pub async fn low_latency_probe(
    store: &TerminalStore,
    target: TargetRef,
) -> Result<LowLatencyOutcome, String> {
    let resolved = resolve_target(store, &target).await?;
    Ok(resolved.probe_low_latency().await)
}

/// What rmux's attempt to install — or **upgrade** — `mosh-server` on this host
/// came to.
///
/// **Asked once, right after `low_latency_probe`.** That probe is what starts an
/// attempt, so by the time this is called there is either something to await or
/// nothing was started. Both questions come back through one call because they
/// are one mechanism and the panel has one place to report them; when a host has
/// settled answers to both, the upgrade is the later fact and the one reported.
///
/// It *awaits an attempt that is still running* rather than being polled. One
/// call, resolving when there is something to say: a poller on a package
/// install would be a request every second or two for minutes, for a fact that
/// changes exactly once. `None` means no attempt was made — the switch is off,
/// the host is local, or nothing probed it — and the caller must then say
/// nothing rather than invent a state.
///
/// Nothing here resolves or connects a target: it reads a decision another task
/// is making, so a host that is slow to install cannot make this slow to answer
/// for any *other* reason.
pub async fn mosh_install_status(target: TargetRef) -> Result<Option<MoshInstallOutcome>, String> {
    match target.id() {
        TargetId::Local => Ok(None),
        TargetId::Ssh(host) => Ok(rmux_ssh::mosh_install::status(&host).await),
    }
}

/// What version of `mosh-server` this host has, and whether rmux considers it
/// too old.
///
/// **The comparison is Rust's, not the panel's.** The minimum lives in one place
/// (`rmux_ssh::MOSH_MIN_SERVER_VERSION`) and a second version parser in the
/// webview would be a second chance to get `1.10` versus `1.9` backwards. The
/// number is handed over as well, because the sentence the operator reads has to
/// name it.
///
/// Reads the cache the low-latency probe filled and never probes anything, so
/// this is only meaningful after `low_latency_probe` — which is exactly when the
/// panel asks. `version: null` covers both "not probed" and "present but its
/// version could not be read", and `outdated` is false for both: guessing there
/// would mean upgrading packages on someone's server because a string changed
/// format.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoshServerStatus {
    version: Option<String>,
    outdated: bool,
    minimum: String,
}

pub fn mosh_server_status(target: TargetRef) -> MoshServerStatus {
    let version = match target.id() {
        TargetId::Local => None,
        TargetId::Ssh(host) => rmux_ssh::mosh::cached_server_version(&host),
    };
    MoshServerStatus {
        version: version.map(|v| v.to_string()),
        outdated: rmux_ssh::mosh::is_outdated(version),
        minimum: rmux_ssh::MOSH_MIN_SERVER_VERSION.to_string(),
    }
}

/// Is rmux changing this host's `mosh-server` right now?
///
/// Answers immediately and never waits — which is the whole reason it is
/// separate from `mosh_install_status`, whose promise stays pending for as long
/// as the attempt runs. The panel asks this first, so it draws an in-flight
/// state only where there is genuinely something in flight rather than assuming
/// one and taking it back a moment later.
pub fn mosh_install_pending(target: TargetRef) -> bool {
    match target.id() {
        TargetId::Local => false,
        TargetId::Ssh(host) => rmux_ssh::mosh_install::pending(&host),
    }
}

/// Whether rmux may install `mosh-server` on a host that has not got one.
///
/// Default **on** — that is what was asked for — and mirrored here from the
/// webview's stored preference at startup and on every change, exactly as the
/// benchmark-logging gate is. Rust holds the live gate because the decision is
/// made in a background task the webview never sees.
///
/// Off is off, for an upgrade exactly as for an install — the setting means
/// "rmux may change this host", and replacing a package on it is squarely that.
/// No detection, no `sudo`, nothing written to `~/.rmux/mosh-install.json`.
pub fn set_mosh_auto_install(on: bool) {
    rmux_ssh::mosh_install::set_enabled(on);
    tracing::info!("automatic mosh-server install {}", if on { "enabled" } else { "disabled" });
}

/// Open a terminal and start streaming its output.
// An IPC entry point: every argument is a distinct thing the webview must send,
// and bundling them into a struct would only move the same list behind a name.
#[allow(clippy::too_many_arguments)]
pub async fn terminal_open(
    ctx: &Ctx,
    store: &TerminalStore,
    target: TargetRef,
    cwd: Option<String>,
    // `session` is a stable name for the shell on the target. The same name
    // always reattaches to the same shell, which is what survives a restart.
    session: Option<String>,
    // Ask the target to minimise echo latency for this shell — the per-session
    // TRANSPORT choice. A *request*, never a protocol: what it becomes is the
    // target's decision, and `None`/`false` is the ordinary ssh path. Kept with
    // the session for the same reason the model profile is, so a pane that
    // restarts comes back on the transport it was launched with.
    low_latency: Option<bool>,
    // A named program to run in place of the shell — a closed set, never a
    // free-form command (see [`Run`]).
    run: Option<Run>,
    cols: u16,
    rows: u16,
    output: Arc<dyn Sink>,
    lifecycle: Arc<dyn Sink>,
) -> Result<OpenedTerminal, String> {
    let resolved = resolve_target(store, &target).await?;
    let run_line = run.map(Run::line);

    // A named terminal runs under the agent so it outlives this connection.
    // Without a name there is nothing to reattach *to*, so a plain login shell
    // is the honest behaviour rather than a session nobody can find again.
    let plain_shell = || {
        let mut spec = match run_line {
            // Through the login shell for the same reason the agent path uses
            // one: `codex` is on a version manager's PATH that exists only there.
            Some(line) => CommandSpec::login_shell().arg("-c").arg(line),
            None => CommandSpec::login_shell(),
        };
        if let Some(cwd) = &cwd {
            spec = spec.cwd(cwd.clone());
        }
        spec
    };

    let mut spec = match &session {
        // **A host without an agent still gets a terminal.** Windows cannot run
        // the agent (see `ensure_agent`), and refusing outright would mean no
        // shell at all on a host where everything else — files, search, Claude —
        // works. What is lost is persistence, so the shell ends with the
        // connection; that is worth saying, but it is not worth withholding the
        // terminal over.
        Some(name) => match crate::agent::ensure_agent(ctx, resolved.as_ref()).await {
            Ok(installed) => {
                let spec = installed.attach_spec(name, cwd.as_deref(), cols, rows);
                match run_line {
                    Some(line) => spec.arg("--login-command").arg(line),
                    None => spec,
                }
            }
            Err(reason) => {
                tracing::info!(%reason, "opening a non-persistent terminal");
                plain_shell()
            }
        },
        None => plain_shell(),
    };

    for (key, value) in terminal_env() {
        spec = spec.env(key, value);
    }

    // **The probe has to happen before the command is built.** `build_command`
    // is synchronous and reads a cache the probe fills; an unprobed host is not
    // a refusal, it is simply unknown, so setting the flag without awaiting
    // this would fall back to ssh silently and for ever. Cached per host, so
    // this costs one round trip once per run and only for an operator who asked.
    if low_latency.unwrap_or(false) {
        resolved.probe_low_latency().await;
        spec = spec.low_latency(true);
    }

    // For SSH this becomes `ssh -tt host -- <agent> attach --session ...`,
    // spawned in a LOCAL pty. The terminal never learns whether it is remote.
    let command = resolved.build_command(&spec).map_err(err)?;

    let size = TermSize { cols, rows };
    // `cwd` is applied by the target: locally as the PTY's working directory,
    // remotely as a `cd` in the shell line. Passing it here too would make a
    // remote path be interpreted against the local filesystem.
    //
    // Under the agent it is passed as `--cwd` instead and applied by the daemon
    // when the session is created, so it must not also be set here — a reattach
    // would otherwise drag the shell back to the project root.
    let local_cwd = (session.is_none() && matches!(target.id(), TargetId::Local))
        .then(|| cwd.as_deref().map(camino::Utf8Path::new))
        .flatten();

    let terminal = Arc::new(Terminal::spawn(&command, local_cwd, size).map_err(err)?);
    let id = terminal.id().to_owned();

    store.terminals.lock().insert(id.clone(), Arc::clone(&terminal));
    stream_to_channel(Arc::clone(&terminal), output, lifecycle);

    Ok(OpenedTerminal { id, cols, rows })
}

/// Reattach a view to a terminal that is already running, catching up first.
pub async fn terminal_attach(
    store: &TerminalStore,
    id: String,
    output: Arc<dyn Sink>,
    lifecycle: Arc<dyn Sink>,
) -> Result<(), String> {
    let terminal = store
        .terminals
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no such terminal: {id}"))?;

    stream_to_channel(terminal, output, lifecycle);
    Ok(())
}

/// Send input.
pub async fn terminal_write(
    store: &TerminalStore,
    id: String,
    data: String,
) -> Result<(), String> {
    let terminal = store
        .terminals
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no such terminal: {id}"))?;

    terminal.write(data.as_bytes()).map_err(err)
}

/// Report a new size, so full-screen programs redraw correctly.
pub async fn terminal_resize(
    store: &TerminalStore,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let terminal = store
        .terminals
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no such terminal: {id}"))?;

    terminal.resize(TermSize { cols, rows }).map_err(err)
}

/// Drop the local view of a terminal and **leave the shell running**.
///
/// This is `terminal_close` without the `agent kill` — and that one omission is
/// the entire difference between the two. Killing the local attach client only
/// ever detaches; the shell lives in the daemon on the target, which is what
/// makes a session survive quitting the app, losing the network, or closing the
/// lid.
///
/// `terminal_close` follows the kill because closing a tab means the operator is
/// finished with it, and without that every closed tab leaks a shell nothing can
/// reach again. Detaching says the opposite: keep it, I am coming back. The way
/// back is HOST ▸ SESSIONS, which lists exactly these.
///
/// Deliberately takes no target and no session name: there is nothing to say to
/// the far side. That asymmetry with `terminal_close` is the safety property —
/// this command has no path that can end a shell.
pub async fn terminal_detach(store: &TerminalStore, id: String) -> Result<(), String> {
    if let Some(terminal) = store.terminals.lock().remove(&id) {
        // Already exited is success, not an error: the operator asked to stop
        // watching it, and it has stopped.
        let _ = terminal.kill();
    }
    Ok(())
}

/// Kill a terminal and forget it — on this machine *and* on the target.
pub async fn terminal_close(
    ctx: &Ctx,
    store: &TerminalStore,
    id: String,
    target: Option<TargetRef>,
    session: Option<String>,
) -> Result<(), String> {
    let terminal = store.terminals.lock().remove(&id);
    if let Some(terminal) = terminal {
        // A child that has already exited cannot be killed; that is success here,
        // not an error to report.
        let _ = terminal.kill();
    }

    // Killing the local attach client only detaches. The shell lives in the
    // daemon on the target, and closing a tab means the operator is finished
    // with it — without this, every closed tab leaks a shell that nothing can
    // ever reach again.
    if let (Some(target), Some(session)) = (target, session)
        && let Ok(resolved) = resolve_target(store, &target).await
        && let Ok(installed) = crate::agent::ensure_agent(ctx, resolved.as_ref()).await
    {
        {
            {
                let spec = CommandSpec::new(&installed.program)
                    .arg("kill")
                    .arg(&session)
                    .tty(rmux_transport::Tty::None);
                // Best effort: a daemon that is already gone took the shell with
                // it, which is the outcome being asked for anyway.
                let _ = resolved.exec(&spec).await;
            }
        }
    }

    Ok(())
}

/// Pump a terminal's output into the webview.
fn stream_to_channel(terminal: Arc<Terminal>, output: Arc<dyn Sink>, lifecycle: Arc<dyn Sink>) {
    // Catch up and subscribe atomically, so the handover neither drops nor
    // duplicates a chunk. See `Terminal::attach`.
    let (backlog, mut receiver) = terminal.attach();

    if !backlog.is_empty() {
        let _ = output.send_bytes(backlog.to_vec());
    }

    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(TerminalEvent::Output(chunk)) => {
                    // A send failure means the webview went away (reload, window
                    // closed). Stop forwarding, but leave the PTY running — the
                    // session outlives its view by design.
                    if output.send_bytes(chunk.to_vec()).is_err() {
                        break;
                    }
                }
                Ok(TerminalEvent::Exited { code }) => {
                    let _ = send_lifecycle(lifecycle.as_ref(), &Lifecycle::Exited { code });
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(chunks)) => {
                    // Report rather than swallow: the screen is now missing bytes.
                    tracing::warn!(chunks, "terminal view fell behind; output dropped");
                    let _ = send_lifecycle(lifecycle.as_ref(), &Lifecycle::Lagged { chunks });
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// A lifecycle message, as the JSON a typed `Channel<Lifecycle>` sent.
fn send_lifecycle(sink: &dyn Sink, message: &Lifecycle) -> anyhow::Result<()> {
    sink.send_json(serde_json::to_value(message)?)
}

// ── Registration ─────────────────────────────────────────────────────────────
//
// Argument objects, keyed exactly as `ui/src/components/Terminal.tsx`,
// `ui/src/lib/workspace.ts` and `ui/src/lib/api.ts` send them. Channels arrive
// as their `"__CHANNEL__:<n>"` id and are resolved on the invoking UI.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetArgs {
    pub target: TargetRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetMoshAutoInstallArgs {
    pub on: bool,
}

/// A program a terminal may run instead of a shell.
///
/// **A closed set, on purpose.** The UI asks for a *purpose* and the line is
/// built here from the one definition of it, so the webview never hands a
/// command string to a remote shell. The first member exists so signing in to
/// Codex is a real terminal running the real CLI — the operator answers its
/// device-code prompt themselves, and rmux scripts none of the browser flow.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Run {
    /// `codex login --device-auth`.
    CodexLogin,
}

impl Run {
    pub fn line(self) -> &'static str {
        match self {
            Run::CodexLogin => rmux_claude::codex::login_line(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenArgs {
    pub target: TargetRef,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub low_latency: Option<bool>,
    #[serde(default)]
    pub run: Option<Run>,
    pub cols: u16,
    pub rows: u16,
    pub output: String,
    pub lifecycle: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAttachArgs {
    pub id: String,
    pub output: String,
    pub lifecycle: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWriteArgs {
    pub id: String,
    pub data: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizeArgs {
    pub id: String,
    pub cols: u16,
    pub rows: u16,
}

/// `terminal_detach` takes the id **and nothing else** — see its doc comment.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalDetachArgs {
    pub id: String,
}

/// `terminal_close` takes a target and a session name because it carries an
/// `agent kill`; both optional, as they were.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCloseArgs {
    pub id: String,
    #[serde(default)]
    pub target: Option<TargetRef>,
    #[serde(default)]
    pub session: Option<String>,
}

fn sink(call: &Call, id: &str) -> Result<Arc<dyn Sink>, String> {
    call.sink(id).map_err(err)
}

pub fn register(r: &mut Registry) {
    r.add("low_latency_probe", Web::Yes, |call: Call, a: TargetArgs| async move {
        low_latency_probe(&call.ctx().store::<TerminalStore>(), a.target).await
    });
    r.add("mosh_install_status", Web::Yes, |_, a: TargetArgs| mosh_install_status(a.target));
    r.add("mosh_server_status", Web::Yes, |_, a: TargetArgs| async move {
        Ok::<_, String>(mosh_server_status(a.target))
    });
    r.add("mosh_install_pending", Web::Yes, |_, a: TargetArgs| async move {
        Ok::<_, String>(mosh_install_pending(a.target))
    });
    r.add("set_mosh_auto_install", Web::Yes, |_, a: SetMoshAutoInstallArgs| async move {
        set_mosh_auto_install(a.on);
        Ok::<_, String>(())
    });
    r.add("terminal_open", Web::Yes, |call: Call, a: TerminalOpenArgs| async move {
        let output = sink(&call, &a.output)?;
        let lifecycle = sink(&call, &a.lifecycle)?;
        let store = call.ctx().store::<TerminalStore>();
        terminal_open(
            call.ctx(),
            &store,
            a.target,
            a.cwd,
            a.session,
            a.low_latency,
            a.run,
            a.cols,
            a.rows,
            output,
            lifecycle,
        )
        .await
    });
    r.add("terminal_attach", Web::Yes, |call: Call, a: TerminalAttachArgs| async move {
        let output = sink(&call, &a.output)?;
        let lifecycle = sink(&call, &a.lifecycle)?;
        terminal_attach(&call.ctx().store::<TerminalStore>(), a.id, output, lifecycle).await
    });
    r.add("terminal_write", Web::Yes, |call: Call, a: TerminalWriteArgs| async move {
        terminal_write(&call.ctx().store::<TerminalStore>(), a.id, a.data).await
    });
    r.add("terminal_resize", Web::Yes, |call: Call, a: TerminalResizeArgs| async move {
        terminal_resize(&call.ctx().store::<TerminalStore>(), a.id, a.cols, a.rows).await
    });
    r.add("terminal_detach", Web::Yes, |call: Call, a: TerminalDetachArgs| async move {
        terminal_detach(&call.ctx().store::<TerminalStore>(), a.id).await
    });
    r.add("terminal_close", Web::Yes, |call: Call, a: TerminalCloseArgs| async move {
        let store = call.ctx().store::<TerminalStore>();
        terminal_close(call.ctx(), &store, a.id, a.target, a.session).await
    });
}
