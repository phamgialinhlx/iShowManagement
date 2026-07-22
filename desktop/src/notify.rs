//! Native macOS notifications via `UNUserNotificationCenter` — the modern API
//! macOS actually delivers. The legacy `NSUserNotification` path (used by
//! `mac-notification-sys`/`notify-rust`/`tauri-plugin-notification`) is silently
//! dropped on recent macOS for apps that never registered in Notification
//! Center, and there's no way to register through it.
//!
//! `UNUserNotificationCenter` requires a real code signature: an ad-hoc-signed
//! build is denied authorization (`granted=false`). So we request authorization
//! at launch and remember the result — when granted (a properly Developer-ID
//! signed build) we post here (correct app icon, click focuses the app); when
//! denied we report failure and the server falls back to `osascript`. This makes
//! notifications work today and automatically upgrade once the app is signed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNNotificationSound, UNUserNotificationCenter,
};

/// Set from the authorization callback: true only when macOS grants us
/// permission (requires proper signing). Until then, `post` defers to osascript.
static AUTHORIZED: AtomicBool = AtomicBool::new(false);

/// `UNUserNotificationCenter` throws if we're not a bundled app. We only ever
/// touch it when launched from the `.app` (which has a bundle identifier).
fn bundled() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

fn center() -> Retained<UNUserNotificationCenter> {
    UNUserNotificationCenter::currentNotificationCenter()
}

/// Ask for notification permission once at launch. Call after the app has
/// finished launching (the notification machinery isn't ready during Tauri
/// `setup()`). The result gates [`post`].
pub fn request_authorization() {
    if !bundled() {
        return;
    }
    let opts = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
    let handler = RcBlock::new(|granted: Bool, err: *mut NSError| {
        AUTHORIZED.store(granted.as_bool(), Ordering::Relaxed);
        let detail = unsafe { err.as_ref() }
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| "no error".into());
        log_line(&format!("authorization granted={} ({detail})", granted.as_bool()));
    });
    center().requestAuthorizationWithOptions_completionHandler(opts, &handler);
}

/// Finder-launched apps have no visible stderr, so authorization outcomes go
/// to `~/.ism/desktop.log` for diagnosis.
fn log_line(msg: &str) {
    use std::io::Write;
    let Ok(home) = std::env::var("HOME") else { return };
    let dir = std::path::PathBuf::from(home).join(".ism");
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("desktop.log"))
    else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(f, "{ts} {msg}");
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Post a banner via `UNUserNotificationCenter`. Returns false (→ osascript
/// fallback) when unbundled or macOS denied authorization (ad-hoc build).
pub fn post(title: &str, body: &str, subtitle: Option<&str>) -> bool {
    if !bundled() || !AUTHORIZED.load(Ordering::Relaxed) {
        return false;
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    if let Some(sub) = subtitle {
        content.setSubtitle(&NSString::from_str(sub));
    }
    content.setSound(Some(&UNNotificationSound::defaultSound()));

    let id = SEQ.fetch_add(1, Ordering::Relaxed);
    let ident = NSString::from_str(&format!("ism-{id}"));
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&ident, &content, None);
    center().addNotificationRequest_withCompletionHandler(&request, None);
    true
}
