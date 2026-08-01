//! rmux as a backend.
//!
//! Everything the workbench does — sessions, ssh connections, tunnels — is
//! reachable over a local socket so other apps in the ecosystem can build on it
//! instead of reimplementing it. `rbrowse` is the first, but nothing here is
//! specific to a browser; an Electron dashboard would speak the same protocol.
//!
//! ## Where the session list actually lives
//!
//! In the **webview**, not here. Sessions are UI state: created by the operator,
//! persisted in `localStorage`, ordered by a rail. Rust holds a *mirror*, pushed
//! up by [`control_sync`] whenever that state changes.
//!
//! That is a deliberate trade and worth being honest about. A client's view is
//! only as fresh as the last sync, so the sync must run on every change rather
//! than on a timer. The alternative — moving session ownership into Rust — would
//! mean every UI interaction round-trips through IPC to change a name or reorder
//! a list, which is exactly the kind of hop this project exists to remove.
//!
//! ## Reports are data, never instructions
//!
//! A [`Report`] comes from a browser, which renders pages rmux did not write.
//! Console text, DOM contents and page titles are all attacker-controlled in the
//! ordinary case — that is what the web is. So a report is stored and shown to
//! the operator, and nothing here acts on one. In particular a selection's note
//! is *offered* to the session's Claude for the operator to send; it is never
//! typed into a conversation on the page's say-so.

use std::sync::Arc;

use rmux_control::{ControlServer, Event, Handler, Report, Request, Response, SessionInfo};
use rmux_ssh::forward::{Forwards, ForwardState};
use rmux_transport::{CommandSpec, Tty};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::RwLock;

/// The Tauri event a report is delivered to the UI on.
pub const REPORT_EVENT: &str = "rmux://report";
/// The Tauri event a client's request to switch sessions arrives on.
pub const ACTIVATE_EVENT: &str = "rmux://activate";

/// A report, with the session it belongs to, as the UI receives it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingReport {
    pub session: String,
    #[serde(flatten)]
    pub report: Report,
}

#[derive(Default)]
pub struct ControlState {
    sessions: RwLock<Vec<SessionInfo>>,
    active: RwLock<Option<String>>,
    /// `None` until the socket is up, or if it could not be created. A failure
    /// here must not stop the app: the workbench does not need this socket, and
    /// only clients built on it do.
    server: RwLock<Option<Arc<ControlServer>>>,
}

/// Wires the protocol to rmux's own state. Held by the server task.
struct RmuxHandler {
    app: AppHandle,
    forwards: Arc<Forwards>,
}

impl RmuxHandler {
    async fn session(&self, id: &str) -> Option<SessionInfo> {
        let state = self.app.state::<ControlState>();
        let sessions = state.sessions.read().await;
        sessions.iter().find(|s| s.id == id).cloned()
    }
}

#[async_trait::async_trait]
impl Handler for RmuxHandler {
    async fn handle(&self, request: Request) -> Response {
        let state = self.app.state::<ControlState>();

        match request {
            // Answered before the handler is reached.
            Request::Hello { .. } => Response::Ok,

            Request::ListSessions => {
                Response::Sessions { sessions: state.sessions.read().await.clone() }
            }

            Request::ActiveSession => {
                let id = state.active.read().await.clone();
                let session = match id {
                    Some(id) => self.session(&id).await,
                    None => None,
                };
                Response::Session { session }
            }

            Request::Activate { id } => {
                // The rail lives in the webview, so this is a request *to* it —
                // and the mirror is not updated here. Doing so would report a
                // switch that has not happened if the UI declines or is busy.
                match self.app.emit(ACTIVATE_EVENT, &id) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }

            Request::DiscoverPorts { session } => {
                let Some(info) = self.session(&session).await else {
                    return Response::Error { message: format!("no session called {session}") };
                };
                match discover_ports(&self.app, info.host.as_deref()).await {
                    Ok(ports) => Response::Ports { ports },
                    Err(message) => Response::Error { message },
                }
            }

            Request::ForwardPort { session, port } => {
                let Some(info) = self.session(&session).await else {
                    return Response::Error { message: format!("no session called {session}") };
                };
                let forward = self.forwards.start(info.host.as_deref(), port).await;
                Response::Forwarded {
                    port: forward.port,
                    // `Starting` counts as ok: the tunnel is up as far as anyone
                    // can tell this early, and `Failed` is reported by the state
                    // rather than guessed at here.
                    ok: !matches!(forward.state, ForwardState::Failed),
                    error: forward.error,
                }
            }

            Request::OpenProxy { session } => {
                let Some(info) = self.session(&session).await else {
                    return Response::Error { message: format!("no session called {session}") };
                };
                match self.forwards.socks(info.host.as_deref()).await {
                    Ok(port) => Response::Proxy { port },
                    Err(message) => Response::Error { message },
                }
            }

            Request::Report { session, report } => {
                // Filed against a session that exists, or refused. A report for
                // an unknown session has nowhere to be shown, and silently
                // dropping it would look like the browser was not connected.
                if self.session(&session).await.is_none() {
                    return Response::Error { message: format!("no session called {session}") };
                }
                match self.app.emit(REPORT_EVENT, IncomingReport { session, report }) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
        }
    }
}

/// What the target is listening on.
async fn discover_ports(app: &AppHandle, host: Option<&str>) -> Result<Vec<u16>, String> {
    // Only the host is known here. A client names a *session*, and everything
    // else about how to reach it — user, port, jump hosts — is `~/.ssh/config`'s
    // business, which is where it stays: rmux never resolves that file itself.
    let target = crate::terminal::TargetRef {
        host: host.map(str::to_owned),
        user: None,
        port: None,
    };
    let claude_store = app.state::<crate::claude::ClaudeStore>();
    let resolved = crate::claude::resolve(&claude_store, &target).await?;

    let out = resolved
        .exec(
            &CommandSpec::new("sh")
                .arg("-c")
                .arg(rmux_ssh::forward::DISCOVER_SCRIPT)
                .tty(Tty::None),
        )
        .await
        .map_err(|e| e.to_string())?;

    // A host with neither `ss` nor `netstat` is not an error — it has nothing to
    // report, and a client can still ask for a port by number.
    Ok(rmux_ssh::forward::parse_listening(out.stdout_or_err().unwrap_or(""))
        .into_iter()
        .map(|p| p.port)
        .collect())
}

/// Start the socket. Failure is logged, never fatal — see the module comment.
pub async fn start(app: AppHandle, forwards: Arc<Forwards>) {
    let handler = Arc::new(RmuxHandler { app: app.clone(), forwards });
    match ControlServer::start(handler).await {
        Ok(server) => {
            let state = app.state::<ControlState>();
            *state.server.write().await = Some(Arc::new(server));
        }
        Err(e) => {
            tracing::warn!(error = %e, "control socket unavailable; clients cannot attach");
        }
    }
}

/// The session list, as the UI sees it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sync {
    pub sessions: Vec<SessionInfo>,
    pub active: Option<String>,
}

/// Push the webview's session state down, and tell clients what changed.
///
/// Diffed here rather than in the UI so every client sees the same events
/// regardless of which UI action caused them — a rename from the rail and a
/// title adopted from Claude are the same event to a client, and should be.
#[tauri::command]
pub async fn control_sync(state: State<'_, ControlState>, sync: Sync) -> Result<(), String> {
    let Some(server) = state.server.read().await.clone() else { return Ok(()) };

    let mut events = Vec::new();
    {
        let previous = state.sessions.read().await;

        for session in &sync.sessions {
            match previous.iter().find(|p| p.id == session.id) {
                None => events.push(Event::SessionCreated { session: session.clone() }),
                Some(before) if before != session => {
                    events.push(Event::SessionRenamed { session: session.clone() })
                }
                Some(_) => {}
            }
        }
        for before in previous.iter() {
            if !sync.sessions.iter().any(|s| s.id == before.id) {
                events.push(Event::SessionClosed { id: before.id.clone() });
            }
        }
    }

    let activated = {
        let previous = state.active.read().await;
        // The whole session, not the id — see `Event::SessionActivated`.
        match (&sync.active, previous.as_deref()) {
            (Some(id), before) if Some(id.as_str()) != before => {
                sync.sessions.iter().find(|s| &s.id == id).cloned()
            }
            _ => None,
        }
    };
    if let Some(session) = activated {
        events.push(Event::SessionActivated { session });
    }

    *state.sessions.write().await = sync.sessions;
    *state.active.write().await = sync.active;

    for event in events {
        server.emit(event);
    }
    Ok(())
}

/// Ask a connected browser to open a URL in this session's partition.
///
/// An event rather than a response, because rmux is the server here: it cannot
/// call the browser, only broadcast to whoever is listening. If nothing is
/// attached this quietly does nothing, which is the honest outcome — there is no
/// browser to have failed.
#[tauri::command]
pub async fn control_open_url(
    state: State<'_, ControlState>,
    session: String,
    url: String,
    focus: bool,
) -> Result<bool, String> {
    let Some(server) = state.server.read().await.clone() else { return Ok(false) };
    server.emit(Event::OpenUrl { session, url, focus });
    Ok(true)
}

/// Where a client should look, and whether the socket came up at all.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlInfo {
    pub running: bool,
    /// The handshake file holding the socket path and this run's token. Its
    /// *contents* are never returned — a client proves access by reading it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake: Option<String>,
}

#[tauri::command]
pub async fn control_info(state: State<'_, ControlState>) -> Result<ControlInfo, String> {
    let running = state.server.read().await.is_some();
    Ok(ControlInfo {
        running,
        handshake: rmux_control::handshake_path()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|_| running),
    })
}

/// Tell clients to stop rather than reconnect-loop into a dead socket.
pub async fn shutdown(app: &AppHandle) {
    let state = app.state::<ControlState>();
    if let Some(server) = state.server.read().await.clone() {
        server.emit(Event::ShuttingDown);
    }
}
