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
