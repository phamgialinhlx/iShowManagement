//! Embedded in-tab browser for the desktop shell.
//!
//! A single native child webview is added to the main window, routed through the
//! host's SOCKS proxy (`macos-proxy` feature), and positioned by the frontend
//! over the browser tab's content region. We register it as core's
//! `browser_controller` hook, so the axum `browser::embed`/`control` handlers —
//! which the same Svelte UI already talks to over HTTP — drive it.
//!
//! Every webview method here goes through the runtime's message-passing
//! dispatcher (and `add_child` hops to the main thread itself), so the hook can
//! run straight from the axum worker thread with no manual main-thread marshalling.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ismcore::BrowserCommand;
use tauri::webview::WebviewBuilder;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewUrl, Wry};

const MAIN_LABEL: &str = "main";
const BROWSER_LABEL: &str = "embedded-browser";

type Shared = Arc<Mutex<Option<Webview<Wry>>>>;

/// Cache for the main webview's top safe-area inset (f64 bits; NaN = unread).
type Inset = Arc<AtomicU64>;

/// Install the core embedded-browser controller, backed by one child webview on
/// the main window.
pub fn register(app: &AppHandle) {
    let app = app.clone();
    let webview: Shared = Arc::new(Mutex::new(None));
    let inset: Inset = Arc::new(AtomicU64::new(f64::NAN.to_bits()));
    ismcore::set_browser_controller(move |cmd| apply(&app, &webview, &inset, cmd));
}

fn apply(app: &AppHandle, shared: &Shared, inset: &Inset, cmd: BrowserCommand) -> Result<(), String> {
    match cmd {
        BrowserCommand::Open { url, socks_port, rect } => {
            close_existing(shared);
            let window = app.get_window(MAIN_LABEL).ok_or("main window not found")?;
            let url = Url::parse(&url).map_err(|e| e.to_string())?;
            let proxy = Url::parse(&format!("socks5://127.0.0.1:{socks_port}")).unwrap();
            let builder =
                WebviewBuilder::new(BROWSER_LABEL, WebviewUrl::External(url)).proxy_url(proxy);
            // The main webview sits under the title bar (fullSizeContentView), so its
            // DOM origin is inset; the child's is the frame top. Re-read the inset on
            // open (it can change with fullscreen) and shift the child down to match.
            inset.store(f64::NAN.to_bits(), Ordering::Relaxed);
            let dy = top_inset(app, inset);
            let webview = window
                .add_child(
                    builder,
                    LogicalPosition::new(rect.x, rect.y + dy),
                    LogicalSize::new(rect.w, rect.h),
                )
                .map_err(|e| e.to_string())?;
            *shared.lock().unwrap() = Some(webview);
            Ok(())
        }
        BrowserCommand::Bounds { rect } => {
            let dy = top_inset(app, inset);
            with(shared, |w| {
                w.set_position(LogicalPosition::new(rect.x, rect.y + dy)).map_err(|e| e.to_string())?;
                w.set_size(LogicalSize::new(rect.w, rect.h)).map_err(|e| e.to_string())
            })
        }
        BrowserCommand::Navigate { url } => {
            let url = Url::parse(&url).map_err(|e| e.to_string())?;
            with(shared, |w| w.navigate(url).map_err(|e| e.to_string()))
        }
        BrowserCommand::Back => with(shared, |w| w.eval("history.back()").map_err(|e| e.to_string())),
        BrowserCommand::Forward => {
            with(shared, |w| w.eval("history.forward()").map_err(|e| e.to_string()))
        }
        BrowserCommand::Reload => with(shared, |w| w.reload().map_err(|e| e.to_string())),
        BrowserCommand::Show => with(shared, |w| w.show().map_err(|e| e.to_string())),
        BrowserCommand::Hide => with(shared, |w| w.hide().map_err(|e| e.to_string())),
        BrowserCommand::Close => {
            close_existing(shared);
            Ok(())
        }
    }
}

/// Run `f` against the live webview, or no-op if none exists (Show/Hide/Bounds
/// can race a Close during tab switches — a missing webview isn't an error).
fn with<F: FnOnce(&Webview<Wry>) -> Result<(), String>>(shared: &Shared, f: F) -> Result<(), String> {
    match shared.lock().unwrap().as_ref() {
        Some(w) => f(w),
        None => Ok(()),
    }
}

fn close_existing(shared: &Shared) {
    if let Some(w) = shared.lock().unwrap().take() {
        let _ = w.close();
    }
}

/// Cached top safe-area inset of the main webview, reading it once on demand.
fn top_inset(app: &AppHandle, cache: &Inset) -> f64 {
    let c = f64::from_bits(cache.load(Ordering::Relaxed));
    if !c.is_nan() {
        return c;
    }
    let v = read_top_inset(app);
    cache.store(v.to_bits(), Ordering::Relaxed);
    v
}

#[cfg(target_os = "macos")]
fn read_top_inset(app: &AppHandle) -> f64 {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    #[repr(C)]
    struct NSEdgeInsets { top: f64, left: f64, bottom: f64, right: f64 }
    unsafe impl objc2::Encode for NSEdgeInsets {
        const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
            "NSEdgeInsets",
            &[f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING],
        );
    }
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mww) = app.get_webview_window(MAIN_LABEL) {
        let _ = mww.with_webview(move |pw| unsafe {
            let v = pw.inner() as *mut AnyObject;
            let sa: NSEdgeInsets = msg_send![&*v, safeAreaInsets];
            let _ = tx.send(sa.top);
        });
    }
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or(0.0)
}

#[cfg(not(target_os = "macos"))]
fn read_top_inset(_app: &AppHandle) -> f64 {
    0.0
}
