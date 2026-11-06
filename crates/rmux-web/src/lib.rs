//! rmux-web: rmux's own UI and command table, served over HTTP and a WebSocket.
//!
//! `docs/rmux-web.md` is the design and the wire protocol; `ui/src/lib/bridge.ts`
//! is the other end of it. This crate is a *transport* for
//! [`rmux_app::Registry`] and nothing more — it implements no command of its own,
//! and it **spawns no process**: every child process belongs to a command in
//! rmux-app, where the `no_console_window()` rule is enforced.
//!
//! Routes:
//!
//! | route                     | auth                       | |
//! |---------------------------|----------------------------|-|
//! | `GET  /api/ws`            | Access, bearer or `?token=`| upgrade, same-origin only |
//! | `POST /api/invoke/{cmd}`  | Access or bearer           | `Web::Yes` commands only |
//! | `GET  /api/file?path=`    | Access, bearer or `?token=`| the backgrounds dir only |
//! | `GET  /api/seed`          | Access or bearer           | `<data dir>/seed.json` |
//! | anything else under /api  | Access or bearer           | 404 |
//! | everything else           | none                       | the static UI |
//!
//! "Access" is a verified Cloudflare Access assertion ([`access`]). The UI
//! bundle is not secret — the page must load before it can read its token out
//! of the fragment — so it is served without auth. Everything that *does*
//! something is under `/api`, behind [`auth::authorize`].
//!
//! **Nothing here logs a request body, a query string or the token.** Bodies
//! carry file contents; query strings carry the token on two routes. That is
//! also why there is no generic HTTP trace layer: it would log the URI.

pub mod access;
pub mod auth;
pub mod cli;
pub mod conns;
pub mod host;

mod files;
mod seed;
mod ui;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use futures::{SinkExt as _, StreamExt as _};
use rmux_app::{Call, Ctx, Registry, Web};
use serde::Deserialize;

use crate::auth::Auth;
use crate::conns::{ConnResolver, Conns};
use crate::host::WebHost;

/// The header a page names its WebSocket connection in.
pub const CONN_HEADER: &str = "x-rmux-conn";

/// Largest invoke body. Saves and uploads go through invoke, so this is set by
/// the largest file anyone edits from a phone, not by a typical call.
pub const MAX_INVOKE_BODY: usize = 64 * 1024 * 1024;

/// A WebSocket keepalive. cloudflared and most proxies drop an idle socket
/// after a minute or two; an idle terminal is the normal case.
const PING_EVERY: Duration = Duration::from_secs(25);

/// Everything needed to build the server. The registry is passed in, not
/// built here, so tests can serve a registry with extra test-only commands.
pub struct Options {
    pub registry: Registry,
    /// The built UI (`dist/`). `None` serves `/api` only.
    pub ui_dir: Option<PathBuf>,
    pub resource_dir: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub token: String,
    /// Cloudflare Access verification, when configured. The token stays valid
    /// beside it for requests that carry no assertion.
    pub access: Option<Arc<access::Access>>,
    /// Per-connection outbound queue, in frames. See [`conns`].
    pub queue_capacity: usize,
}

/// Process-wide state: one [`Ctx`], one registry, built once.
pub struct AppState {
    ctx: Arc<Ctx>,
    registry: Arc<Registry>,
    auth: Auth,
    conns: Arc<Conns>,
    ui_dir: Option<PathBuf>,
    backgrounds: PathBuf,
    seed: PathBuf,
}

impl AppState {
    pub fn conns(&self) -> &Arc<Conns> {
        &self.conns
    }
}

/// Build the router. Creates the data and cache directories (`0700`).
pub fn build(options: Options) -> anyhow::Result<(Router, Arc<AppState>)> {
    host::ensure_private_dir(&options.data_dir)?;
    host::ensure_private_dir(&options.cache_dir)?;
    if let Some(ui) = &options.ui_dir {
        anyhow::ensure!(
            ui.join("index.html").is_file(),
            "--ui {}: no index.html there (expected vite's dist/)",
            ui.display()
        );
    }

    let conns = Arc::new(Conns::new(options.queue_capacity));
    let backgrounds = options.data_dir.join("backgrounds");
    let seed = options.data_dir.join(seed::FILE_NAME);
    let host = WebHost::new(options.resource_dir, options.data_dir, options.cache_dir, conns.clone());
    let state = Arc::new(AppState {
        ctx: Arc::new(Ctx::new(Arc::new(host))),
        registry: Arc::new(options.registry),
        auth: Auth::token(&options.token).with_access(options.access),
        conns,
        ui_dir: options.ui_dir,
        backgrounds,
        seed,
    });

    let api = Router::new()
        .route("/api/ws", get(ws_upgrade))
        .route("/api/invoke/{cmd}", post(invoke))
        .route("/api/file", get(files::serve))
        .route("/api/seed", get(seed::serve))
        .route("/api", any(api_not_found))
        .route("/api/{*rest}", any(api_not_found))
        .layer(DefaultBodyLimit::max(MAX_INVOKE_BODY))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let router = api.fallback(ui::serve).with_state(state.clone());
    Ok((router, state))
}

/// Serve until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown)
        .await
}

/// `{"message": ...}` with a status — the error shape `bridge.ts` reads.
pub(crate) fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, axum::Json(serde_json::json!({ "message": message.into() }))).into_response()
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// The auth gate in front of every `/api` route. The decision itself is
/// [`auth::authorize`]; this only extracts its inputs.
async fn require_auth(State(state): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path();
    // A browser cannot put a header on a WS upgrade or an <img>; nowhere else
    // needs the query form, so nowhere else accepts it.
    let query_allowed = path == "/api/ws" || path == "/api/file";
    let query_token = Query::<TokenQuery>::try_from_uri(req.uri()).ok().and_then(|q| q.0.token);

    match auth::authorize(&state.auth, req.headers(), query_token.as_deref(), query_allowed).await {
        Ok(principal) => {
            req.extensions_mut().insert(principal);
            next.run(req).await
        }
        Err(denial) => {
            // One line per refusal. The path, never the query (it may hold a
            // wrong token), and a fixed reason — never the token or the JWT.
            tracing::info!(path = %req.uri().path(), denied = ?denial.kind, reason = denial.reason, "refused a request");
            error(
                StatusCode::UNAUTHORIZED,
                "not authorised — sign in through Cloudflare Access (reload the page), \
                 or open the URL rmux-web printed at startup",
            )
        }
    }
}

async fn api_not_found() -> Response {
    error(StatusCode::NOT_FOUND, "no such endpoint")
}

/// `POST /api/invoke/{cmd}`.
///
/// Only `Web::Yes` commands are dispatched. A `Web::No` command answers exactly
/// as an unknown one does — 404, the text Tauri uses for an unknown command —
/// and is never run: `run_on_target` is `Web::No` because over HTTP it would be
/// an `exec` endpoint.
async fn invoke(
    State(state): State<Arc<AppState>>,
    Path(cmd): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(command) = state.registry.get(&cmd).filter(|c| c.web() == Web::Yes) else {
        tracing::info!(%cmd, "refused a command not served on the web");
        return error(StatusCode::NOT_FOUND, format!("Command {cmd} not found"));
    };

    let args = if body.iter().all(u8::is_ascii_whitespace) {
        serde_json::Value::Null
    } else {
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => v,
            Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid JSON body for `{cmd}`: {e}")),
        }
    };

    let conn = headers.get(CONN_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let resolver = ConnResolver { conns: state.conns.clone(), conn };
    let call = Call::new(state.ctx.clone(), Arc::new(resolver));

    // Spawned, so a page that navigates away mid-call does not cancel a save
    // half-way through — the desktop runs a command to completion too.
    let task = tokio::spawn(command.run(call, args));
    let outcome = task.await;
    match outcome {
        Ok(Ok(value)) => {
            tracing::debug!(%cmd, "ok");
            axum::Json(value).into_response()
        }
        Ok(Err(message)) => {
            tracing::debug!(%cmd, "command returned an error");
            let status = if message.starts_with("invalid args for command") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            error(status, message)
        }
        Err(join) => {
            tracing::error!(%cmd, "command task failed: {join}");
            error(StatusCode::INTERNAL_SERVER_ERROR, format!("{cmd}: the command crashed"))
        }
    }
}

/// Is the upgrade same-origin? `Origin` must be present and its authority must
/// equal `Host`. Browsers always send `Origin` on a WebSocket, and this is the
/// check that stops a page on another site opening a terminal in the
/// operator's browser (cross-site WebSocket hijacking).
pub fn same_origin(headers: &HeaderMap) -> bool {
    let (Some(origin), Some(host)) = (
        headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()),
        headers.get(header::HOST).and_then(|v| v.to_str().ok()),
    ) else {
        return false;
    };
    let Some(authority) = origin.strip_prefix("https://").or_else(|| origin.strip_prefix("http://")) else {
        return false;
    };
    let authority = authority.trim_end_matches('/');
    !authority.is_empty() && authority.eq_ignore_ascii_case(host)
}

/// `GET /api/ws`.
async fn ws_upgrade(State(state): State<Arc<AppState>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !same_origin(&headers) {
        tracing::warn!("refused a cross-origin WebSocket upgrade");
        return error(StatusCode::FORBIDDEN, "cross-origin WebSocket refused");
    }
    // The page sends nothing but control frames; a large inbound message is
    // not a client of ours.
    ws.max_message_size(64 * 1024).on_upgrade(move |socket| run_socket(state, socket))
}

async fn run_socket(state: Arc<AppState>, socket: WebSocket) {
    let (conn, mut queue) = state.conns.open();
    let id = conn.id().to_owned();
    let (mut tx, mut rx) = socket.split();
    tracing::info!(conn = %id, "page connected");

    let hello = serde_json::json!({ "hello": { "conn": id } }).to_string();
    if tx.send(Message::Text(hello.into())).await.is_ok() {
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        loop {
            let outgoing = tokio::select! {
                () = conn.cancelled() => break,
                m = queue.recv() => match m { Some(m) => m, None => break },
                _ = ping.tick() => Message::Ping(Bytes::new()),
                incoming = rx.next() => match incoming {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue, // pongs; the page sends nothing else
                },
            };
            // A socket whose TCP window is shut blocks here; the producers'
            // bounded queue fills and cancels it, which ends this wait too.
            tokio::select! {
                () = conn.cancelled() => break,
                r = tx.send(outgoing) => if r.is_err() { break },
            }
        }
    }

    state.conns.remove(&id);
    // Bounded: closing a socket whose peer stopped reading must not hang the task.
    let _ = tokio::time::timeout(Duration::from_secs(2), tx.close()).await;
    tracing::info!(conn = %id, "page disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::testkit::*;
    use axum::body::Body;
    use tower::ServiceExt as _;

    const TOKEN: &str = "lib-test-token-0123456789";

    fn router(dir: &std::path::Path) -> Router {
        build(Options {
            registry: Registry::default(),
            ui_dir: None,
            resource_dir: None,
            data_dir: dir.join("data"),
            cache_dir: dir.join("cache"),
            token: TOKEN.into(),
            access: Some(verifier(&["alice@example.com"])),
            queue_capacity: 8,
        })
        .unwrap()
        .0
    }

    async fn status(router: &Router, uri: &str, headers: &[(&str, String)]) -> u16 {
        let mut req = axum::http::Request::get(uri);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        router.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap().status().as_u16()
    }

    /// Every `/api` route goes through the Access check — header and cookie —
    /// and a bad assertion is refused there even beside a good token.
    #[tokio::test]
    async fn access_assertions_gate_every_api_route() {
        let dir = tempfile::tempdir().unwrap();
        let r = router(dir.path());
        let good = sign(KEY_A, "kid-a", &claims("alice@example.com"));
        let forged = sign(KEY_B, "kid-a", &claims("alice@example.com"));
        let stranger = sign(KEY_A, "kid-a", &claims("someone@else.com"));
        let hdr = |j: &str| ("cf-access-jwt-assertion", j.to_owned());
        let cookie = |j: &str| ("cookie", format!("CF_Authorization={j}"));
        let bearer = ("authorization", format!("Bearer {TOKEN}"));

        // Authorised → past the gate: /api/seed is 404 (no file), /api/file 400 (no path).
        assert_eq!(status(&r, "/api/seed", &[hdr(&good)]).await, 404);
        assert_eq!(status(&r, "/api/seed", &[cookie(&good)]).await, 404);
        assert_eq!(status(&r, "/api/file", &[cookie(&good)]).await, 400);
        assert_eq!(status(&r, "/api/nope", &[hdr(&good)]).await, 404);
        assert_eq!(status(&r, "/api/seed", std::slice::from_ref(&bearer)).await, 404);

        for bad in [&forged, &stranger] {
            assert_eq!(status(&r, "/api/seed", &[hdr(bad)]).await, 401);
            assert_eq!(status(&r, "/api/seed", &[cookie(bad)]).await, 401);
            assert_eq!(status(&r, "/api/seed", &[hdr(bad), bearer.clone()]).await, 401);
            assert_eq!(status(&r, &format!("/api/file?token={TOKEN}"), &[cookie(bad)]).await, 401);
        }
        assert_eq!(status(&r, "/api/seed", &[]).await, 401);
    }
}
