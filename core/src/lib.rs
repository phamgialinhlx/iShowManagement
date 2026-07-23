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
use std::sync::{Arc, Mutex};

use axum::extract::Request;
use axum::http::{header, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::json;
use tower_http::trace::TraceLayer;

use api::AppState;
use secrets::SecretStore;
use store::Store;

/// Default loopback port.
pub const DEFAULT_PORT: u16 = 7070;

/// The Svelte build, embedded into the binary at compile time so the app is a
/// single self-contained file — no `web/dist` on disk at runtime. In debug
/// builds rust-embed reads the folder live (fast dev iteration); release builds
/// embed the bytes. The folder is resolved relative to this crate's Cargo.toml.
#[derive(RustEmbed)]
#[folder = "../web/dist"]
struct Assets;

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
/// this app never binds a routable interface (see security ADR). The desktop
/// shell prefers [`serve_on`] so it can own the port before the window loads.
pub async fn serve(port: u16) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("serving embedded frontend on http://{}", addr);
    axum::serve(listener, build_router().await)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Serve on an already-bound listener. The desktop shell binds the socket itself
/// (owning the port, or falling back to a free one) and hands it here — so a stray
/// process on the default port can never make the app load *its* server instead.
pub async fn serve_on(listener: std::net::TcpListener) -> anyhow::Result<()> {
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    tracing::info!("serving embedded frontend on {}", listener.local_addr()?);
    axum::serve(listener, build_router().await)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Assemble the axum app (state, ssh-host discovery, routes, middleware). Shared
/// by [`serve`] and [`serve_on`].
async fn build_router() -> Router {
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
        .route("/api/servers/{id}/tmux/claude", get(notify::claude_inventory))
        .route("/api/servers/{id}/tmux/select", post(managers::tmux_select))
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
        .fallback(static_handler)
        .layer(middleware::from_fn(origin_guard))
        .layer(middleware::from_fn(no_store_dynamic))
        .layer(TraceLayer::new_for_http());

    app
}

/// Serve the embedded SPA. Exact asset hits (`/assets/…`, `/favicon.svg`) return
/// their bytes; any other path falls back to `index.html` with 200 so client-side
/// routing works. `/api` and `/ws` routes are matched before this fallback.
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // Content-hashed assets (`assets/index-<hash>.js`) are safe to cache forever;
    // index.html is served at a fixed URL, so it must NEVER be cached — a stale
    // copy would keep pointing at old asset hashes and the app would appear not to
    // update. WKWebView heuristically caches responses that carry no Cache-Control
    // (exactly this trap), so we always set it. `serve` picks the header from
    // whether we resolved a real asset or fell back to index.html.
    let (file, cache) = match Assets::get(path) {
        Some(f) if path.starts_with("assets/") => (Some(f), "public, max-age=31536000, immutable"),
        Some(f) => (Some(f), "no-cache"),
        None => (Assets::get("index.html"), "no-cache"),
    };
    match file {
        Some(file) => (
            [
                (header::CONTENT_TYPE, file.metadata.mimetype().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
            file.data.into_owned(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "frontend not embedded").into_response(),
    }
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "ishowmanagement-core" }))
}

/// Reject browser requests whose `Origin` isn't loopback — a remote page must
/// not be able to drive the local server. Absent Origin (curl, same-origin nav)
/// is allowed. Pairs with the loopback-only bind.
/// Stamp dynamic responses `Cache-Control: no-store` so the webview never serves
/// stale API/WS data (WKWebView heuristically caches responses that carry no
/// cache directive — that's how a stale `/tmux/claude` hid running Claude panes).
/// Responses that already set `Cache-Control` — the static assets — are left
/// alone, so hashed assets keep `immutable` and index.html keeps `no-cache`.
async fn no_store_dynamic(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    if !res.headers().contains_key(header::CACHE_CONTROL) {
        res.headers_mut()
            .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    }
    res
}

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

#[cfg(test)]
mod tests {
    use super::Assets;

    /// The frontend must be embedded (or readable in debug) — its absence is what
    /// shipped a white screen when the build read `web/dist` from a path that only
    /// existed on the build machine. Guards that regression.
    #[test]
    fn frontend_index_is_available() {
        assert!(
            Assets::get("index.html").is_some(),
            "web/dist/index.html not found — run `npm run build` in web/ before building core"
        );
    }
}
