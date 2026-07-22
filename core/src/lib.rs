//! iShowManagement core library — the axum server plus all feature modules.
//!
//! The `core` binary is a thin wrapper over [`serve`]; the `desktop` (Tauri)
//! shell embeds the same function to run the server in-process. See
//! `plans/rust-port.md`.

mod api;
mod bg;
mod browser;
mod discovery;
mod files;
mod forward;
mod managers;
mod net;
mod notify;
mod pty;
mod secrets;
mod security;
mod ssh;
mod store;
mod ws;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use api::AppState;
use secrets::SecretStore;
use store::Store;

/// Default loopback port.
pub const DEFAULT_PORT: u16 = 7070;

/// A native notification handed to the host-registered sender.
pub struct NativeNotification {
    pub title: String,
    pub body: String,
    pub subtitle: Option<String>,
}

type Notifier = Box<dyn Fn(NativeNotification) -> bool + Send + Sync>;
static NOTIFIER: std::sync::OnceLock<Notifier> = std::sync::OnceLock::new();

/// Register the host's native notification sender. The Tauri shell installs one
/// backed by `tauri-plugin-notification` (posts under our app bundle, correct
/// icon, click focuses the app). When none is registered — the standalone `core`
/// binary in dev — [`api::notify`] falls back to `osascript`. First call wins.
pub fn set_notifier<F>(f: F)
where
    F: Fn(NativeNotification) -> bool + Send + Sync + 'static,
{
    let _ = NOTIFIER.set(Box::new(f));
}

/// Deliver via the registered notifier; `false` if none is set or it failed.
pub(crate) fn dispatch_notification(n: NativeNotification) -> bool {
    NOTIFIER.get().map(|f| f(n)).unwrap_or(false)
}

/// Build the router and serve on `127.0.0.1:<port>` until Ctrl-C. Loopback only —
/// this app never binds a routable interface (see security ADR).
pub async fn serve(port: u16) -> anyhow::Result<()> {
    // Per-user data dir survives version upgrades (macOS: ~/Library/Application Support/).
    let data_dir = dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("iShowManagement");

    let state = AppState {
        store: Arc::new(Mutex::new(Store::load(&data_dir))),
        secrets: Arc::new(SecretStore::open(&data_dir)),
        servers: Arc::new(Mutex::new(Vec::new())),
        forwards: Arc::new(Mutex::new(HashMap::new())),
        proxies: Arc::new(Mutex::new(HashMap::new())),
    };

    // Populate the server list from ~/.ssh/config before serving.
    *state.servers.lock().unwrap() = discovery::discover().await;
    tracing::info!("discovered {} ssh host(s)", state.servers.lock().unwrap().len());

    let static_dir = static_dir();
    let index = static_dir.join("index.html");
    // SPA: serve built assets; for unknown paths fall back to index.html (200).
    let serve_dir = ServeDir::new(&static_dir).fallback(ServeFile::new(&index));

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/servers", get(api::get_servers))
        .route("/api/tunnels", get(api::get_tunnels))
        .route("/api/notify", post(api::notify))
        .route("/api/watching", post(notify::set_watching))
        .route("/api/servers/refresh", post(api::refresh_servers))
        .route("/api/servers/{id}/hidden", post(api::set_hidden))
        .route("/api/servers/{id}/touch", post(api::touch))
        .route(
            "/api/servers/{id}/password",
            put(api::set_password).delete(api::delete_password),
        )
        .route("/api/servers/{id}/overview", get(managers::overview))
        .route("/api/servers/{id}/docker", get(managers::docker))
        .route("/api/servers/{id}/tmux", get(managers::tmux))
        .route("/api/servers/{id}/docker/stats", get(managers::docker_stats))
        .route("/api/servers/{id}/docker/{cid}/{action}", post(managers::docker_action))
        .route("/api/servers/{id}/ports", get(managers::ports))
        .route("/api/servers/{id}/processes", get(managers::processes))
        .route("/api/servers/{id}/kill", post(managers::kill))
        .route("/api/servers/{id}/files", get(files::list))
        .route("/api/servers/{id}/files/view", get(files::view))
        .route("/api/servers/{id}/files/download", get(files::download))
        .route(
            "/api/servers/{id}/ports/{port}/forward",
            post(forward::forward).delete(forward::unforward),
        )
        .route(
            "/api/servers/{id}/claude-notify",
            get(notify::status).post(notify::install).delete(notify::uninstall),
        )
        .route("/api/servers/{id}/claude-notify/events", get(notify::events))
        .route("/ws/notify", get(notify::notify_ws))
        .route("/api/servers/{id}/browser", post(browser::browser))
        .route("/api/servers/{id}/proxy", delete(browser::stop_proxy))
        .route("/ws", get(ws::handler))
        .with_state(state)
        .fallback_service(serve_dir)
        .layer(middleware::from_fn(origin_guard))
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("serving {} on http://{}", static_dir.display(), addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Where the Svelte build lives. `STATIC_DIR` overrides; otherwise default to
/// `web/dist` resolved from this crate's location at compile time.
fn static_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STATIC_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core has a parent dir")
        .join("web")
        .join("dist")
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "ishowmanagement-core" }))
}

/// Reject browser requests whose `Origin` isn't loopback — a remote page must
/// not be able to drive the local server. Absent Origin (curl, same-origin nav)
/// is allowed. Pairs with the loopback-only bind.
async fn origin_guard(req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if security::is_allowed_origin(origin) {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "forbidden origin").into_response()
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
