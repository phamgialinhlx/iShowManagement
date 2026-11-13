//! The `rmux` command line, from the desktop's side: putting it on `PATH`, and
//! telling it which servers exist.
//!
//! **The command is the agent binary this app already ships**
//! (`Contents/Resources/agents/rmux-agent` on macOS), so installing is a
//! symlink: nothing is copied, and the next app update moves the command with
//! it. The one exception is an AppImage, whose resources live in a mount that
//! disappears when the app exits — a link into it would dangle, so the static
//! musl agent is copied instead.
//!
//! The server list lives only in the webview's storage, so the UI hands it here
//! on every change and it is written to `~/.rmux/servers.json` (`0600`,
//! atomic) for `rmux ls` to fan out over. No secrets are in it.

use std::path::{Path, PathBuf};

use rmux_agent::servers::{self, ServerEntry};
use serde::Serialize;
use tauri::Manager;

/// Mirror the server list to `~/.rmux/servers.json` — and make it the authority
/// on which key each server was told to offer.
///
/// The chosen-identity table (`rmux_ssh::keys`) is **replaced** here, before the
/// file write, so a server switched back to DEFAULT stops offering its key on
/// the next connection even if writing the CLI's copy fails. A `TargetRef`
/// carrying an identity only ever *adds* one; this is the only place one is
/// taken away, and it is the list the operator is actually looking at.
#[tauri::command]
pub fn servers_sync(servers: Vec<ServerEntry>) -> Result<(), String> {
    rmux_ssh::keys::replace_chosen_identities(servers.iter().filter_map(|s| {
        let path = PathBuf::from(s.identity.as_deref()?.trim());
        path.is_absolute().then(|| (s.key_label(), path))
    }));
    let home = dirs::home_dir().ok_or("no home directory")?;
    servers::write(&home, &servers).map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    link: Option<String>,
    target: Option<String>,
    current: bool,
    bundled: Option<String>,
    on_path: bool,
    note: Option<String>,
}

/// The binary an install points at, for this machine.
///
/// macOS: the native host build. Linux: the static musl agent for this
/// architecture — the bundle's bare `agents/rmux-agent` is the *build
/// machine's* host binary (the agents are compiled once, on a Mac, for every
/// platform), so it is the wrong file to put on a Linux `PATH`.
fn bundled_agent<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    let resource = app.path().resource_dir().ok();
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let source = rmux_agent::provision::default_source(resource.as_deref(), exe_dir.as_deref());
    if cfg!(target_os = "linux") {
        let triple = if cfg!(target_arch = "aarch64") {
            "aarch64-unknown-linux-musl"
        } else {
            "x86_64-unknown-linux-musl"
        };
        let musl = source.dir.join(format!("rmux-agent-{triple}"));
        return musl.exists().then_some(musl);
    }
    source.local.filter(|p| p.exists())
}

/// Where a link may go, in order of preference: the system-wide folder every
/// shell already has on `PATH`, then the per-user one.
fn destinations() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/usr/local/bin/rmux")];
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".local/bin/rmux"));
    }
    out
}

fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d == dir))
}

/// Is `path` an `rmux` we installed (a link to an agent, or a copied agent)?
/// Anything else at that name belongs to someone else and is never touched.
fn is_ours(path: &Path) -> bool {
    match std::fs::read_link(path) {
        Ok(target) => target
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rmux-agent")),
        Err(_) => std::fs::read(path)
            .ok()
            .is_some_and(|bytes| bytes.windows(b"RMUX_DETACH_KEY".len()).any(|w| w == b"RMUX_DETACH_KEY")),
    }
}

fn status_with(bundled: Option<&Path>, note: Option<String>) -> CliStatus {
    let found = destinations().into_iter().find(|p| p.symlink_metadata().is_ok() && is_ours(p));
    let target = found.as_ref().and_then(|p| std::fs::read_link(p).ok());
    let current = match (&target, bundled) {
        (Some(t), Some(b)) => std::fs::canonicalize(t).ok() == std::fs::canonicalize(b).ok(),
        // A copied binary (AppImage) is current by construction until the next update.
        (None, Some(_)) => found.is_some(),
        _ => false,
    };
    CliStatus {
        on_path: found.as_ref().and_then(|p| p.parent()).is_some_and(on_path),
        link: found.as_ref().map(|p| p.display().to_string()),
        target: target.map(|t| t.display().to_string()),
        current,
        bundled: bundled.map(|b| b.display().to_string()),
        note,
    }
}

#[tauri::command]
pub fn cli_status<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> CliStatus {
    status_with(bundled_agent(&app).as_deref(), None)
}

/// Put `rmux` at `dest`, replacing only a previous install of ours.
///
/// The new link is made beside the destination and renamed over it, so there
/// is never a moment with no `rmux` at all.
fn place(source: &Path, dest: &Path, copy: bool) -> std::io::Result<()> {
    if dest.symlink_metadata().is_ok() && !is_ours(dest) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists and is not rmux's; it was left alone", dest.display()),
        ));
    }
    let dir = dest.parent().ok_or_else(|| std::io::Error::other("no parent folder"))?;
    let temp = dir.join(format!(".rmux.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    if copy {
        std::fs::copy(source, &temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
        }
    } else {
        #[cfg(unix)]
        std::os::unix::fs::symlink(source, &temp)?;
        #[cfg(not(unix))]
        return Err(std::io::Error::other("installing the command is supported on macOS and Linux"));
    }
    if let Err(e) = std::fs::rename(&temp, dest) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

#[tauri::command]
pub fn cli_install<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<CliStatus, String> {
    let bundled = bundled_agent(&app).ok_or("this build of rmux has no agent binary to install")?;
    let copy = std::env::var_os("APPIMAGE").is_some();

    let mut reasons = Vec::new();
    for dest in destinations() {
        let Some(dir) = dest.parent() else { continue };
        // Only the per-user folder is ours to create.
        if !dir.exists() && dir.starts_with("/usr") {
            reasons.push(format!("{} does not exist", dir.display()));
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(dir) {
            reasons.push(format!("{}: {e}", dir.display()));
            continue;
        }
        match place(&bundled, &dest, copy) {
            Ok(()) => {
                // Verified, not assumed: the command must resolve to a file that exists.
                if std::fs::metadata(&dest).is_err() {
                    return Err(format!("{} was created but does not lead to the agent", dest.display()));
                }
                let note = if on_path(dir) {
                    format!("Installed at {}. Open a new terminal and run `rmux ls`.", dest.display())
                } else {
                    format!(
                        "Installed at {}, which is not on your PATH yet — add it to your shell profile.",
                        dest.display()
                    )
                };
                return Ok(status_with(Some(&bundled), Some(note)));
            }
            // A foreign `rmux` is a reason to stop, not to try the next folder:
            // installing a second one further down PATH would be shadowed by it.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e.to_string()),
            Err(e) => reasons.push(format!("{}: {e}", dir.display())),
        }
    }
    Err(format!("could not install the command ({})", reasons.join("; ")))
}

#[tauri::command]
pub fn cli_uninstall<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<CliStatus, String> {
    let mut removed = Vec::new();
    for dest in destinations() {
        if dest.symlink_metadata().is_ok() && is_ours(&dest) {
            std::fs::remove_file(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            removed.push(dest.display().to_string());
        }
    }
    let note = if removed.is_empty() {
        "There was no rmux command to remove.".to_owned()
    } else {
        format!("Removed {}.", removed.join(" and "))
    };
    Ok(status_with(bundled_agent(&app).as_deref(), Some(note)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn install_replaces_its_own_link_and_never_a_stranger() {
        let dir = std::env::temp_dir().join(format!("rmux-cli-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let agent = dir.join("rmux-agent");
        std::fs::write(&agent, b"#!/bin/sh\n").unwrap();
        let dest = dir.join("rmux");

        place(&agent, &dest, false).unwrap();
        assert_eq!(std::fs::read_link(&dest).unwrap(), agent);
        // Re-installing over our own link is fine.
        place(&agent, &dest, false).unwrap();
        assert!(is_ours(&dest));

        // Someone else's `rmux` is left alone.
        std::fs::remove_file(&dest).unwrap();
        std::fs::write(&dest, b"another tool").unwrap();
        let err = place(&agent, &dest, false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&dest).unwrap(), b"another tool");
        std::fs::remove_dir_all(&dir).ok();
    }
}
