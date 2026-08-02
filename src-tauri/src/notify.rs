//! Telling the operator when a session wants them.
//!
//! The whole premise of rmux is running several Claudes at once, which only
//! pays off if you can stop watching them. Without a notification the operator
//! has to poll the rail with their eyes — and the moment they stop doing that,
//! a session that finished in ninety seconds sits idle for twenty minutes.
//!
//! ## An app command, driving the plugin from Rust
//!
//! This is a `#[tauri::command]` of our own that calls the notification
//! plugin's **Rust** API, and that combination is deliberate:
//!
//!  - The plugin is what gives a notification **rmux's own identity** — its
//!    icon and its name. The first version shelled out to `osascript`, which
//!    posts under Script Editor's identity: an unrelated icon, and a name that
//!    tells the operator nothing about which app wants them.
//!  - Calling it from Rust means no ACL entry. The ACL governs the *webview's*
//!    access to `plugin:*` commands, and a missing grant fails **silently** —
//!    the promise rejects with nothing surfaced, which for a feature whose whole
//!    job is to be noticed is the worst failure available.
//!
//! ## What actually happens, and what the icon will be
//!
//! rmux command → the plugin → `notify_rust` → `mac-notification-sys` → macOS
//! Notification Center. The plugin picks the posting identity itself, and the
//! choice is worth knowing because it is not ours to make:
//!
//! ```ignore
//! let _ = notify_rust::set_application(if tauri::is_dev() {
//!     "com.apple.Terminal"          // ← a dev build posts as Terminal
//! } else {
//!     &self.identifier              // ← a bundled build posts as rmux
//! });
//! ```
//!
//! So **`pnpm tauri dev` shows Terminal's icon and name**, by the plugin's own
//! design: a dev binary has no registered bundle identity to post under. Only
//! the built `.app` carries rmux's icon. That is not a bug to chase in dev.
//!
//! ## Failures are invisible, and that is the plugin's doing
//!
//! `show()` ends in `tauri::async_runtime::spawn(async move { let _ =
//! notification.show(); })` — spawned, with the result discarded. Nothing this
//! command returns can tell you whether anything appeared. An earlier version of
//! this file claimed the opposite; it was wrong, and a comment asserting a
//! guarantee that does not exist is worse than no comment. If notifications go
//! quiet, the place to look is System Settings › Notifications for whichever
//! identity is posting — Terminal in dev, rmux in a bundle.

use tauri::State;
use tokio::sync::Mutex;

/// Suppresses repeats of the same message.
///
/// Status is polled several times a second, and a session sitting in `waiting`
/// would otherwise notify on every tick. Keyed by session so two sessions
/// finishing together both get through.
#[derive(Default)]
pub struct NotifyStore {
    last: Mutex<std::collections::HashMap<String, String>>,
}

/// Show a notification, unless it is the same one this session just showed.
#[tauri::command]
pub async fn notify(
    app: tauri::AppHandle,
    store: State<'_, NotifyStore>,
    session: String,
    title: String,
    body: String,
) -> Result<bool, String> {
    {
        let mut last = store.last.lock().await;
        let key = format!("{title}\u{0}{body}");
        if last.get(&session) == Some(&key) {
            return Ok(false);
        }
        last.insert(session, key);
    }

    // `Ok` here means "handed to the plugin", not "the operator saw it" — see
    // the note above about the discarded result. Nothing downstream can promise
    // more than that, so nothing here pretends to.
    show(&app, &title, &body).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Post it, under rmux's own identity.
fn show(app: &tauri::AppHandle, title: &str, body: &str) -> anyhow::Result<()> {
    use tauri_plugin_notification::NotificationExt as _;

    // The session name is the title: it is what says *which* of several running
    // Claudes this is. The app's own name is already on the notification,
    // supplied by the bundle.
    app.notification().builder().title(title).body(body).show()?;
    Ok(())
}

/// Forget a session's last notification, so its next one always shows.
#[tauri::command]
pub async fn notify_reset(store: State<'_, NotifyStore>, session: String) -> Result<(), String> {
    store.last.lock().await.remove(&session);
    Ok(())
}
