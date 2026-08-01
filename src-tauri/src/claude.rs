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

    // Hand the stored Claude account to this host's agent first, so the session
    // starts already signed in. No-op when nothing is stored.
    crate::claude_account::apply_to_target(&app, resolved.as_ref()).await?;

    let session = Arc::new(match &session_name {
        Some(name) => {
            let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;
            let line = ClaudeSession::launch_line(resume.as_deref(), &[], rendering);

            let mut spec = installed.attach_spec(name, cwd.as_deref(), cols, rows);
            spec = spec.arg("--login-command").arg(line);

            ClaudeSession::start_with_spec(resolved.as_ref(), spec, cwd.as_deref(), size)
                .map_err(err)?
        }
        None => ClaudeSession::start_resuming(
            resolved.as_ref(),
            cwd.as_deref(),
            resume.as_deref(),
            &[],
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

/// What Claude is showing. Polled by the UI.
#[tauri::command]
pub async fn claude_state(
    store: State<'_, ClaudeStore>,
    id: String,
) -> Result<ClaudeState, String> {
    Ok(session(store.inner(), &id)?.state())
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
