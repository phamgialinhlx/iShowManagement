//! iShowManagement desktop shell (Phase 7).
//!
//! Runs the embedded `ismcore` axum server on a background thread (loopback,
//! random-free default port) and opens a native Tauri window pointed at it once
//! it's accepting connections. The window loads the same SPA the browser build
//! serves, so the entire feature set works unchanged.
//!
//! macOS uses the system WebKit — no extra runtime deps. Package with
//! `cargo tauri build` (`.app`/`.dmg`).

use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use tauri::{Url, WebviewUrl, WebviewWindowBuilder};

mod notify;

const PORT: u16 = ismcore::DEFAULT_PORT;

fn main() {
    // Finder-launched apps get launchd's minimal PATH, which breaks
    // ~/.ssh/config ProxyCommands that live in Homebrew (cloudflared →
    // "Connection closed by UNKNOWN port 65535"). Adopt the login shell's
    // PATH before anything spawns so ssh/pty children see terminal-equivalent
    // tools. Must run before the server thread starts.
    adopt_login_shell_path();

    // Route the server's banner requests through UNUserNotificationCenter (our
    // bundle → correct icon, click focuses the app). Falls back to osascript in
    // core when this reports failure (e.g. running unbundled in dev).
    ismcore::set_notifier(|n| notify::post(&n.title, &n.body, n.subtitle.as_deref()));

    // Embedded server on its own multi-thread runtime.
    thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            if let Err(e) = ismcore::serve(PORT).await {
                eprintln!("embedded server error: {e}");
            }
        });
    });

    tauri::Builder::default()
        .setup(|app| {
            // Only open the window once the server is accepting connections,
            // otherwise the webview would load a blank/error page.
            wait_for_server();
            let url = Url::parse(&format!("http://127.0.0.1:{PORT}")).expect("valid url");
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("iShowManagement")
                .inner_size(1200.0, 800.0)
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app, event| {
            // Request notification permission once the app has finished
            // launching — the notification machinery isn't ready during
            // `setup()`, so a request there is silently dropped.
            if let tauri::RunEvent::Ready = event {
                notify::request_authorization();
            }
        });
}

/// Replace this process's PATH with the user's login-shell PATH. The marker
/// isolates the value from anything shell startup files print. Falls back to
/// appending the standard Homebrew dirs if the shell can't be queried.
fn adopt_login_shell_path() {
    const MARKER: &str = "__ISM_PATH__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let path = std::process::Command::new(&shell)
        .args(["-l", "-c", &format!("printf '%s' \"{MARKER}$PATH\"")])
        .output()
        .ok()
        .and_then(|o| extract_path(&String::from_utf8_lossy(&o.stdout), MARKER));
    match path {
        Some(p) => std::env::set_var("PATH", p),
        None => {
            let current = std::env::var("PATH").unwrap_or_default();
            let mut p = current.clone();
            for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
                if !current.split(':').any(|d| d == dir) {
                    p.push(':');
                    p.push_str(dir);
                }
            }
            std::env::set_var("PATH", p);
        }
    }
}

fn extract_path(stdout: &str, marker: &str) -> Option<String> {
    let p = stdout.split(marker).nth(1)?.trim();
    (!p.is_empty()).then(|| p.to_string())
}

fn wait_for_server() {
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", PORT)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::extract_path;

    #[test]
    fn extracts_path_after_marker_despite_rc_noise() {
        let out = "welcome banner from .zprofile\n__M__/opt/homebrew/bin:/usr/bin:/bin";
        assert_eq!(
            extract_path(out, "__M__").as_deref(),
            Some("/opt/homebrew/bin:/usr/bin:/bin")
        );
    }

    #[test]
    fn rejects_missing_or_empty_path() {
        assert_eq!(extract_path("no marker here", "__M__"), None);
        assert_eq!(extract_path("__M__  \n", "__M__"), None);
    }
}
