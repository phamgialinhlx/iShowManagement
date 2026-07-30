//! iShowManagement desktop shell (Phase 7).
//!
//! Runs the embedded `ismcore` axum server on a background thread (loopback,
//! random-free default port) and opens a native Tauri window pointed at it once
//! it's accepting connections. The window loads the same SPA the browser build
//! serves, so the entire feature set works unchanged.
//!
//! macOS uses the system WebKit — no extra runtime deps. Package with
//! `cargo tauri build` (`.app`/`.dmg`).

use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use tauri::{Url, WebviewUrl, WebviewWindowBuilder};

// Native `UNUserNotificationCenter` bindings. macOS-only; elsewhere the server's
// own notification path applies and this module isn't compiled.
#[cfg(target_os = "macos")]
mod notify;

const PORT: u16 = ismcore::DEFAULT_PORT;

fn main() {
    // Finder-launched apps get launchd's minimal PATH, which breaks
    // ~/.ssh/config ProxyCommands that live in Homebrew (cloudflared →
    // "Connection closed by UNKNOWN port 65535"). Adopt the login shell's
    // PATH before anything spawns so ssh/pty children see terminal-equivalent
    // tools. Must run before the server thread starts. Unix-only: Windows has
    // no login shell, and its PATH is already inherited from the user profile.
    #[cfg(unix)]
    adopt_login_shell_path();

    // Route the server's banner requests through UNUserNotificationCenter (our
    // bundle → correct icon, click focuses the app). Falls back to osascript in
    // core when this reports failure (e.g. running unbundled in dev).
    #[cfg(target_os = "macos")]
    ismcore::set_notifier(|n| notify::post(&n.title, &n.body, n.subtitle.as_deref()));

    // Bind our own loopback socket BEFORE loading the window, so the app always
    // serves — and loads — its own embedded server. If something else holds the
    // default port (e.g. a stray `cargo run -p core`), we fall back to a free
    // port rather than silently loading that other process's UI.
    let listener = bind_loopback(PORT);
    let port = listener.local_addr().expect("bound loopback addr").port();

    // Embedded server on its own multi-thread runtime, serving our bound socket.
    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            if let Err(e) = ismcore::serve_on(listener).await {
                eprintln!("embedded server error: {e}");
            }
        });
    });

    tauri::Builder::default()
        .setup(move |app| {
            // Only open the window once the server is accepting connections,
            // otherwise the webview would load a blank/error page.
            wait_for_server(port);
            let url = Url::parse(&format!("http://127.0.0.1:{port}")).expect("valid url");
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
                #[cfg(target_os = "macos")]
                notify::request_authorization();
            }
        });
}

/// Replace this process's PATH with the user's login-shell PATH. The marker
/// isolates the value from anything shell startup files print. Falls back to
/// appending the standard Homebrew dirs if the shell can't be queried.
#[cfg(unix)]
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

#[cfg(unix)]
fn extract_path(stdout: &str, marker: &str) -> Option<String> {
    let p = stdout.split(marker).nth(1)?.trim();
    (!p.is_empty()).then(|| p.to_string())
}

/// Bind a loopback listener on `preferred`, or — if it's taken — on an
/// OS-assigned free port. Owning the socket here (rather than letting the server
/// bind by number) guarantees the window loads *our* server, never a squatter's.
fn bind_loopback(preferred: u16) -> TcpListener {
    TcpListener::bind((Ipv4Addr::LOCALHOST, preferred)).unwrap_or_else(|_| {
        eprintln!("port {preferred} is in use — serving on an ephemeral port instead");
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback server port")
    })
}

fn wait_for_server(port: u16) {
    for _ in 0..100 {
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::bind_loopback;
    #[cfg(unix)]
    use super::extract_path;

    #[test]
    fn bind_loopback_falls_back_when_preferred_port_is_taken() {
        // Hold a real free port, then ask for the same one — it must fall back to
        // a different (ephemeral) port instead of panicking or returning the same.
        let held = bind_loopback(0);
        let taken = held.local_addr().unwrap().port();
        let fallback = bind_loopback(taken);
        assert_ne!(fallback.local_addr().unwrap().port(), taken);
    }

    #[cfg(unix)]
    #[test]
    fn extracts_path_after_marker_despite_rc_noise() {
        let out = "welcome banner from .zprofile\n__M__/opt/homebrew/bin:/usr/bin:/bin";
        assert_eq!(
            extract_path(out, "__M__").as_deref(),
            Some("/opt/homebrew/bin:/usr/bin:/bin")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_missing_or_empty_path() {
        assert_eq!(extract_path("no marker here", "__M__"), None);
        assert_eq!(extract_path("__M__  \n", "__M__"), None);
    }
}
