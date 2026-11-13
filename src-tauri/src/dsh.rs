//! Showing DeepSeek Harness's Web UI inside an rmux pane.
//!
//! The server half (`rmux_app::dsh`) leaves a `dsh web` listening on the
//! target's loopback and the launch token in memory. This half makes it
//! visible: a loopback forward to that port, then a **child webview** placed
//! over the pane's rectangle and navigated to `http://127.0.0.1:<local>/?token=`.
//!
//! ## Why a child webview and not an `<iframe>`
//!
//! Measured, not assumed, in a real WKWebView with the page at
//! `tauri://localhost` (the production origin): the iframe's token exchange
//! succeeds (`303` + `Set-Cookie`), but the cookie is `SameSite=Strict` and the
//! frame is cross-site, so WebKit never sends it back and dsh answers *"dsh web
//! authentication required"*. That is dsh's browser-trust fence working as
//! designed (its decision record: DNS-rebinding and cross-site defence), and the
//! only ways round it — a proxy that injects the cookie, or stripping
//! `Sec-Fetch-Site` — would switch that fence off for every local page. A child
//! webview navigated *directly* is a top-level, first-party browsing context:
//! same probe, full UI. So the pane is a native webview that follows the DOM
//! rect the pane reports.
//!
//! ## Constraints that keep this from becoming an in-app browser
//!
//! - **Loopback only.** The URL is built here from the forward's local port;
//!   the UI never supplies one, so no arbitrary URL can be loaded.
//! - **No navigation off that origin.** `on_navigation` admits only
//!   `http://127.0.0.1:<local>`; a link elsewhere opens in the real browser if it
//!   is `https`, and is dropped otherwise. New windows likewise.
//! - **No IPC.** The page is a remote origin and Tauri 2.12 rejects every app
//!   command from a remote origin unless a capability's `remote` grants it —
//!   none does. dsh's page can never call rmux.
//! - **The token stays in Rust.** It is read from the server's output, used to
//!   build the URL and the status probe's cookie, and never sent to the UI.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_app::TargetRef;
use rmux_ssh::forward::{ForwardState, Forwards};
use rmux_transport::{NoConsoleWindow as _, TargetId};
use serde::{Deserialize, Serialize};
use tauri::{LogicalPosition, LogicalSize, Manager, Runtime, WebviewUrl};

/// One open dsh view, keyed by the rmux session id.
struct View {
    /// Child webview label (`dsh-<session id>`).
    label: String,
    /// `user@host` for an SSH target, `None` for this machine.
    host: Option<String>,
    remote: u16,
    local: u16,
    token: String,
    /// The status probe's own cookie, from its own token exchange.
    cookie: Option<String>,
}

impl View {
    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.local)
    }
    fn url(&self) -> String {
        format!("{}/?token={}", self.origin(), self.token)
    }
}

/// Every dsh view and the forwards behind them. Lives in the shared `Ctx` so
/// `server::disconnect` can evict it alongside the other caches.
pub struct DshViews {
    views: Mutex<HashMap<String, View>>,
    forwards: Arc<Forwards>,
    /// Labels whose webviews a disconnect orphaned; the Tauri command that has
    /// an `AppHandle` closes them (the testable `disconnect` has only a `Ctx`).
    orphaned: Mutex<Vec<String>>,
    http: reqwest::Client,
}

impl Default for DshViews {
    fn default() -> Self {
        Self {
            views: Mutex::default(),
            forwards: Arc::new(Forwards::new()),
            orphaned: Mutex::default(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(4))
                .no_proxy()
                .build()
                .unwrap_or_default(),
        }
    }
}

impl DshViews {
    /// Drop every view and forward on `host`. True when anything was held — the
    /// count `server_disconnect` reports.
    pub async fn evict_host(&self, id: &TargetId) -> bool {
        let TargetId::Ssh(host) = id else { return false };
        let host = host.label();
        let labels: Vec<String> = {
            let mut views = self.views.lock();
            let keys: Vec<String> = views
                .iter()
                .filter(|(_, v)| v.host.as_deref() == Some(host.as_str()))
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| views.remove(k)).map(|v| v.label).collect()
        };
        let forwards = self.forwards.stop_loopbacks_for(&host).await;
        let any = !labels.is_empty() || forwards > 0;
        self.orphaned.lock().extend(labels);
        any
    }

    fn take_orphaned(&self) -> Vec<String> {
        std::mem::take(&mut *self.orphaned.lock())
    }
}

fn views<R: Runtime>(app: &tauri::AppHandle<R>) -> Arc<DshViews> {
    crate::bridge::store::<DshViews, R>(app)
}

/// Close the webviews a disconnect left behind. Called by `server_disconnect`.
pub fn close_orphaned<R: Runtime>(app: &tauri::AppHandle<R>) {
    for label in views(app).take_orphaned() {
        if let Some(w) = app.get_webview(&label) {
            let _ = w.close();
        }
    }
}

/// The same connection string the other tunnels dial (see `tunnels::conn`).
fn conn(target: &TargetRef) -> Option<String> {
    match target.id() {
        TargetId::Ssh(host) => Some(host.label()),
        TargetId::Local => None,
    }
}

fn label_for(session_id: &str) -> String {
    // Labels admit `a-zA-Z0-9-/:_`; session ids are minted from that set, but
    // an adopted one is a host's name for it, so anything else becomes `_`.
    let clean: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("dsh-{clean}")
}

/// Is `url` on the view's own origin (or an inert scheme a page uses itself)?
fn same_origin(url: &tauri::Url, origin: &str) -> bool {
    match url.scheme() {
        "about" | "blob" | "data" => true,
        "http" => url.origin().ascii_serialization() == origin,
        _ => false,
    }
}

/// Hand an `https` link to the real browser; anything else is dropped.
fn open_outside(url: &tauri::Url) {
    if url.scheme() != "https" {
        tracing::info!(%url, "dsh view: refused a non-https navigation off its origin");
        return;
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).no_console_window().arg(url.as_str()).spawn();
}

#[derive(Deserialize, Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    /// The local end — shown in the pane header ("127.0.0.1:52011 → host:41234").
    pub local_port: u16,
    pub remote_port: u16,
}

/// Forward (if remote) and show the dsh UI for `session_id` over `rect`.
///
/// Idempotent and self-healing: called again after a network drop, it finds the
/// forward dead, dials a new one, and re-navigates the webview to the new local
/// port. The token is unchanged (same server process), so the page simply
/// re-authenticates.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn dsh_view_open<R: Runtime>(
    app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    session_id: String,
    attach_id: String,
    target: TargetRef,
    rect: Rect,
    visible: bool,
) -> Result<Opened, String> {
    let ctx = crate::bridge::ctx(&app);
    let endpoint = ctx
        .store::<rmux_app::dsh::DshStore>()
        .endpoint(&attach_id)
        .ok_or_else(|| "DeepSeek Harness has not printed its address yet".to_owned())?;
    let store = views(&app);
    let host = conn(&target);

    let local = match &host {
        Some(h) => {
            let f = store.forwards.loopback(h, endpoint.port).await;
            if f.state != ForwardState::Active {
                return Err(format!(
                    "could not forward the Web UI from the server: {}",
                    f.error.unwrap_or_else(|| "ssh exited".into())
                ));
            }
            f.port
        }
        None => endpoint.port,
    };

    let label = label_for(&session_id);
    let (url, origin, changed) = {
        let mut views = store.views.lock();
        let changed = views
            .get(&session_id)
            .is_none_or(|v| v.local != local || v.token != endpoint.token);
        let view = View {
            label: label.clone(),
            host: host.clone(),
            remote: endpoint.port,
            local,
            token: endpoint.token.clone(),
            cookie: if changed { None } else { views.get(&session_id).and_then(|v| v.cookie.clone()) },
        };
        let out = (view.url(), view.origin(), changed);
        views.insert(session_id.clone(), view);
        out
    };
    let url: tauri::Url = url.parse().map_err(|e| format!("{e}"))?;
    let position = LogicalPosition::new(rect.x, rect.y);
    let size = LogicalSize::new(rect.width.max(1.0), rect.height.max(1.0));

    match app.get_webview(&label) {
        Some(existing) => {
            if changed {
                existing.navigate(url).map_err(|e| e.to_string())?;
            }
            let _ = existing.set_position(position);
            let _ = existing.set_size(size);
            if visible { existing.show() } else { existing.hide() }.map_err(|e| e.to_string())?;
        }
        None => {
            let nav_origin = origin.clone();
            let win_origin = origin.clone();
            let builder = tauri::webview::WebviewBuilder::new(&label, WebviewUrl::External(url))
                .on_navigation(move |u| {
                    let ok = same_origin(u, &nav_origin);
                    if !ok {
                        open_outside(u);
                    }
                    ok
                })
                .on_new_window(move |u, _| {
                    if same_origin(&u, &win_origin) {
                        tracing::info!("dsh view: new window on its own origin refused");
                    } else {
                        open_outside(&u);
                    }
                    tauri::webview::NewWindowResponse::Deny
                });
            let child = webview
                .window()
                .add_child(builder, position, size)
                .map_err(|e| format!("could not create the DeepSeek Harness view: {e}"))?;
            if !visible {
                let _ = child.hide();
            }
        }
    }

    Ok(Opened { local_port: local, remote_port: endpoint.port })
}

/// Follow the pane: new rectangle, or hidden (another view, a modal, unmount).
/// False when there is no view — the pane then calls `dsh_view_open` again.
#[tauri::command]
pub async fn dsh_view_bounds<R: Runtime>(
    app: tauri::AppHandle<R>,
    session_id: String,
    rect: Rect,
    visible: bool,
) -> Result<bool, String> {
    let Some(view) = app.get_webview(&label_for(&session_id)) else { return Ok(false) };
    if visible {
        let _ = view.set_position(LogicalPosition::new(rect.x, rect.y));
        let _ = view.set_size(LogicalSize::new(rect.width.max(1.0), rect.height.max(1.0)));
        view.show().map_err(|e| e.to_string())?;
    } else {
        view.hide().map_err(|e| e.to_string())?;
    }
    Ok(true)
}

/// Tear the view down: close the webview, stop the forward. Called when the
/// session is closed *or* detached — either way this machine stops looking.
#[tauri::command]
pub async fn dsh_view_close<R: Runtime>(app: tauri::AppHandle<R>, session_id: String) -> Result<(), String> {
    if let Some(w) = app.get_webview(&label_for(&session_id)) {
        let _ = w.close();
    }
    let store = views(&app);
    let removed = store.views.lock().remove(&session_id);
    if let Some(v) = removed
        && let Some(host) = &v.host
    {
        store.forwards.stop_loopback(host, v.remote).await;
    }
    Ok(())
}

/// Open this session's UI in the operator's real browser. Only ever the URL
/// rmux built for this view — never one the caller names.
#[tauri::command]
pub async fn dsh_open_browser<R: Runtime>(app: tauri::AppHandle<R>, session_id: String) -> Result<(), String> {
    let url = views(&app)
        .views
        .lock()
        .get(&session_id)
        .map(View::url)
        .ok_or_else(|| "the DeepSeek Harness view is not open".to_owned())?;
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    tokio::process::Command::new(program)
        .no_console_window()
        .arg(&url)
        .spawn()
        .map_err(|e| format!("could not open a browser: {e}"))?;
    Ok(())
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    /// The UI answered HTTP through the forward.
    pub reachable: bool,
    /// `working` when any dsh session is running a turn, `waiting` when one has
    /// an unanswered question, `idle` otherwise. Absent when unreadable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
}

/// One light status read through the forward: `session/list`, the same RPC the
/// dsh sidebar uses. Authenticated with the probe's own cookie, minted from the
/// launch token on first use.
#[tauri::command]
pub async fn dsh_probe<R: Runtime>(app: tauri::AppHandle<R>, session_id: String) -> Result<Probe, String> {
    let store = views(&app);
    let Some((origin, url, cookie)) = store
        .views
        .lock()
        .get(&session_id)
        .map(|v| (v.origin(), v.url(), v.cookie.clone()))
    else {
        return Ok(Probe { reachable: false, status: None });
    };

    let cookie = match cookie {
        Some(c) => c,
        None => {
            let Ok(res) = store.http.get(&url).send().await else {
                return Ok(Probe { reachable: false, status: None });
            };
            let Some(c) = res
                .headers()
                .get(reqwest::header::SET_COOKIE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .map(str::to_owned)
            else {
                // Answered, but not with a cookie: up, status unknown.
                return Ok(Probe { reachable: true, status: None });
            };
            if let Some(v) = store.views.lock().get_mut(&session_id) {
                v.cookie = Some(c.clone());
            }
            c
        }
    };

    let body = serde_json::json!({
        "type": "client-request",
        "rpcId": "rmux-status",
        "method": "session/list",
        "payload": { "args": { "_request": {} } },
    });
    let res = store
        .http
        .post(format!("{origin}/api/session/list"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&body)
        .send()
        .await;
    let Ok(res) = res else { return Ok(Probe { reachable: false, status: None }) };
    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        // Expired or minted for an older port: mint again next time.
        if let Some(v) = store.views.lock().get_mut(&session_id) {
            v.cookie = None;
        }
        return Ok(Probe { reachable: true, status: None });
    }
    let value: serde_json::Value = res.json().await.unwrap_or_default();
    Ok(Probe { reachable: true, status: status_of(&value) })
}

/// Map a `session/list` response to a rail status.
///
/// `running` is the Host's own summary flag; an active entry in the
/// `userQuestions` projection is a question waiting on the operator. Only what
/// dsh reports is mapped — a response we cannot read yields `None`, never a
/// guessed "idle".
pub fn status_of(value: &serde_json::Value) -> Option<&'static str> {
    let items = value.pointer("/result/value/items")?.as_array()?;
    let waiting = items.iter().any(|row| {
        row.pointer("/projections/values/userQuestions/active")
            .and_then(|a| a.as_array())
            .is_some_and(|a| !a.is_empty())
    });
    if waiting {
        return Some("waiting");
    }
    if items.iter().any(|row| row.get("running").and_then(|r| r.as_bool()) == Some(true)) {
        return Some("working");
    }
    Some("idle")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `session/list` answer from `dsh web` (0.2, one blank session),
    /// trimmed to the fields read.
    const REAL_IDLE: &str = r#"{"type":"server-response","rpcId":"r1","result":{"ok":true,"value":{"items":[{"sessionId":"session-f7fbcafe","agentAvailable":true,"running":false,"blank":true,"projections":{"kind":"sequenced","asOfSeq":2,"values":{"userQuestions":{"active":[],"settled":[]},"plan":{"active":false,"pending":false}}}}]}}}"#;

    #[test]
    fn session_list_maps_to_rail_status() {
        let idle: serde_json::Value = serde_json::from_str(REAL_IDLE).unwrap();
        assert_eq!(status_of(&idle), Some("idle"));

        let mut working = idle.clone();
        working["result"]["value"]["items"][0]["running"] = true.into();
        assert_eq!(status_of(&working), Some("working"));

        let mut asking = working.clone();
        asking["result"]["value"]["items"][0]["projections"]["values"]["userQuestions"]["active"] =
            serde_json::json!([{"id": "q1"}]);
        assert_eq!(status_of(&asking), Some("waiting"), "a question outranks running");

        let failed: serde_json::Value =
            serde_json::from_str(r#"{"result":{"ok":false,"error":{"code":"gateway/internal"}}}"#).unwrap();
        assert_eq!(status_of(&failed), None, "unreadable is absent, not idle");
    }

    #[test]
    fn navigation_is_held_to_the_views_own_origin() {
        let origin = "http://127.0.0.1:52011";
        let ok = |s: &str| same_origin(&s.parse().unwrap(), origin);
        assert!(ok("http://127.0.0.1:52011/"));
        assert!(ok("http://127.0.0.1:52011/sessions/abc?x=1"));
        assert!(ok("about:blank"));
        assert!(!ok("http://127.0.0.1:52012/"), "another local port is another app");
        assert!(!ok("http://localhost:52011/"), "a different authority, a different cookie jar");
        assert!(!ok("https://example.com/"));
        assert!(!ok("file:///etc/passwd"));
    }

    #[test]
    fn labels_are_valid_and_per_session() {
        assert_eq!(label_for("dshsession-abc-1"), "dsh-dshsession-abc-1");
        assert_eq!(label_for("weird name/π"), "dsh-weird_name__");
    }

    #[tokio::test]
    async fn a_disconnect_evicts_views_on_that_host_only() {
        let store = DshViews::default();
        let view = |host: Option<&str>, label: &str| View {
            label: label.into(),
            host: host.map(str::to_owned),
            remote: 41234,
            local: 52011,
            token: "t".into(),
            cookie: None,
        };
        store.views.lock().insert("a".into(), view(Some("me@box"), "dsh-a"));
        store.views.lock().insert("b".into(), view(Some("me@other"), "dsh-b"));
        store.views.lock().insert("c".into(), view(None, "dsh-c"));

        let host = rmux_transport::SshHostId { user: Some("me".into()), ..rmux_transport::SshHostId::new("box") };
        assert!(store.evict_host(&TargetId::Ssh(host)).await);
        assert_eq!(store.take_orphaned(), vec!["dsh-a".to_owned()]);
        let left: Vec<_> = store.views.lock().keys().cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        assert_eq!(left, vec!["b".to_owned(), "c".to_owned()]);
        assert!(!store.evict_host(&TargetId::Local).await, "a local target has nothing to evict");
    }
}
