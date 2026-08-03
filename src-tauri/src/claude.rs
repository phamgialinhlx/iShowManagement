//! Claude session IPC.
//!
//! A session owns a PTY, so it outlives the panel showing it — closing the tab
//! must not kill a Claude that is mid-task.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_claude::{ClaudeSession, ClaudeState, SessionInfo};
use rmux_ssh::SshTarget;
use rmux_term::{TermSize, TerminalEvent};
use rmux_transport::{LocalTarget, Target, TargetId};
use serde::Serialize;
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody, Response};

use crate::terminal::TargetRef;

#[derive(Default)]
pub struct ClaudeStore {
    sessions: Mutex<HashMap<String, Arc<ClaudeSession>>>,
    targets: Mutex<HashMap<TargetId, Arc<dyn Target>>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedSession {
    pub id: String,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub(crate) async fn resolve(store: &ClaudeStore, target: &TargetRef) -> Result<Arc<dyn Target>, String> {
    let id = target.id();
    if let Some(existing) = store.targets.lock().get(&id) {
        return Ok(Arc::clone(existing));
    }
    let resolved: Arc<dyn Target> = match &id {
        TargetId::Local => Arc::new(LocalTarget::new()),
        TargetId::Ssh(host) => {
            let ssh = SshTarget::new(host.clone());
            ssh.connect().await.map_err(err)?;
            Arc::new(ssh)
        }
    };
    store.targets.lock().insert(id, Arc::clone(&resolved));
    Ok(resolved)
}

/// Claude sessions already recorded for a folder, newest first.
#[tauri::command]
pub async fn claude_list_sessions(
    store: State<'_, ClaudeStore>,
    target: TargetRef,
    folder: String,
) -> Result<Vec<SessionInfo>, String> {
    let resolved = resolve(store.inner(), &target).await?;
    ClaudeSession::list(resolved.as_ref(), &folder).await.map_err(err)
}

/// Every Claude session on a host, newest first.
///
/// The folder comes back with each one, read from the transcript's own `cwd`, so
/// resuming does not require having found the directory first.
#[tauri::command]
pub async fn claude_list_all_sessions(
    store: State<'_, ClaudeStore>,
    target: TargetRef,
) -> Result<Vec<SessionInfo>, String> {
    let resolved = resolve(store.inner(), &target).await?;
    ClaudeSession::list_all(resolved.as_ref()).await.map_err(err)
}

/// Read a conversation back as text.
///
/// Separate from the live session on purpose: this reads the transcript on disk,
/// so it works for a conversation that is not running, and it is unaffected by
/// whatever the TUI happens to be drawing right now.
#[tauri::command]
pub async fn claude_transcript(
    store: State<'_, ClaudeStore>,
    target: TargetRef,
    folder: String,
    session: Option<String>,
    tail_bytes: Option<u64>,
) -> Result<rmux_claude::transcript::Transcript, String> {
    let resolved = resolve(store.inner(), &target).await?;
    let tail = tail_bytes.unwrap_or(rmux_claude::transcript::DEFAULT_TAIL_BYTES);

    let script = rmux_claude::transcript::transcript_script(&folder, session.as_deref(), tail);
    let spec = rmux_transport::CommandSpec::new("sh")
        .arg("-c")
        .arg(script)
        .tty(rmux_transport::Tty::None);

    let out = resolved.exec(&spec).await.map_err(err)?;
    Ok(rmux_claude::transcript::parse(out.stdout.as_bytes(), true))
}

/// End the agent-hosted Claude session with this name.
///
/// Needed to change rendering mode. The agent reattaches *by name*, so remounting
/// the view alone would simply find the Claude already running under the old
/// mode — the environment only applies when the session is created.
#[tauri::command]
pub async fn claude_end_session<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, ClaudeStore>,
    target: TargetRef,
    session_name: String,
) -> Result<(), String> {
    let resolved = resolve(store.inner(), &target).await?;
    let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;

    let spec = rmux_transport::CommandSpec::new(&installed.program)
        .arg("kill")
        .arg(&session_name)
        .tty(rmux_transport::Tty::None);

    // Best effort: a session that is already gone is the outcome being asked for.
    let _ = resolved.exec(&spec).await;
    Ok(())
}

/// Launch `claude` and stream its screen to the UI.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn claude_start<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, ClaudeStore>,
    target: TargetRef,
    cwd: Option<String>,
    // `resume` carries the id of a conversation to continue instead of starting
    // a new one; `None` starts fresh.
    resume: Option<String>,
    // A stable name for this conversation on the target. Given one, Claude runs
    // under the agent — so it keeps working after rmux is closed, and reopening
    // reattaches to the same running process rather than replaying a transcript.
    session_name: Option<String>,
    // Fullscreen puts Claude on the alternate screen and gives it the mouse,
    // which breaks selection and makes scrolling round-trip. Off unless asked.
    fullscreen: Option<bool>,
    // `--dangerously-skip-permissions`. Deliberately a *per-launch* argument and
    // never a stored preference: it is chosen for one conversation at the moment
    // of starting it, which is the only point the operator has the context to
    // judge it. A saved setting would silently apply it to work started weeks
    // later on a different machine.
    skip_permissions: Option<bool>,
    // Which saved model configuration to run under — Kimi, GLM, a gateway. Kept
    // *with* the session rather than as an app-wide default, because which
    // provider a piece of work runs against is a property of that work, and a
    // conversation that silently changed provider on reconnect would be billed
    // and answered by whoever happened to be selected at the time.
    model_profile: Option<String>,
    cols: u16,
    rows: u16,
    output: Channel<Response>,
) -> Result<StartedSession, String> {
    let resolved = resolve(store.inner(), &target).await?;
    let size = TermSize { cols, rows };
    let rendering = if fullscreen.unwrap_or(false) {
        rmux_claude::Rendering::Fullscreen
    } else {
        rmux_claude::Rendering::Inline
    };

    // One argument list for both launch paths. They had drifted apart once
    // already — the agent path missed the rendering flags — so anything that
    // affects how Claude starts is built here and used by both.
    let args: Vec<String> = if skip_permissions.unwrap_or(false) {
        vec!["--dangerously-skip-permissions".to_owned()]
    } else {
        Vec::new()
    };

    // Hand the stored Claude account to this host's agent first, so the session
    // starts already signed in. No-op when nothing is stored.
    // Both of these deliver environment *through the agent*, so on a host where
    // it is unavailable they are skipped rather than fatal — the operator's own
    // `claude` login on that machine signs the session in instead, which is the
    // pre-agent behaviour and works.
    let agent_available = resolved.platform() != Some(rmux_transport::Platform::Windows);
    if agent_available {
        crate::claude_account::apply_to_target(&app, resolved.as_ref()).await?;
    }

    // Then the model profile, which may override the account's variables — a
    // profile that carries its own token is meant to win over the stored Claude
    // login, which is the whole point of selecting one. Applied even when none
    // is selected: the daemon outlives sessions, so the previous profile's base
    // URL is still in its environment, and starting without clearing it would
    // run this session against the old provider while the UI showed Anthropic.
    if agent_available {
        crate::model_profile::apply_to_target(&app, resolved.as_ref(), model_profile.as_deref())
            .await?;
    } else if model_profile.is_some() {
        // Silently ignoring a chosen provider would run the conversation against
        // Anthropic while the UI named someone else — the exact silent
        // mis-routing the profile design exists to prevent.
        return Err(
            "a model profile needs the rmux agent to deliver its credentials, and persistence \
             is not enabled on Windows yet — start this session on Anthropic, or run it from \
             another host"
                .to_owned(),
        );
    }

    // Without an agent the conversation cannot outlive the connection, but it
    // still runs — and `--resume` means reopening picks the transcript back up,
    // so what is lost is the *process* surviving, not the work.
    let session = Arc::new(match session_name.as_ref().filter(|_| agent_available) {
        Some(name) => {
            let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;
            let line = ClaudeSession::launch_line(resume.as_deref(), &args, rendering);

            let mut spec = installed.attach_spec(name, cwd.as_deref(), cols, rows);
            spec = spec.arg("--login-command").arg(line);

            ClaudeSession::start_with_spec(resolved.as_ref(), spec, cwd.as_deref(), size)
                .map_err(err)?
        }
        None => ClaudeSession::start_resuming(
            resolved.as_ref(),
            cwd.as_deref(),
            resume.as_deref(),
            &args,
            size,
        )
        .map_err(err)?,
    });

    let id = session.terminal().id().to_owned();
    store.inner().sessions.lock().insert(id.clone(), Arc::clone(&session));

    stream_claude(Arc::clone(&session), output);
    Ok(StartedSession { id })
}

/// Pump a Claude session's screen into the webview.
///
/// The raw screen always goes to an xterm view, so the operator sees exactly
/// what Claude is showing rather than only our interpretation of it.
fn stream_claude(session: Arc<ClaudeSession>, output: Channel<Response>) {
    let (backlog, mut receiver) = session.terminal().attach();
    if !backlog.is_empty() {
        let _ = output.send(Response::new(InvokeResponseBody::Raw(backlog.to_vec())));
    }
    tauri::async_runtime::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(TerminalEvent::Output(chunk)) => {
                    if output.send(Response::new(InvokeResponseBody::Raw(chunk.to_vec()))).is_err()
                    {
                        break;
                    }
                }
                Ok(TerminalEvent::Exited { .. }) => break,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(_) => {}
            }
        }
    });
}

/// Re-stream an already-running Claude session.
///
/// A view that remounts must reattach, not start over. Starting over would kill
/// a conversation mid-task and lose its context — the exact opposite of what a
/// session is for.
#[tauri::command]
pub async fn claude_attach(
    store: State<'_, ClaudeStore>,
    id: String,
    output: Channel<Response>,
) -> Result<(), String> {
    let session = session(store.inner(), &id)?;
    stream_claude(session, output);
    Ok(())
}

fn session(store: &ClaudeStore, id: &str) -> Result<Arc<ClaudeSession>, String> {
    store.sessions.lock().get(id).cloned().ok_or_else(|| format!("no such session: {id}"))
}

/// What Claude is showing, plus whether it is still there at all.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolledState {
    #[serde(flatten)]
    pub state: ClaudeState,
    /// The exit code, once the attach process has died.
    ///
    /// **This is how a dropped connection becomes visible.** The pane is fed by
    /// a broadcast of terminal output; when the process behind it exits, that
    /// stream simply stops. Nothing arrives, nothing errors, and the pane sits
    /// there looking like a Claude that has gone quiet — which after a laptop
    /// sleeps is exactly what it is not. The UI already polls this endpoint
    /// several times a second, so reporting death here costs nothing and needs
    /// no second channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exited: Option<i32>,
}

/// What Claude is showing. Polled by the UI.
#[tauri::command]
pub async fn claude_state(
    store: State<'_, ClaudeStore>,
    id: String,
) -> Result<PolledState, String> {
    let session = session(store.inner(), &id)?;
    Ok(PolledState { state: session.state(), exited: session.terminal().exit_code() })
}

/// Answer the prompt identified by `fingerprint`.
///
/// The fingerprint is required, not optional: it is what stops an answer landing
/// on a question that has already been replaced.
#[tauri::command]
pub async fn claude_answer(
    store: State<'_, ClaudeStore>,
    id: String,
    fingerprint: String,
    key: String,
) -> Result<(), String> {
    session(store.inner(), &id)?.answer(&fingerprint, &key).map_err(err)
}

#[tauri::command]
pub async fn claude_send(
    store: State<'_, ClaudeStore>,
    id: String,
    text: String,
) -> Result<(), String> {
    session(store.inner(), &id)?.send(&text).await.map_err(err)
}

#[tauri::command]
pub async fn claude_interrupt(store: State<'_, ClaudeStore>, id: String) -> Result<(), String> {
    session(store.inner(), &id)?.interrupt().map_err(err)
}

#[tauri::command]
pub async fn claude_resize(
    store: State<'_, ClaudeStore>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    session(store.inner(), &id)?.resize(TermSize { cols, rows }).map_err(err)
}

/// Raw keystrokes, for typing directly into Claude's own TUI.
#[tauri::command]
pub async fn claude_write(
    store: State<'_, ClaudeStore>,
    id: String,
    data: String,
) -> Result<(), String> {
    session(store.inner(), &id)?.terminal().write(data.as_bytes()).map_err(err)
}

#[tauri::command]
pub async fn claude_stop(store: State<'_, ClaudeStore>, id: String) -> Result<(), String> {
    if let Some(session) = store.inner().sessions.lock().remove(&id) {
        let _ = session.terminal().kill();
    }
    Ok(())
}
