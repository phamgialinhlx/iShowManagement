//! Port forwarding: `ssh -N -L 127.0.0.1:<local>:127.0.0.1:<remote>` over the
//! shared connection, making a server-local service reachable on localhost.
//! Mirrors the forward half of `references/tsmanager/server/managers.js`.

use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::api::{AppState, ForwardEntry, LOCAL_ID};
use crate::bg::BgSsh;
use crate::net::is_port_open;
use crate::secrets::pw_key;
use crate::security::safe_name;
use crate::ssh;

fn key(alias: &str, remote: u16) -> String {
    format!("{alias}:{remote}")
}

/// `POST /api/servers/{id}/ports/{port}/forward`
pub async fn forward(
    State(state): State<AppState>,
    Path((id, port)): Path<(String, u16)>,
) -> (StatusCode, Json<Value>) {
    if id == LOCAL_ID || !safe_name(&id) {
        return err(StatusCode::BAD_REQUEST, "already local — nothing to forward");
    }
    if port < 1 {
        return err(StatusCode::BAD_REQUEST, "bad port");
    }
    let fkey = key(&id, port);

    // Reuse a live forward.
    {
        let map = state.forwards.lock().unwrap();
        if let Some(e) = map.get(&fkey) {
            if !e.proc.exited() {
                return (StatusCode::OK, Json(json!({ "ok": true, "localPort": e.local_port })));
            }
        }
    }

    // Prefer the same local port; fall back to an offset if it's taken.
    let mut local = port;
    if is_port_open(local).await {
        local = 20000 + (port % 10000);
    }
    if is_port_open(local).await {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("local ports {port} and {local} are both in use"),
        );
    }

    let cmd = ssh::forward_command(&id, local, port);
    let password = state.secrets.get(&pw_key(&id));
    let proc = match BgSsh::spawn(cmd, password) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("spawn failed: {e}")),
    };

    // Wait for the local end to accept connections (8s), or fail with output.
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if proc.exited() {
            let msg = proc.recent();
            let hint = if msg.is_empty() { "Open the Console first to authenticate." } else { &msg };
            return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("forward failed. {hint}"));
        }
        if is_port_open(local).await {
            state.forwards.lock().unwrap().insert(fkey, ForwardEntry { local_port: local, proc });
            return (StatusCode::OK, Json(json!({ "ok": true, "localPort": local })));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // `proc` drops here → ssh killed.
    err(StatusCode::INTERNAL_SERVER_ERROR, "Timed out starting the forward.")
}

/// `DELETE /api/servers/{id}/ports/{port}/forward`
pub async fn unforward(
    State(state): State<AppState>,
    Path((id, port)): Path<(String, u16)>,
) -> (StatusCode, Json<Value>) {
    // Removing from the map drops the BgSsh → kills the ssh process.
    state.forwards.lock().unwrap().remove(&key(&id, port));
    (StatusCode::OK, Json(json!({ "ok": true })))
}

fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}
