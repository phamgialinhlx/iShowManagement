//! Terminal IPC.
//!
//! Output crosses to the webview over a **raw** channel
//! ([`InvokeResponseBody::Raw`]), not as JSON. That matters: Tauri's default
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
use rmux_transport::{
    CommandSpec, LocalTarget, SshHostId, Target, TargetId, local::terminal_env,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody, Response};

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

/// Which machine to open a terminal on.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetRef {
    /// `None` opens on the local machine — rmux as an ordinary IDE.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

impl TargetRef {
    pub(crate) fn id(&self) -> TargetId {
        match &self.host {
            None => TargetId::Local,
            Some(host) => TargetId::Ssh(SshHostId {
                alias: host.clone(),
                user: self.user.clone(),
                port: self.port,
            }),
        }
    }
}

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

/// Open a terminal and start streaming its output.
// An IPC entry point: every argument is a distinct thing the webview must send,
// and bundling them into a struct would only move the same list behind a name.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn terminal_open<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, TerminalStore>,
    target: TargetRef,
    cwd: Option<String>,
    // `session` is a stable name for the shell on the target. The same name
    // always reattaches to the same shell, which is what survives a restart.
    session: Option<String>,
    cols: u16,
    rows: u16,
    output: Channel<Response>,
    lifecycle: Channel<Lifecycle>,
) -> Result<OpenedTerminal, String> {
    let resolved = resolve_target(store.inner(), &target).await?;

    // A named terminal runs under the agent so it outlives this connection.
    // Without a name there is nothing to reattach *to*, so a plain login shell
    // is the honest behaviour rather than a session nobody can find again.
    let plain_shell = || {
        let mut spec = CommandSpec::login_shell();
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
        Some(name) => match crate::agent::ensure_agent(&app, resolved.as_ref()).await {
            Ok(installed) => installed.attach_spec(name, cwd.as_deref(), cols, rows),
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

    store.inner().terminals.lock().insert(id.clone(), Arc::clone(&terminal));
    stream_to_channel(Arc::clone(&terminal), output, lifecycle);

    Ok(OpenedTerminal { id, cols, rows })
}

/// Reattach a view to a terminal that is already running, catching up first.
#[tauri::command]
pub async fn terminal_attach(
    store: State<'_, TerminalStore>,
    id: String,
    output: Channel<Response>,
    lifecycle: Channel<Lifecycle>,
) -> Result<(), String> {
    let terminal = store
        .inner()
        .terminals
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no such terminal: {id}"))?;

    stream_to_channel(terminal, output, lifecycle);
    Ok(())
}

/// Send input.
#[tauri::command]
pub async fn terminal_write(
    store: State<'_, TerminalStore>,
    id: String,
    data: String,
) -> Result<(), String> {
    let terminal = store
        .inner()
        .terminals
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no such terminal: {id}"))?;

    terminal.write(data.as_bytes()).map_err(err)
}

/// Report a new size, so full-screen programs redraw correctly.
#[tauri::command]
pub async fn terminal_resize(
    store: State<'_, TerminalStore>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let terminal = store
        .inner()
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
#[tauri::command]
pub async fn terminal_detach(store: State<'_, TerminalStore>, id: String) -> Result<(), String> {
    if let Some(terminal) = store.inner().terminals.lock().remove(&id) {
        // Already exited is success, not an error: the operator asked to stop
        // watching it, and it has stopped.
        let _ = terminal.kill();
    }
    Ok(())
}

/// Kill a terminal and forget it — on this machine *and* on the target.
#[tauri::command]
pub async fn terminal_close<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, TerminalStore>,
    id: String,
    target: Option<TargetRef>,
    session: Option<String>,
) -> Result<(), String> {
    let terminal = store.inner().terminals.lock().remove(&id);
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
        && let Ok(resolved) = resolve_target(store.inner(), &target).await
        && let Ok(installed) = crate::agent::ensure_agent(&app, resolved.as_ref()).await
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
fn stream_to_channel(
    terminal: Arc<Terminal>,
    output: Channel<Response>,
    lifecycle: Channel<Lifecycle>,
) {
    // Catch up and subscribe atomically, so the handover neither drops nor
    // duplicates a chunk. See `Terminal::attach`.
    let (backlog, mut receiver) = terminal.attach();

    if !backlog.is_empty() {
        let _ = output.send(raw(backlog.to_vec()));
    }

    tauri::async_runtime::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(TerminalEvent::Output(chunk)) => {
                    // A send failure means the webview went away (reload, window
                    // closed). Stop forwarding, but leave the PTY running — the
                    // session outlives its view by design.
                    if output.send(raw(chunk.to_vec())).is_err() {
                        break;
                    }
                }
                Ok(TerminalEvent::Exited { code }) => {
                    let _ = lifecycle.send(Lifecycle::Exited { code });
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(chunks)) => {
                    // Report rather than swallow: the screen is now missing bytes.
                    tracing::warn!(chunks, "terminal view fell behind; output dropped");
                    let _ = lifecycle.send(Lifecycle::Lagged { chunks });
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Wrap bytes so they cross the IPC boundary unencoded.
fn raw(bytes: Vec<u8>) -> Response {
    Response::new(InvokeResponseBody::Raw(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_host_means_the_local_machine() {
        let local = TargetRef { host: None, user: None, port: None };
        assert_eq!(local.id(), TargetId::Local);
    }

    #[test]
    fn a_host_alias_is_carried_through_unparsed() {
        // The alias may be a ~/.ssh/config Host entry; splitting or rewriting it
        // here would break ProxyJump and friends.
        let remote =
            TargetRef { host: Some("devbox".into()), user: Some("deploy".into()), port: Some(2222) };

        match remote.id() {
            TargetId::Ssh(host) => {
                assert_eq!(host.alias, "devbox");
                assert_eq!(host.user.as_deref(), Some("deploy"));
                assert_eq!(host.port, Some(2222));
            }
            other => panic!("expected an ssh target, got {other:?}"),
        }
    }

    #[test]
    fn targets_are_cached_per_destination() {
        // Same alias with a different user is a different authenticated session
        // and must not reuse the first one's connection.
        let a = TargetRef { host: Some("devbox".into()), user: None, port: None };
        let b = TargetRef { host: Some("devbox".into()), user: Some("root".into()), port: None };
        assert_ne!(a.id(), b.id());
    }
}
