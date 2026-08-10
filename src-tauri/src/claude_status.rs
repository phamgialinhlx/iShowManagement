//! Client side of agent-push Claude status (Phase 2).
//!
//! `rmux-agent watch-status` (see `crates/rmux-agent/src/status.rs`) streams a
//! host's Claude session-status changes as NDJSON. This reads that stream — one
//! watcher per host — and forwards each line to the webview as a `claude-status`
//! event, so the rail can be driven by *changes* instead of the per-pane
//! `claude_state` busy-poll that was heating the machine.
//!
//! ## How the stream is run
//!
//! Through `Target::build_command`, exactly like a terminal — for SSH that wraps
//! the command in `ssh -T host -- …`, reusing the ControlMaster socket and the
//! user's `ssh` config for free, and with `Tty::None` it is a plain piped stdout
//! rather than a PTY. Local and remote are the same code path; the watcher never
//! learns SSH exists.
//!
//! ## Failure handling
//!
//! - The stream closing (ssh dropped, the laptop slept) is not an error — the
//!   watcher reconnects after a short backoff, because the sessions on the far
//!   side are still running.
//! - An agent too old to know the subcommand exits with a usage error on stderr;
//!   that is reported once as `unsupported` so the UI falls back to the poll, and
//!   the watcher stops rather than respawning a failing process forever.
//! - Windows/macOS hosts have no agent at all, so there is nothing to watch and
//!   the UI keeps polling there.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use rmux_transport::{CommandSpec, NoConsoleWindow, Platform, Target, TargetId, Tty};
use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::claude::{ClaudeStore, resolve};
use crate::terminal::TargetRef;

/// The event the UI listens on. Each payload is one line from the agent, with a
/// `targetId` added — either `{"ready":true}`, a status update
/// (`{sessionId, cwd, pid, status, updatedAt}`, `status` possibly `"gone"`), or
/// `{"unsupported":true}`.
const EVENT: &str = "claude-status";

const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct ClaudeStatusStore {
    /// One watcher task per host. A finished handle means the watcher exited
    /// (unsupported host) and may be restarted.
    watchers: Mutex<HashMap<TargetId, tokio::task::JoinHandle<()>>>,
}

impl ClaudeStatusStore {
    /// Stop watching this host, and **abort the task**.
    ///
    /// Dropping a `JoinHandle` does not cancel what it is running — the task
    /// keeps going, detached, with nothing left holding it. This one runs
    /// `rmux-agent watch-status` over SSH in a reconnect loop, so a merely
    /// forgotten watcher would notice its connection drop and dial straight back
    /// out. A disconnect would then close the master and something would
    /// immediately re-open it: the operator sees a server that refuses to
    /// disconnect, and nothing on screen names the watcher as the cause.
    ///
    /// This is the store PR #9 does not evict, because it did not exist yet.
    pub fn evict_target(&self, id: &TargetId) -> bool {
        match self.watchers.lock().remove(id) {
            Some(handle) => {
                handle.abort();
                true
            }
            None => false,
        }
    }

    /// Insert a watcher handle directly. Tests only.
    pub fn insert_for_test(&self, id: TargetId, handle: tokio::task::JoinHandle<()>) {
        self.watchers.lock().insert(id, handle);
    }
}

/// Start (or confirm) a status watcher for `target`. Idempotent — safe to call
/// whenever an agent-hosted Claude session opens, and on repeat.
#[tauri::command]
pub async fn claude_status_watch<R: Runtime>(
    app: AppHandle<R>,
    status_store: State<'_, ClaudeStatusStore>,
    claude_store: State<'_, ClaudeStore>,
    target: TargetRef,
) -> Result<(), String> {
    let id = target.id();
    if is_running(&status_store, &id) {
        return Ok(());
    }

    let resolved = resolve(claude_store.inner(), &target).await?;
    // No agent on Windows/macOS → no stream to read; the UI keeps polling there.
    if resolved.platform() == Some(Platform::Windows) {
        return Ok(());
    }
    let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;

    ensure_watch(&app, status_store.inner(), resolved, installed.program);
    Ok(())
}

/// Stop the watcher for `target`, if any.
#[tauri::command]
pub fn claude_status_unwatch(status_store: State<'_, ClaudeStatusStore>, target: TargetRef) {
    if let Some(handle) = status_store.watchers.lock().remove(&target.id()) {
        handle.abort();
    }
}

fn is_running(store: &ClaudeStatusStore, id: &TargetId) -> bool {
    store.watchers.lock().get(id).map(|h| !h.is_finished()).unwrap_or(false)
}

/// Spawn the per-host watcher task if one is not already running.
///
/// Synchronous and cheap so it can be called from the session-start path with an
/// already-resolved target, without another connect or round trip.
pub fn ensure_watch<R: Runtime>(
    app: &AppHandle<R>,
    store: &ClaudeStatusStore,
    target: Arc<dyn Target>,
    program: String,
) {
    let id = target.id().clone();
    let mut guard = store.watchers.lock();
    if guard.get(&id).map(|h| !h.is_finished()).unwrap_or(false) {
        return;
    }
    let app = app.clone();
    let label = id.label();
    // `tokio::spawn` (not `tauri::async_runtime::spawn`) for its `JoinHandle`,
    // which carries both `is_finished` and `abort`; tauri runs on tokio, so the
    // runtime is the same one either way.
    let handle = tokio::spawn(async move {
        run_watcher(app, target, program, label).await;
    });
    guard.insert(id, handle);
}

/// Reconnecting read loop for one host.
async fn run_watcher<R: Runtime>(
    app: AppHandle<R>,
    target: Arc<dyn Target>,
    program: String,
    target_id: String,
) {
    let mut backoff = BACKOFF_START;
    loop {
        match run_once(&app, target.as_ref(), &program, &target_id).await {
            Outcome::Unsupported => {
                // An older agent. Tell the UI once so it falls back to polling,
                // then stop — respawning a process that will only error again is
                // waste, and the fingerprinted agent path means an upgrade
                // reinstalls before this code would run against it anyway.
                let _ = app.emit(EVENT, json!({ "targetId": target_id, "unsupported": true }));
                return;
            }
            // The stream had been working and closed — reconnect promptly.
            Outcome::Ended => backoff = BACKOFF_START,
            // Never got going. Back off so a persistently unreachable host does
            // not spin.
            Outcome::FailedToStart => {}
        }
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

async fn run_once<R: Runtime>(
    app: &AppHandle<R>,
    target: &dyn Target,
    program: &str,
    target_id: &str,
) -> Outcome {
    let spec = CommandSpec::new(program).arg("watch-status").tty(Tty::None);
    let Ok(resolved) = target.build_command(&spec) else {
        return Outcome::FailedToStart;
    };

    let mut cmd = Command::new(&resolved.program);
    cmd.args(&resolved.args);
    for (key, value) in &resolved.env {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // A GUI process has no console, so a console-subsystem child (ssh) would get
    // a brand new visible window on Windows without this.
    cmd.no_console_window();
    // Aborting this task (unwatch, or app teardown) must take the ssh child with
    // it rather than leaking a connection.
    cmd.kill_on_drop(true);

    let Ok(mut child) = cmd.spawn() else {
        return Outcome::FailedToStart;
    };

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Outcome::FailedToStart;
    };

    // Watch stderr for the old-agent usage error. Any other stderr (an ssh
    // connection message) is left to fall through as a plain reconnect.
    let unsupported = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&unsupported);
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("usage:") || line.contains("unknown option") {
                flag.store(true, Ordering::Relaxed);
            }
        }
    });

    let mut saw_ready = false;
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("ready").and_then(serde_json::Value::as_bool) == Some(true) {
            saw_ready = true;
        }
        if let Some(obj) = value.as_object_mut() {
            obj.insert("targetId".to_owned(), json!(target_id));
        }
        let _ = app.emit(EVENT, value);
    }

    let _ = child.wait().await;
    stderr_task.abort();

    // Only "unsupported" if it never streamed anything: a stream that produced a
    // ready line and *then* saw the word "usage" in some later output is a
    // reconnect, not an old agent.
    if unsupported.load(Ordering::Relaxed) && !saw_ready {
        Outcome::Unsupported
    } else {
        Outcome::Ended
    }
}
