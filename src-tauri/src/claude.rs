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

impl ClaudeStore {
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

/// Every pi conversation on a host, newest first.
///
/// The pi twin of [`claude_list_all_sessions`]: the folder each one belongs to
/// comes back on `cwd`, read from the transcript's own header, so a resume
/// picker does not require the operator to have found the directory first.
#[tauri::command]
pub async fn pi_list_all_sessions(
    store: State<'_, ClaudeStore>,
    target: TargetRef,
) -> Result<Vec<rmux_claude::pi::Conversation>, String> {
    let resolved = resolve(store.inner(), &target).await?;
    ClaudeSession::pi_list_all(resolved.as_ref()).await.map_err(err)
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

/// Read a pi conversation back as text.
///
/// The pi twin of [`claude_transcript`]: same shape, same "read the tail on the
/// machine that owns the disk" path, but the script and parser are pi's, because
/// pi keeps its transcripts in a cwd-encoded directory and names each file
/// `<ISO-timestamp>_<id>.jsonl`. With no `session`, the newest conversation in
/// `cwd` is read — "the latest", which is what you want having just talked to it.
#[tauri::command]
pub async fn pi_transcript(
    store: State<'_, ClaudeStore>,
    target: TargetRef,
    cwd: String,
    session: Option<String>,
    tail_bytes: Option<u64>,
) -> Result<rmux_claude::transcript::Transcript, String> {
    let resolved = resolve(store.inner(), &target).await?;
    let tail = tail_bytes.unwrap_or(rmux_claude::transcript::DEFAULT_TAIL_BYTES);

    let script = rmux_claude::pi::transcript_script(&cwd, session.as_deref(), tail);
    let spec = rmux_transport::CommandSpec::new("sh")
        .arg("-c")
        .arg(script)
        .tty(rmux_transport::Tty::None);

    let out = resolved.exec(&spec).await.map_err(err)?;
    let bytes = out.stdout.as_bytes();

    // Split the NUL-framed `id\0size\0` header off the front — the same framing
    // `pi::transcript_script` emits, mirroring `transcript::parse` — then hand the
    // body to pi's lenient parser. `pi::parse` leaves `total_bytes`/`read_bytes`
    // zero, so they are filled here from the header size and the tail read, or the
    // transcript view's byte counts and "load more" break.
    let mut parts = bytes.splitn(3, |b| *b == 0);
    let (_id, size, body) = (parts.next(), parts.next(), parts.next());
    let (Some(size), Some(body)) = (size, body) else {
        // No file / empty output: an empty transcript, not an error.
        return Ok(rmux_claude::transcript::Transcript::default());
    };
    let total_bytes = String::from_utf8_lossy(size).trim().parse::<u64>().unwrap_or(0);

    let mut transcript = rmux_claude::pi::parse(body, true);
    transcript.total_bytes = total_bytes;
    transcript.read_bytes = body.len() as u64;
    Ok(transcript)
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
    status_store: State<'_, crate::claude_status::ClaudeStatusStore>,
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
    // Desktop-launched Claude renders fullscreen by default. `None`/unset means
    // the operator never configured this session, so it gets the default; only an
    // explicit `Some(false)` — the INLINE toggle in Session Settings — stays
    // inline. The bridge is unaffected: it builds its own line from
    // `Rendering::default()` (still Inline) and never reaches here.
    let rendering = if fullscreen.unwrap_or(true) {
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
            // Push status for this host from now on, so the rail is driven by
            // change events rather than the per-pane screen-scrape poll. Cheap
            // and idempotent — a second session on the same host is a no-op.
            crate::claude_status::ensure_watch(
                &app,
                status_store.inner(),
                Arc::clone(&resolved),
                installed.program.clone(),
            );
            let line = rmux_claude::launch_line(resume.as_deref(), &args, rendering);

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

/// Launch `pi` under the agent and stream its screen to the UI.
///
/// The pi twin of [`claude_start`]. It reuses the very same [`ClaudeStore`], so
/// the id-keyed, provider-blind commands — `claude_attach`, `claude_write`,
/// `claude_send`, `claude_resize`, `claude_stop`, `claude_end_session` — all
/// operate on the pi session unchanged. What differs is only what pi supplies:
/// no `Rendering`/`CLAUDE_CODE_*` env (pi has one inline mode), no
/// `--dangerously-skip-permissions`, no model profile and no Claude account.
/// pi has no `~/.claude/sessions` status file, so its rail dot is driven by a
/// hook extension instead: `pi_start` installs it on the host (best-effort),
/// sets `RMUX_AGENT_BIN`/`RMUX_STATUS_KEY` on the launch line so pi publishes
/// `~/.rmux/status/<name>.json`, and starts the same `ensure_watch` stream that
/// carries it up to the rail.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn pi_start<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, ClaudeStore>,
    status_store: State<'_, crate::claude_status::ClaudeStatusStore>,
    target: TargetRef,
    cwd: Option<String>,
    // Carries the id of a pi conversation to continue (`--session-id`); `None`
    // starts fresh. Accepted now so Phase 2 needs no signature change.
    resume: Option<String>,
    // The daemon session name on the target, e.g. `pi-<id>`. The daemon already
    // classifies `pi-`-prefixed sessions, and `claude_end_session` kills by this
    // name — so closing the pane kills the work with no pi-specific kill needed.
    session_name: Option<String>,
    cols: u16,
    rows: u16,
    output: Channel<Response>,
) -> Result<StartedSession, String> {
    let resolved = resolve(store.inner(), &target).await?;
    let size = TermSize { cols, rows };

    // pi runs under the agent so the conversation outlives rmux and reattaches
    // by name. Without the agent there is no persistent-shell host for it — pi
    // has no non-agent launch path — so this fails loudly with a reason rather
    // than silently starting something that cannot survive a disconnect. The
    // agent is unavailable on Windows (it is a linux-musl binary).
    let agent_available = resolved.platform() != Some(rmux_transport::Platform::Windows);
    let Some(name) = session_name.as_ref().filter(|_| agent_available) else {
        return Err(
            "pi runs under the rmux agent, which is not available on this host yet — start pi \
             on a Linux host, or open a plain terminal there instead"
                .to_owned(),
        );
    };

    let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;

    // Push status for this host from now on. pi has no `~/.claude/sessions`
    // file, but its hook writes `~/.rmux/status/<key>.json`, which the agent's
    // `watch-status` stream already carries — so a pi-only host has nothing
    // streaming that directory until this runs. Idempotent per target.
    crate::claude_status::ensure_watch(
        &app,
        status_store.inner(),
        Arc::clone(&resolved),
        installed.program.clone(),
    );

    // Give this pi session the status signal Claude gets for free. Install the
    // hook extension on the host (idempotent, **best-effort**) so pi publishes
    // `working | idle`; a host that cannot take it still runs pi, only the rail
    // dot stays dark — so a failure warns and never blocks the launch, mirroring
    // the bridge's own install. The extension source is base64'd and `base64 -d`
    // decoded into the file: it is TS text with newlines, and base64's alphabet
    // carries nothing the shell reacts to. `$HOME` is resolved to an absolute
    // path first — never shell-quoted as a literal — then the destination is
    // quoted.
    let install: Result<(), String> = async {
        use base64::Engine as _;

        let home_spec = rmux_transport::CommandSpec::new("sh")
            .arg("-c")
            .arg(rmux_agent::provision::home_script())
            .tty(rmux_transport::Tty::None);
        let home = resolved.exec(&home_spec).await.map_err(err)?.stdout.trim().to_owned();
        if home.is_empty() {
            return Err("the host did not report a home directory".to_owned());
        }

        let dir = format!("{}/.pi/agent/extensions", home.trim_end_matches('/'));
        let path = rmux_claude::pi::status_extension_path(&home);
        let b64 = base64::engine::general_purpose::STANDARD
            .encode(rmux_claude::pi::status_extension_source());
        let script = format!(
            "set -e\nmkdir -p {}\nprintf %s '{}' | base64 -d > {}",
            rmux_transport::shell_quote(&dir),
            b64,
            rmux_transport::shell_quote(&path),
        );
        let spec = rmux_transport::CommandSpec::new("sh")
            .arg("-c")
            .arg(&script)
            .tty(rmux_transport::Tty::None);
        resolved.exec(&spec).await.map_err(err)?;
        Ok(())
    }
    .await;
    if let Err(e) = install {
        tracing::warn!(error = %e, "could not install pi status extension");
    }

    // Fresh session: no initial prompt. `session_name` labels a new conversation
    // and `resume` continues an existing one — argument order per `pi::launch_line`.
    // Prefix two env assignments onto the **shell line** (never `CommandSpec::env`):
    // under the agent the daemon spawns the shell with its own environment, so env
    // on the attach command never arrives. `RMUX_AGENT_BIN` points the hook at the
    // host agent's `rmux-agent hook`; `RMUX_STATUS_KEY` is the daemon session name,
    // so the status file is `~/.rmux/status/<name>.json`, streamed with
    // `sessionId == name` — exactly what the rail matches on. Neither is a secret
    // (a path and a session name), so the shell line is the right place for them.
    let line = rmux_claude::pi::launch_line(None, session_name.as_deref(), resume.as_deref());
    let line = format!(
        "RMUX_AGENT_BIN={} RMUX_STATUS_KEY={} {}",
        rmux_transport::shell_quote(&installed.program),
        rmux_transport::shell_quote(name),
        line,
    );

    // The pi line stands alone as the `--login-command` value; the agent spawns
    // it through `CommandSpec::login_shell()` (`-l -i`), so no shell is
    // hand-built and no env travels on argv.
    let mut spec = installed.attach_spec(name, cwd.as_deref(), cols, rows);
    spec = spec.arg("--login-command").arg(line);

    let session =
        Arc::new(ClaudeSession::start_with_spec(resolved.as_ref(), spec, cwd.as_deref(), size)
            .map_err(err)?);

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
