//! Writing to the OS clipboard from the native side.
//!
//! `navigator.clipboard.writeText` in WKWebView refuses with `NotAllowedError`
//! ("not allowed by the user agent or the platform in the current context")
//! unless rmux's own document holds focus *and* the call still counts as a user
//! gesture. Neither is reliable here: a dsh pane is a separate native webview
//! that keeps focus after a click beside it, and an `await` between the click
//! and the write spends the gesture. So "Copy full path" worked on some presses
//! and not others — reported as "has to press many times". The pasteboard
//! has no such rule, so copying goes through it.
//!
//! Desktop only: in rmux-web this would fill the *server's* clipboard, so the
//! web build keeps the browser API (where focus is not split between views).

use std::sync::Mutex;

/// Kept alive for the run. On X11 the process that set the clipboard is the one
/// that serves every later paste — dropping it empties the clipboard unless a
/// clipboard manager happens to be running.
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

fn write(text: String) -> Result<(), String> {
    let mut slot = CLIPBOARD.lock().map_err(|_| "clipboard lock poisoned".to_string())?;
    if slot.is_none() {
        *slot = Some(arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?);
    }
    slot.as_mut()
        .expect("set above")
        .set_text(text)
        .map_err(|e| format!("could not write to the clipboard: {e}"))
}

#[tauri::command]
pub async fn clipboard_write(text: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || write(text))
        .await
        .map_err(|e| e.to_string())?
}
