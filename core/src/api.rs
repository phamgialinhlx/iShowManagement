//! REST API + shared application state for the sidebar: list servers (from ssh
//! config), toggle a host's `hidden` flag, and store/clear its SSH password.
//! Passwords are write-only over the wire — responses expose `hasPassword`, never
//! the value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::bg::BgSsh;
use crate::discovery::{self, ResolvedHost};
use crate::secrets::{pw_key, SecretStore};
use crate::security::safe_name;
use crate::store::Store;

/// The synthetic id for the app's own machine (Phase 1 local console).
pub const LOCAL_ID: &str = "__local__";

/// A live `ssh -L` port forward, keyed by `<alias>:<remote_port>`.
pub struct ForwardEntry {
    pub local_port: u16,
    pub proc: BgSsh,
}

/// A live `ssh -D` SOCKS proxy, keyed by alias.
pub struct ProxyEntry {
    pub port: u16,
    pub proc: BgSsh,
}

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub secrets: Arc<SecretStore>,
    pub servers: Arc<Mutex<Vec<ResolvedHost>>>,
    pub forwards: Arc<Mutex<HashMap<String, ForwardEntry>>>,
    pub proxies: Arc<Mutex<HashMap<String, ProxyEntry>>>,
}

#[derive(Serialize)]
pub struct ServerDto {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    #[serde(rename = "proxyJump")]
    pub proxy_jump: Option<String>,
    pub hidden: bool,
    #[serde(rename = "hasPassword")]
    pub has_password: bool,
    #[serde(rename = "socksIndex")]
    pub socks_index: u32,
    #[serde(rename = "isLocal")]
    pub is_local: bool,
    #[serde(rename = "lastAccessed")]
    pub last_accessed: Option<u64>,
}

fn build_dtos(state: &AppState) -> Vec<ServerDto> {
    let mut store = state.store.lock().unwrap();
    let servers = state.servers.lock().unwrap();
    let local_last_accessed = store.last_accessed(LOCAL_ID);

    // The local pseudo-host is always first and always visible.
    let mut out = vec![ServerDto {
        id: LOCAL_ID.to_string(),
        name: "Local".to_string(),
        host: "localhost".to_string(),
        user: std::env::var("USER").unwrap_or_default(),
        port: 0,
        proxy_jump: None,
        hidden: false,
        has_password: false,
        socks_index: 0,
        is_local: true,
        last_accessed: local_last_accessed,
    }];

    for h in servers.iter() {
        let st = store.ensure(&h.alias); // assigns a stable socks index on first sight
        out.push(ServerDto {
            id: h.alias.clone(),
            name: h.alias.clone(),
            host: h.hostname.clone(),
            user: h.user.clone(),
            port: h.port,
            proxy_jump: h.proxy_jump.clone(),
            hidden: st.hidden,
            has_password: state.secrets.has(&pw_key(&h.alias)),
            socks_index: st.socks_index,
            is_local: false,
            last_accessed: st.last_accessed,
        });
    }
    out
}

/// `GET /api/servers` — the sidebar list (includes hidden entries; the UI filters).
pub async fn get_servers(State(state): State<AppState>) -> Json<Vec<ServerDto>> {
    Json(build_dtos(&state))
}

/// `POST /api/servers/refresh` — re-read ssh config and re-resolve, then return the list.
pub async fn refresh_servers(State(state): State<AppState>) -> Json<Vec<ServerDto>> {
    let fresh = discovery::discover().await;
    *state.servers.lock().unwrap() = fresh;
    Json(build_dtos(&state))
}

#[derive(Deserialize)]
pub struct HiddenReq {
    pub hidden: bool,
}

/// `POST /api/servers/{alias}/hidden` — show/hide a host in the sidebar.
pub async fn set_hidden(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Json(req): Json<HiddenReq>,
) -> StatusCode {
    if !safe_name(&alias) {
        return StatusCode::BAD_REQUEST;
    }
    state.store.lock().unwrap().set_hidden(&alias, req.hidden);
    StatusCode::NO_CONTENT
}

/// `POST /api/servers/{alias}/touch` — stamp a host as just-accessed so the
/// sidebar's recency order persists across restarts.
pub async fn touch(State(state): State<AppState>, Path(alias): Path<String>) -> StatusCode {
    if alias != LOCAL_ID && !safe_name(&alias) {
        return StatusCode::BAD_REQUEST;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    state.store.lock().unwrap().touch(&alias, now);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
pub struct PasswordReq {
    pub password: String,
}

/// `PUT /api/servers/{alias}/password` — store an SSH password (encrypted).
pub async fn set_password(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Json(req): Json<PasswordReq>,
) -> StatusCode {
    if !safe_name(&alias) {
        return StatusCode::BAD_REQUEST;
    }
    match state.secrets.set(&pw_key(&alias), &req.password) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(e) => {
            tracing::error!("store password for {alias}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Serialize)]
pub struct ForwardDto {
    pub alias: String,
    #[serde(rename = "remotePort")]
    pub remote_port: u16,
    #[serde(rename = "localPort")]
    pub local_port: u16,
}

#[derive(Serialize)]
pub struct ProxyDto {
    pub alias: String,
    pub port: u16,
}

#[derive(Serialize)]
pub struct TunnelsDto {
    pub forwards: Vec<ForwardDto>,
    pub proxies: Vec<ProxyDto>,
}

/// `GET /api/tunnels` — every live `ssh -L` forward and `ssh -D` SOCKS proxy across
/// all hosts, for the status bar / home dashboard. Dead entries are skipped.
pub async fn get_tunnels(State(state): State<AppState>) -> Json<TunnelsDto> {
    let mut forwards = Vec::new();
    {
        let map = state.forwards.lock().unwrap();
        for (k, e) in map.iter() {
            if e.proc.exited() {
                continue;
            }
            // Forwards are keyed `<alias>:<remote_port>` (alias never contains ':').
            if let Some((alias, remote)) = k.rsplit_once(':') {
                if let Ok(remote_port) = remote.parse::<u16>() {
                    forwards.push(ForwardDto {
                        alias: alias.to_string(),
                        remote_port,
                        local_port: e.local_port,
                    });
                }
            }
        }
    }
    let mut proxies = Vec::new();
    {
        let map = state.proxies.lock().unwrap();
        for (alias, e) in map.iter() {
            if e.proc.exited() {
                continue;
            }
            proxies.push(ProxyDto { alias: alias.clone(), port: e.port });
        }
    }
    forwards.sort_by(|a, b| a.alias.cmp(&b.alias).then(a.remote_port.cmp(&b.remote_port)));
    proxies.sort_by(|a, b| a.alias.cmp(&b.alias));
    Json(TunnelsDto { forwards, proxies })
}

#[derive(Serialize)]
pub struct PortFreeDto {
    pub free: bool,
}

/// `GET /api/local-port-free/{port}` — is a local TCP port free to bind? Powers the
/// manual-forward form's live local-port suggestion (the browser can't probe this).
pub async fn local_port_free(Path(port): Path<u16>) -> Json<PortFreeDto> {
    Json(PortFreeDto { free: !crate::net::is_port_open(port).await })
}

#[derive(Deserialize)]
pub struct NotifyReq {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub subtitle: Option<String>,
}

/// `POST /api/notify` — raise a native macOS banner. Kept for direct/manual use;
/// Claude events are now delivered straight from `notify::notify_stream` so they
/// fire even while the webview is suspended (backgrounded).
pub async fn notify(Json(req): Json<NotifyReq>) -> StatusCode {
    if deliver_banner(req.title, req.body, req.subtitle).await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

/// Raise a native banner, returning whether it was delivered. Prefers the
/// host-registered notifier (the Tauri shell's `UNUserNotificationCenter` — our
/// bundle icon, click focuses the app); falls back to `osascript` (Script Editor
/// icon, but functional) when unregistered or macOS denied authorization (an
/// ad-hoc-signed build). Runs in the native process, so it works while the app
/// window is backgrounded.
pub(crate) async fn deliver_banner(title: String, body: String, subtitle: Option<String>) -> bool {
    let native = {
        let n = crate::NativeNotification {
            title: title.clone(),
            body: body.clone(),
            subtitle: subtitle.clone(),
        };
        tokio::task::spawn_blocking(move || crate::dispatch_notification(n))
            .await
            .unwrap_or(false)
    };
    if native {
        return true;
    }
    osascript_notify(&title, &body, subtitle.as_deref()).await
}

/// AppleScript fallback for the banner. Platform-specific, so it is split out:
/// there is no portable `display notification`, and on other platforms the
/// host-registered notifier above is the only delivery path.
#[cfg(target_os = "macos")]
async fn osascript_notify(title: &str, body: &str, subtitle: Option<&str>) -> bool {
    fn esc(s: &str) -> String {
        s.chars()
            .filter(|c| !c.is_control())
            .take(400)
            .flat_map(|c| match c {
                '"' => vec!['\\', '"'],
                '\\' => vec!['\\', '\\'],
                other => vec![other],
            })
            .collect()
    }
    let mut script = format!(
        "display notification \"{}\" with title \"{}\"",
        esc(body),
        esc(title),
    );
    if let Some(sub) = subtitle {
        script.push_str(&format!(" subtitle \"{}\"", esc(sub)));
    }
    script.push_str(" sound name \"Ping\"");

    match tokio::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .await
    {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            tracing::warn!("osascript notify failed: {}", String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(e) => {
            tracing::warn!("osascript spawn failed: {e}");
            false
        }
    }
}

/// Non-macOS: no portable banner mechanism, so report the notification as
/// undelivered rather than spawning a binary that does not exist.
#[cfg(not(target_os = "macos"))]
async fn osascript_notify(_title: &str, _body: &str, _subtitle: Option<&str>) -> bool {
    false
}

/// `DELETE /api/servers/{alias}/password` — clear a stored password.
pub async fn delete_password(
    State(state): State<AppState>,
    Path(alias): Path<String>,
) -> StatusCode {
    if !safe_name(&alias) {
        return StatusCode::BAD_REQUEST;
    }
    match state.secrets.delete(&pw_key(&alias)) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(e) => {
            tracing::error!("delete password for {alias}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
