//! Filesystem IPC.
//!
//! Every command takes a target, so the UI uses one set of calls whether the file
//! is on this machine or on a host across the world.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_fs::{DirEntry, FileContent, FileSystem, LocalFs, PreviewContent, SearchHit, SearchQuery, TargetFs};
use rmux_ssh::SshTarget;
use rmux_transport::TargetId;
use tauri::State;

use crate::terminal::TargetRef;

/// Filesystems already built, keyed by target.
///
/// Cached so an SSH filesystem reuses the existing ControlMaster connection
/// rather than reconnecting for every listing.
#[derive(Default)]
pub struct FsStore {
    filesystems: Mutex<HashMap<TargetId, Arc<dyn FileSystem>>>,
}

impl FsStore {
    /// Forget this target's cached handle.
    ///
    /// Returns whether there was one. Dropping rmux's handle is only half of a
    /// disconnect — the transport is closed by the caller, because a cache that
    /// merely forgets leaves the connection up if any other clone survives.
    pub fn evict_target(&self, id: &TargetId) -> bool {
        self.filesystems.lock().remove(id).is_some()
    }

    /// Insert a resolved handle directly. Tests only: the real path needs a
    /// live host, which a unit test must not require.
    pub fn insert_for_test(&self, id: TargetId, value: Arc<dyn FileSystem>) {
        self.filesystems.lock().insert(id, value);
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn filesystem(
    store: &FsStore,
    target: &TargetRef,
) -> Result<Arc<dyn FileSystem>, String> {
    let id = target.id();

    if let Some(existing) = store.filesystems.lock().get(&id) {
        return Ok(Arc::clone(existing));
    }

    let fs: Arc<dyn FileSystem> = match &id {
        // Native calls locally: routing a local file browser through a shell
        // would spawn a process per directory, which is exactly the sluggishness
        // a local IDE must not have.
        TargetId::Local => Arc::new(LocalFs::new()),
        TargetId::Ssh(host) => {
            let ssh = SshTarget::new(host.clone());
            ssh.connect().await.map_err(err)?;
            Arc::new(TargetFs::new(ssh))
        }
    };

    store.filesystems.lock().insert(id, Arc::clone(&fs));
    Ok(fs)
}

#[tauri::command]
pub async fn fs_list(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
) -> Result<Vec<DirEntry>, String> {
    filesystem(store.inner(), &target).await?.list_dir(&path).await.map_err(err)
}

#[tauri::command]
pub async fn fs_read(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
) -> Result<FileContent, String> {
    filesystem(store.inner(), &target).await?.read_file(&path).await.map_err(err)
}

#[tauri::command]
pub async fn fs_write(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
    contents: String,
) -> Result<(), String> {
    filesystem(store.inner(), &target).await?.write_file(&path, &contents).await.map_err(err)
}

/// Read a file as base64, for formats the text editor cannot show.
#[tauri::command]
pub async fn fs_preview(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
) -> Result<PreviewContent, String> {
    filesystem(store.inner(), &target).await?.read_preview(&path).await.map_err(err)
}

#[tauri::command]
pub async fn fs_home(store: State<'_, FsStore>, target: TargetRef) -> Result<String, String> {
    filesystem(store.inner(), &target).await?.home_dir().await.map_err(err)
}

#[tauri::command]
pub async fn fs_create_file(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
) -> Result<(), String> {
    filesystem(store.inner(), &target).await?.create_file(&path).await.map_err(err)
}

#[tauri::command]
pub async fn fs_create_dir(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
) -> Result<(), String> {
    filesystem(store.inner(), &target).await?.create_dir(&path).await.map_err(err)
}

/// Write dropped or picked bytes into a new file on the target.
///
/// The payload arrives base64-encoded because the IPC bridge is JSON, and is
/// decoded here so the far side receives raw bytes — a `.png` routed through a
/// `String` would be corrupted before it ever reached the disk.
///
/// The size check happens on the *decoded* length. Checking the base64 instead
/// would reject files a third smaller than the stated limit, which is a cap that
/// lies about itself.
#[tauri::command]
pub async fn fs_upload(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
    base64: String,
) -> Result<(), String> {
    use base64::Engine as _;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64.as_bytes())
        .map_err(|e| format!("could not decode the upload: {e}"))?;

    if bytes.len() as u64 > rmux_fs::MAX_UPLOAD_BYTES {
        return Err(format!(
            "{} is too large to upload — the limit is {} MB",
            path,
            rmux_fs::MAX_UPLOAD_BYTES / (1024 * 1024)
        ));
    }

    filesystem(store.inner(), &target).await?.upload(&path, &bytes).await.map_err(err)
}

/// What a completed download tells the operator.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Downloaded {
    /// The full local path. "Check your downloads" is not an answer when the
    /// folder holds two hundred things — the same reasoning as the log export.
    pub path: String,
    pub bytes: u64,
}

/// Copy a file off the target onto this machine.
///
/// ## Why there is no save dialog
///
/// rmux has no dialog plugin, and adding one for this would be the wrong trade:
/// Tauri plugin commands need an explicit ACL grant or they are rejected
/// *silently* — the promise rejects with nothing surfaced — which is a failure
/// mode this app has already been bitten by. The app instead has a convention
/// that works and is already proven by Export Log: **write it where a person can
/// find it, and print the full path.** One click, no second window, and the
/// answer to "where did it go?" is on screen.
///
/// ## The name is made unique rather than overwritten
///
/// `download` refuses to clobber, which is right — but refusing outright would
/// mean pulling the same log twice is an error the operator has to work around
/// by renaming things. So a collision becomes `server (2).log`, the convention
/// every browser uses. Nothing on disk is ever replaced, and nothing is refused
/// for a reason the operator did not cause.
#[tauri::command]
pub async fn fs_download(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
) -> Result<Downloaded, String> {
    let name = path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("download");

    let dir = dirs::download_dir()
        .or_else(dirs::desktop_dir)
        .or_else(dirs::home_dir)
        .ok_or("could not find a folder to download into")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not open {}: {e}", dir.display()))?;

    let dest = unique_path(&dir, name);

    let bytes = filesystem(store.inner(), &target)
        .await?
        .download(&path, &dest)
        .await
        .map_err(err)?;

    Ok(Downloaded { path: dest.to_string_lossy().into_owned(), bytes })
}

/// `report.txt` → `report (2).txt` → `report (3).txt`, until one is free.
///
/// The suffix goes before the extension, not after: `report.txt (2)` stops the
/// file opening in anything, which is a worse outcome than the collision.
fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }

    // Split on the *last* dot, so `archive.tar.gz` becomes `archive.tar (2).gz`
    // rather than `archive (2).tar.gz`. Either is defensible; this one keeps the
    // extension the OS actually dispatches on untouched.
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };

    for n in 2.. {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("an unbounded counter always finds a free name")
}

#[tauri::command]
pub async fn fs_rename(
    store: State<'_, FsStore>,
    target: TargetRef,
    from: String,
    to: String,
) -> Result<(), String> {
    filesystem(store.inner(), &target).await?.rename(&from, &to).await.map_err(err)
}

#[tauri::command]
pub async fn fs_delete(
    store: State<'_, FsStore>,
    target: TargetRef,
    path: String,
) -> Result<(), String> {
    filesystem(store.inner(), &target).await?.delete(&path).await.map_err(err)
}

/// Used by the UI to build child paths without duplicating separator rules.
///
/// Done in Rust because the remote host's separator is not necessarily the local
/// one, and the webview has no way to know which it is talking to.
#[tauri::command]
pub fn fs_join(parent: String, name: String) -> String {
    join_path(&parent, &name)
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        return name.to_owned();
    }
    if parent.ends_with('/') {
        return format!("{parent}{name}");
    }
    format!("{parent}/{name}")
}

/// Hosts from the user's `~/.ssh/config`, for the connection picker.
///
/// Enumeration only — the alias is handed to `ssh` verbatim when connecting, and
/// `ssh` resolves everything else.
#[tauri::command]
pub fn ssh_config_hosts() -> Vec<rmux_ssh::ConfigHost> {
    rmux_ssh::list_hosts()
}

/// The parent of a path, or `None` at the root.
#[tauri::command]
pub fn fs_parent(path: String) -> Option<String> {
    parent_path(&path)
}

fn parent_path(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        // Already at "/" — there is nowhere further up.
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_owned()),
        Some(i) => Some(trimmed[..i].to_owned()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joining_does_not_double_the_separator() {
        assert_eq!(join_path("/home/me", "file.txt"), "/home/me/file.txt");
        // The root is the case that produces "//file.txt" if handled naively.
        assert_eq!(join_path("/", "file.txt"), "/file.txt");
        assert_eq!(join_path("/home/me/", "file.txt"), "/home/me/file.txt");
    }

    #[test]
    fn walking_up_stops_at_the_root() {
        assert_eq!(parent_path("/home/me/file.txt").as_deref(), Some("/home/me"));
        assert_eq!(parent_path("/home/me").as_deref(), Some("/home"));
        assert_eq!(parent_path("/home").as_deref(), Some("/"));
        // Must terminate, or "go up" loops forever at the top of the tree.
        assert_eq!(parent_path("/"), None);
        assert_eq!(parent_path(""), None);
    }

    #[test]
    fn trailing_separators_do_not_confuse_the_parent() {
        assert_eq!(parent_path("/home/me/").as_deref(), Some("/home"));
    }
}

/// Find text under `root`, on whichever machine it lives on.
///
/// One command for local and remote, like every other filesystem call — the
/// branch is inside the `FileSystem` impl, never here.
#[tauri::command]
pub async fn fs_search(
    store: State<'_, FsStore>,
    target: TargetRef,
    root: String,
    query: SearchQuery,
) -> Result<Vec<SearchHit>, String> {
    // An empty query would match every line in the project and take a long time
    // to say nothing. Refused before it reaches a shell.
    if query.text.trim().is_empty() {
        return Ok(Vec::new());
    }
    filesystem(&store, &target).await?.search(&root, &query).await.map_err(err)
}
