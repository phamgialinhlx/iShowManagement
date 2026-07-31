//! Server-side browser: a per-host SOCKS proxy (`ssh -N -D 127.0.0.1:<port>`)
//! plus a Chrome instance with an isolated profile pointed at it, so the browser
//! sees the network *as the server does* (including the server's own
//! `127.0.0.1:PORT`). Mirrors `references/tsmanager/server/browser-routes.js`.
//!
//! `ISM_BROWSER` overrides the app name (`open -na <name>`); `ISM_BROWSER_CMD`
//! replaces the whole launcher (used by tests).

use std::process::Stdio;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::{AppState, ProxyEntry, LOCAL_ID};
use crate::bg::BgSsh;
use crate::net::is_port_open;
use crate::secrets::pw_key;
use crate::security::safe_name;
use crate::ssh;

const SOCKS_BASE: u16 = 11080;

#[derive(Deserialize, Default)]
pub struct BrowserReq {
    #[serde(default)]
    url: Option<String>,
}

/// `POST /api/servers/{id}/browser`
pub async fn browser(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    if id == LOCAL_ID || !safe_name(&id) {
        return err(StatusCode::BAD_REQUEST, "this is the local machine — just open a browser normally");
    }
    let port = match ensure_proxy(&state, &id).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };

    // Body is optional; parse leniently for an initial URL.
    let url = serde_json::from_slice::<BrowserReq>(&body)
        .ok()
        .and_then(|b| b.url)
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "about:blank".into());
    let profile = std::env::temp_dir().join(format!("ism-browser-{}", safe_profile(&id)));

    let flags = vec![
        format!("--proxy-server=socks5://127.0.0.1:{port}"),
        "--proxy-bypass-list=<-loopback>".to_string(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        url,
    ];
    if let Err(e) = launch_browser(&flags) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("browser launch failed: {e}"));
    }
    (StatusCode::OK, Json(json!({ "socksPort": port, "launched": true })))
}

/// `DELETE /api/servers/{id}/proxy` — stop a host's SOCKS proxy. Removing the
/// entry drops its `BgSsh`, which kills the `ssh -D` process.
pub async fn stop_proxy(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    state.proxies.lock().unwrap().remove(&id);
    (StatusCode::OK, Json(json!({ "ok": true })))
}

/// Ensure a live SOCKS proxy for `alias`, returning its port. Reuses an existing
/// one if it's still listening.
async fn ensure_proxy(state: &AppState, alias: &str) -> Result<u16, String> {
    let socks_index = state.store.lock().unwrap().ensure(alias).socks_index;
    let port = SOCKS_BASE + socks_index as u16;

    // Read the live proxy's port without holding the lock across the await
    // (std MutexGuard is !Send).
    let existing = {
        let map = state.proxies.lock().unwrap();
        map.get(alias).filter(|e| !e.proc.exited()).map(|e| e.port)
    };
    if let Some(p) = existing {
        if is_port_open(p).await {
            return Ok(p);
        }
    }
    // Drop any stale entry before respawning.
    state.proxies.lock().unwrap().remove(alias);

    let cmd = ssh::socks_command(alias, port);
    let password = state.secrets.get(&pw_key(alias));
    let proc = BgSsh::spawn(cmd, password).map_err(|e| format!("spawn failed: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if proc.exited() {
            let msg = proc.recent();
            return Err(if msg.is_empty() {
                "ssh proxy exited. Try opening the Console first to authenticate.".into()
            } else {
                format!("ssh proxy exited. {msg}")
            });
        }
        if is_port_open(port).await {
            state.proxies.lock().unwrap().insert(alias.to_string(), ProxyEntry { port, proc });
            return Ok(port);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err("Timed out starting the SOCKS proxy.".into())
}

fn safe_profile(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' })
        .collect()
}

/// Launch the browser detached. Honors `ISM_BROWSER_CMD` (whole command) then
/// `ISM_BROWSER` (app name for `open -na`), defaulting to Google Chrome.
fn launch_browser(flags: &[String]) -> std::io::Result<()> {
    let mut cmd = if let Ok(c) = std::env::var("ISM_BROWSER_CMD") {
        let mut cmd = tokio::process::Command::new(c);
        cmd.args(flags);
        cmd
    } else {
        #[cfg(unix)]
        {
            let browser = std::env::var("ISM_BROWSER").unwrap_or_else(|_| "Google Chrome".into());
            let mut cmd = tokio::process::Command::new("open");
            cmd.arg("-na").arg(browser).arg("--args").args(flags);
            cmd
        }
        #[cfg(not(unix))]
        {
            // No `open` equivalent that also forwards flags: routing through
            // `cmd /C start` would let a `&` inside a URL flag terminate the
            // command. Resolve chrome.exe and spawn it directly instead — no
            // intermediate shell, so flags pass through verbatim.
            let mut cmd = tokio::process::Command::new(windows_chrome_path()?);
            cmd.args(flags);
            cmd
        }
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    // Reap the launcher (e.g. `open`) so it doesn't linger as a zombie.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// First Chrome install that actually exists, honoring `ISM_BROWSER` as an
/// explicit exe path. `CreateProcess` (unlike `ShellExecute`) does not consult
/// the App Paths registry, so a bare `chrome.exe` would not resolve — the full
/// path is required.
#[cfg(not(unix))]
fn windows_chrome_path() -> std::io::Result<std::path::PathBuf> {
    if let Ok(p) = std::env::var("ISM_BROWSER") {
        return Ok(std::path::PathBuf::from(p));
    }
    const SUFFIX: &str = r"Google\Chrome\Application\chrome.exe";
    let roots = ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"];
    for root in roots {
        if let Ok(dir) = std::env::var(root) {
            let p = std::path::Path::new(&dir).join(SUFFIX);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Chrome not found; set ISM_BROWSER to chrome.exe or ISM_BROWSER_CMD to a launcher",
    ))
}

fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}

// ---------------------------------------------------------------------------
// Embedded (in-tab) browser: a native child webview owned by the desktop shell,
// routed through the same per-host SOCKS proxy. macOS bypasses that proxy for
// loopback/private destinations, so this handles public + hostname browsing;
// the server's own 127.0.0.1 services are reached via `ssh -L` forwards + the
// tab navigating to `127.0.0.1:<local>` instead. Non-desktop builds register no
// controller, so `embed` reports 501 and the UI falls back to external Chrome.
// ---------------------------------------------------------------------------

/// A rectangle in the frontend's logical (CSS) pixels, relative to the window's
/// content area — where the desktop shell must place its child webview.
#[derive(Deserialize, Serialize, Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A command the embedded-browser controller carries out against its single live
/// child webview.
pub enum BrowserCommand {
    Open { url: String, socks_port: u16, rect: Rect },
    Bounds { rect: Rect },
    Navigate { url: String },
    Back,
    Forward,
    Reload,
    Show,
    Hide,
    Close,
}

type BrowserController = Box<dyn Fn(BrowserCommand) -> Result<(), String> + Send + Sync>;
static BROWSER_CONTROLLER: std::sync::OnceLock<BrowserController> = std::sync::OnceLock::new();

/// Register the host's embedded-browser controller. The Tauri shell installs one
/// backed by a native child webview. First call wins.
pub fn set_browser_controller<F>(f: F)
where
    F: Fn(BrowserCommand) -> Result<(), String> + Send + Sync + 'static,
{
    let _ = BROWSER_CONTROLLER.set(Box::new(f));
}

pub(crate) fn embedded_browser_available() -> bool {
    BROWSER_CONTROLLER.get().is_some()
}

fn dispatch(cmd: BrowserCommand) -> Result<(), String> {
    match BROWSER_CONTROLLER.get() {
        Some(f) => f(cmd),
        None => Err("embedded browser is only available in the desktop app".into()),
    }
}

#[derive(Deserialize)]
pub struct EmbedReq {
    #[serde(default)]
    url: Option<String>,
    rect: Rect,
}

/// `POST /api/servers/{id}/browser/embed` — ensure the host's SOCKS proxy, then
/// tell the desktop shell to show a child webview (routed through it) over `rect`.
/// 501 when no controller is registered so the UI can fall back to external Chrome.
pub async fn embed(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EmbedReq>,
) -> (StatusCode, Json<Value>) {
    if id == LOCAL_ID || !safe_name(&id) {
        return err(StatusCode::BAD_REQUEST, "this is the local machine — just open a browser normally");
    }
    if !embedded_browser_available() {
        return err(StatusCode::NOT_IMPLEMENTED, "embedded browser is only available in the desktop app");
    }
    let port = match ensure_proxy(&state, &id).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    let url = req
        .url
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "about:blank".into());
    match dispatch(BrowserCommand::Open { url, socks_port: port, rect: req.rect }) {
        Ok(_) => (StatusCode::OK, Json(json!({ "socksPort": port }))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum ControlReq {
    Bounds { rect: Rect },
    Navigate { url: String },
    Back,
    Forward,
    Reload,
    Show,
    Hide,
    Close,
}

/// `POST /api/browser/control` — drive the single live embedded webview.
pub async fn control(Json(req): Json<ControlReq>) -> (StatusCode, Json<Value>) {
    let cmd = match req {
        ControlReq::Bounds { rect } => BrowserCommand::Bounds { rect },
        ControlReq::Navigate { url } => BrowserCommand::Navigate { url },
        ControlReq::Back => BrowserCommand::Back,
        ControlReq::Forward => BrowserCommand::Forward,
        ControlReq::Reload => BrowserCommand::Reload,
        ControlReq::Show => BrowserCommand::Show,
        ControlReq::Hide => BrowserCommand::Hide,
        ControlReq::Close => BrowserCommand::Close,
    };
    match dispatch(cmd) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}
