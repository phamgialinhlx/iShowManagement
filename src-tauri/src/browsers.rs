//! Launching the operator's own browser through a SOCKS proxy onto a target.
//!
//! This is the everyday half of the "no in-app browser" design (see
//! `tunnels.rs`): rbrowse is the full per-session arrangement, but most of the
//! time the operator just wants *a* browser that sees the server's network.
//! `browser_open` reuses `Forwards::socks` (`ssh -D`, idempotent per host) and
//! spawns a detached Chromium pointed at it — so an internal hostname resolves
//! over there, and thanks to `--proxy-bypass-list=<-loopback>` even
//! `http://127.0.0.1:<port>` is the *server's* loopback, which is what makes a
//! per-port "open" button possible with no forwarding step.
//!
//! Only Chromium-family browsers are offered: they accept `--proxy-server` and
//! `--user-data-dir` on the command line. Firefox needs a prefs-configured
//! profile, so it is deliberately excluded rather than half-supported.
//!
//! Two inputs cross the IPC bridge and both are validated here, not trusted:
//! - **`bin` must be re-detected on this side.** The webview naming an
//!   arbitrary path to execute is a privilege escalation, same class of rule as
//!   "a pid crosses the bridge as a u32".
//! - **`url` must be http(s).** Anything else — a leading `-`, a `file:` URL —
//!   is a Chromium flag or a local read handed to a proxied profile.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use tauri::Manager as _;

/// A browser the operator can point at a proxy, as the UI sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// Absolute path to the executable — also the token `browser_open` expects
    /// back, and re-validates against a fresh detection.
    pub bin: String,
}

struct Candidate {
    id: &'static str,
    name: &'static str,
    /// Names looked up on PATH (Linux and anything unix-ish).
    bins: &'static [&'static str],
}

const CANDIDATES: &[Candidate] = &[
    Candidate { id: "chrome", name: "Google Chrome", bins: &["google-chrome-stable", "google-chrome"] },
    Candidate { id: "chromium", name: "Chromium", bins: &["chromium", "chromium-browser"] },
    Candidate { id: "brave", name: "Brave", bins: &["brave-browser", "brave"] },
    Candidate { id: "edge", name: "Microsoft Edge", bins: &["microsoft-edge-stable", "microsoft-edge"] },
    Candidate { id: "vivaldi", name: "Vivaldi", bins: &["vivaldi-stable", "vivaldi"] },
    Candidate { id: "opera", name: "Opera", bins: &["opera"] },
];

/// Fixed install locations checked after PATH — how browsers actually arrive
/// on macOS and Windows, where nothing puts them on PATH.
fn absolute_candidates(id: &str) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let app = match id {
            "chrome" => "Google Chrome.app/Contents/MacOS/Google Chrome",
            "chromium" => "Chromium.app/Contents/MacOS/Chromium",
            "brave" => "Brave Browser.app/Contents/MacOS/Brave Browser",
            "edge" => "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "vivaldi" => "Vivaldi.app/Contents/MacOS/Vivaldi",
            "opera" => "Opera.app/Contents/MacOS/Opera",
            _ => return Vec::new(),
        };
        return vec![PathBuf::from("/Applications").join(app)];
    }
    #[cfg(windows)]
    {
        let rel = match id {
            "chrome" => r"Google\Chrome\Application\chrome.exe",
            "edge" => r"Microsoft\Edge\Application\msedge.exe",
            "brave" => r"BraveSoftware\Brave-Browser\Application\brave.exe",
            "vivaldi" => r"Vivaldi\Application\vivaldi.exe",
            "opera" => r"Opera\opera.exe",
            _ => return Vec::new(),
        };
        return ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(|var| std::env::var_os(var))
            .map(|base| PathBuf::from(base).join(rel))
            .collect();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = id;
        Vec::new()
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Chromium-family browsers present on this machine, one entry per browser.
///
/// No shell-out: PATH is walked directly, so there is no `sh -lc` whose aliases
/// or functions could answer instead of a real executable.
fn detect_with(path_var: Option<&OsString>) -> Vec<BrowserInfo> {
    let dirs: Vec<PathBuf> =
        path_var.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();

    CANDIDATES
        .iter()
        .filter_map(|c| {
            let on_path = c.bins.iter().find_map(|bin| {
                #[cfg(windows)]
                let bin: &str = &format!("{bin}.exe");
                #[cfg(not(windows))]
                let bin: &str = bin;
                dirs.iter().map(|d| d.join(bin)).find(|p| is_executable(p))
            });
            let found = on_path.or_else(|| absolute_candidates(c.id).into_iter().find(|p| is_executable(p)));
            found.map(|p| BrowserInfo { id: c.id, name: c.name, bin: p.to_string_lossy().into_owned() })
        })
        .collect()
}

fn detect() -> Vec<BrowserInfo> {
    detect_with(std::env::var_os("PATH").as_ref())
}

/// The `bin` the webview handed back, only if detection still agrees it is a
/// browser. Anything else is refused by name — this is the whole reason
/// `browser_open` takes a token rather than a path to run.
fn ensure_known(bin: &str, detected: &[BrowserInfo]) -> Result<(), String> {
    if detected.iter().any(|b| b.bin == bin) {
        Ok(())
    } else {
        Err(format!("'{bin}' is not one of the detected browsers; refusing to launch it"))
    }
}

/// `None`, or an http(s) URL. A leading `-` would be a Chromium flag and
/// `file:`/`javascript:` are reads this proxied profile has no business doing —
/// requiring the scheme excludes all of them at once.
fn checked_url(url: Option<String>) -> Result<Option<String>, String> {
    let Some(url) = url else { return Ok(None) };
    let url = url.trim().to_owned();
    if url.is_empty() {
        return Ok(None);
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(Some(url))
    } else {
        Err("only http(s) URLs can be opened in the proxied browser".to_owned())
    }
}

/// Per-host profile under app-data: cookies and logins survive relaunches,
/// while staying fully separate from the operator's daily browser — and from
/// every other host's profile, which all sit behind different proxies.
fn profile_dir(app: &tauri::AppHandle, host: &str) -> Result<PathBuf, String> {
    let safe: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("browser-profiles")
        .join(safe);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn launch(bin: &str, socks_port: u16, profile: &Path, url: Option<&str>) -> Result<(), String> {
    let mut child = std::process::Command::new(bin)
        // Chromium sends hostnames to a socks5 proxy itself, so the server's
        // DNS answers — an internal name resolves over there, not here.
        .arg(format!("--proxy-server=socks5://127.0.0.1:{socks_port}"))
        // Route loopback THROUGH the proxy: 127.0.0.1 in this browser is the
        // server's, which is the whole point of the per-port open button.
        .arg("--proxy-bypass-list=<-loopback>")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg(url.unwrap_or("about:blank"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not launch {bin}: {e}"))?;

    // Reap without holding on: a dropped Child is never waited on, so the
    // browser would sit as a zombie from the moment it quit until rmux did.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[tauri::command]
pub fn browsers_detect() -> Vec<BrowserInfo> {
    detect()
}

/// Open a Chromium-family browser whose whole view of the network is the
/// target's, via the shared SOCKS proxy (started here if need be — asking
/// twice reuses the same `ssh -D`). Returns the proxy's port so the UI can
/// show it. The proxy dies with rmux, so a browser left open outlives its
/// route — pages just stop loading, nothing breaks.
#[tauri::command]
pub async fn browser_open(
    app: tauri::AppHandle,
    store: tauri::State<'_, crate::tunnels::TunnelStore>,
    target: crate::terminal::TargetRef,
    bin: String,
    url: Option<String>,
) -> Result<u16, String> {
    ensure_known(&bin, &detect())?;
    let url = checked_url(url)?;

    // `socks` refuses a host-less target with its own reason — a proxy onto
    // this machine would do nothing.
    let port = store.forwards().socks(target.host.as_deref()).await?;
    let profile = profile_dir(&app, target.host.as_deref().unwrap_or("local"))?;
    launch(&bin, port, &profile, url.as_deref())?;
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rmux-browsers-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn write_bin(dir: &Path, name: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn detects_an_executable_on_the_given_path() {
        let dir = scratch("detect");
        write_bin(&dir, "chromium", 0o755);

        let path_var = std::env::join_paths([&dir]).unwrap();
        let found = detect_with(Some(&path_var));
        let chromium = found.iter().find(|b| b.id == "chromium").expect("chromium should be found");
        assert_eq!(chromium.bin, dir.join("chromium").to_string_lossy());
    }

    #[cfg(unix)]
    #[test]
    fn a_plain_file_is_not_a_browser() {
        // A non-executable `chromium` (a stray download, a directory listing
        // artefact) must not be offered — spawning it fails confusingly later.
        let dir = scratch("noexec");
        write_bin(&dir, "chromium", 0o644);

        let path_var = std::env::join_paths([&dir]).unwrap();
        assert!(detect_with(Some(&path_var)).iter().all(|b| b.id != "chromium"));
    }

    #[test]
    fn an_undetected_bin_is_refused() {
        let detected = vec![BrowserInfo { id: "chromium", name: "Chromium", bin: "/usr/bin/chromium".into() }];
        assert!(ensure_known("/usr/bin/chromium", &detected).is_ok());
        // The attack this guards: the webview naming an arbitrary executable.
        assert!(ensure_known("/usr/bin/rm", &detected).is_err());
        assert!(ensure_known("", &detected).is_err());
    }

    #[test]
    fn only_http_urls_pass() {
        assert_eq!(checked_url(None).unwrap(), None);
        assert_eq!(checked_url(Some("  ".into())).unwrap(), None);
        assert_eq!(checked_url(Some("http://127.0.0.1:3000".into())).unwrap().as_deref(), Some("http://127.0.0.1:3000"));
        assert_eq!(checked_url(Some("https://internal.host".into())).unwrap().as_deref(), Some("https://internal.host"));
        // A leading dash is a Chromium flag, not a page.
        assert!(checked_url(Some("--disable-web-security".into())).is_err());
        assert!(checked_url(Some("file:///etc/passwd".into())).is_err());
        assert!(checked_url(Some("javascript:alert(1)".into())).is_err());
    }
}
