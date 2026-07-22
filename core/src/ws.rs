//! WebSocket ↔ PTY bridge.
//!
//! Modes (via `?mode=`):
//!   - `local`   — the app machine's `$SHELL` (Phase 1).
//!   - `console` — `ssh <alias>` interactive login (Phase 2).
//!   - `tmux`    — `ssh <alias> -- tmux new-session -A -s <session>` (Phase 2).
//! Later: `docker-logs`, `docker-exec`.
//!
//! Frame protocol:
//!   - client → server: **binary** = raw keystrokes; **text** = JSON control
//!     (`{"t":"r","cols":N,"rows":N}` = resize).
//!   - server → client: **binary** = raw PTY output.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use portable_pty::CommandBuilder;
use serde::Deserialize;

use crate::api::{AppState, LOCAL_ID};
use crate::pty::Pty;
use crate::secrets::pw_key;
use crate::security::safe_name;
use crate::ssh;

#[derive(Deserialize)]
pub struct WsParams {
    #[serde(default)]
    mode: String,
    alias: Option<String>,
    session: Option<String>,
    cid: Option<String>,
}

/// Control messages the client can send as a text frame.
#[derive(Deserialize)]
#[serde(tag = "t")]
enum Control {
    #[serde(rename = "r")]
    Resize { cols: u16, rows: u16 },
}

pub async fn handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| run(socket, params, state))
}

async fn run(socket: WebSocket, params: WsParams, state: AppState) {
    let mut socket = socket;
    let spawned = match params.mode.as_str() {
        "" | "local" => Pty::spawn_shell(80, 24),

        "console" | "tmux" => {
            let Some(alias) = params.alias.as_deref().filter(|a| safe_name(a)) else {
                return fail(&mut socket, "console/tmux requires a valid ?alias").await;
            };
            let remote = if params.mode == "tmux" {
                // Default the tmux session name to the alias.
                let session = params.session.as_deref().unwrap_or(alias);
                if !safe_name(session) {
                    return fail(&mut socket, "invalid ?session name").await;
                }
                Some(ssh::tmux_remote(session))
            } else {
                None
            };
            let cmd = ssh::ssh_command(alias, remote.as_deref());
            let password = state.secrets.get(&pw_key(alias));
            Pty::spawn(cmd, 80, 24, password)
        }

        "docker-logs" | "docker-exec" => {
            let Some(alias) = params.alias.as_deref() else {
                return fail(&mut socket, "docker session requires ?alias").await;
            };
            let Some(cid) = params.cid.as_deref().filter(|c| safe_name(c)) else {
                return fail(&mut socket, "docker session requires a valid ?cid").await;
            };
            // cid is SAFE_NAME → safe to interpolate.
            let remote = if params.mode == "docker-logs" {
                format!("docker logs -f --tail 200 {cid}")
            } else {
                format!("docker exec -it {cid} sh -c 'bash || sh'")
            };
            match target_for(alias) {
                Some(true) => Pty::spawn(local_sh(&remote), 80, 24, None),
                Some(false) => {
                    let cmd = ssh::ssh_command(alias, Some(&remote));
                    Pty::spawn(cmd, 80, 24, state.secrets.get(&pw_key(alias)))
                }
                None => return fail(&mut socket, "bad ?alias").await,
            }
        }

        other => return fail(&mut socket, &format!("unsupported mode: {other}")).await,
    };

    let (pty, mut output) = match spawned {
        Ok(v) => v,
        Err(e) => return fail(&mut socket, &format!("failed to start session: {e}")).await,
    };

    bridge(socket, pty, &mut output).await;
    // `pty` drops here → child killed, threads unwind.
}

/// Pump bytes between the socket and the PTY until either side closes.
async fn bridge(mut socket: WebSocket, pty: Pty, output: &mut tokio::sync::mpsc::Receiver<Vec<u8>>) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Binary(bytes))) => pty.write(bytes.to_vec()),
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(Control::Resize { cols, rows }) =
                            serde_json::from_str::<Control>(&text)
                        {
                            pty.resize(cols, rows);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {} // ping/pong handled by axum
                }
            }
            chunk = output.recv() => {
                match chunk {
                    Some(bytes) => {
                        if socket.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break, // session exited
                }
            }
        }
    }
}

async fn fail(socket: &mut WebSocket, msg: &str) {
    let _ = socket.send(Message::Text(msg.to_string().into())).await;
}

/// Resolve an alias for a docker session: `Some(true)` = run locally,
/// `Some(false)` = run over ssh, `None` = invalid alias.
fn target_for(alias: &str) -> Option<bool> {
    if alias == LOCAL_ID {
        Some(true)
    } else if safe_name(alias) {
        Some(false)
    } else {
        None
    }
}

/// A `sh -c <cmd>` command to run in a local PTY.
fn local_sh(cmd: &str) -> CommandBuilder {
    let mut c = CommandBuilder::new("sh");
    c.arg("-c");
    c.arg(cmd);
    c.env("TERM", "xterm-256color");
    if let Ok(home) = std::env::var("HOME") {
        c.cwd(home);
    }
    c
}
