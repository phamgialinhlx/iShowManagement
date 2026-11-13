//! Filesystem IPC.
//!
//! Every command takes a target, so the UI uses one set of calls whether the file
//! is on this machine or on a host across the world.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_fs::{DirEntry, FileContent, FileSystem, LocalFs, PreviewContent, SearchHit, SearchQuery, TargetFs, Transfer};
use rmux_ssh::SshTarget;
use rmux_transport::TargetId;
use serde::Deserialize;

use crate::ctx::Ctx;
use crate::host::Host;
use crate::worktree::{AgentFs, WorktreeStore};
use crate::registry::{Call, NoArgs, Registry, Web};
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
    ctx: &Arc<Ctx>,
    target: &TargetRef,
) -> Result<Arc<dyn FileSystem>, String> {
    let store = ctx.store::<FsStore>();
    let id = target.id();

    if let Some(existing) = store.filesystems.lock().get(&id) {
        return Ok(Arc::clone(existing));
    }

    // The shell path: what answers when `fs-serve` cannot (Windows, an older
    // agent, a dev build with no agents) or is not up yet.
    let fallback: Arc<dyn FileSystem> = match &id {
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
    // Every operation prefers the long-lived `fs-serve` connection and falls
    // back to `fallback` transparently — see `worktree`.
    let server = ctx.store::<WorktreeStore>().server(ctx, target);
    let fs: Arc<dyn FileSystem> = Arc::new(AgentFs::new(server, fallback));

    store.filesystems.lock().insert(id, Arc::clone(&fs));
    Ok(fs)
}

pub async fn fs_list(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
) -> Result<Vec<DirEntry>, String> {
    filesystem(ctx, &target).await?.list_dir(&path).await.map_err(err)
}

pub async fn fs_read(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
) -> Result<FileContent, String> {
    filesystem(ctx, &target).await?.read_file(&path).await.map_err(err)
}

pub async fn fs_write(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
    contents: String,
) -> Result<(), String> {
    filesystem(ctx, &target).await?.write_file(&path, &contents).await.map_err(err)
}

/// Read a file as base64, for formats the text editor cannot show.
pub async fn fs_preview(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
) -> Result<PreviewContent, String> {
    filesystem(ctx, &target).await?.read_preview(&path).await.map_err(err)
}

pub async fn fs_home(ctx: &Arc<Ctx>, target: TargetRef) -> Result<String, String> {
    filesystem(ctx, &target).await?.home_dir().await.map_err(err)
}

pub async fn fs_create_file(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
) -> Result<(), String> {
    filesystem(ctx, &target).await?.create_file(&path).await.map_err(err)
}

pub async fn fs_create_dir(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
) -> Result<(), String> {
    filesystem(ctx, &target).await?.create_dir(&path).await.map_err(err)
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
pub async fn fs_upload(
    ctx: &Arc<Ctx>,
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

    filesystem(ctx, &target).await?.upload(&path, &bytes).await.map_err(err)
}

/// Where an upload is assembled before it is streamed to the target.
///
/// ## Why stage at all
///
/// The webview holds the dropped file as a `File` with no path, so its bytes can
/// only cross the IPC bridge as JSON. In one piece that capped uploads at 64 MB
/// — a base64 string of the whole file, built, copied and parsed on both sides.
/// In chunks appended here, peak memory is one chunk whatever the file's size,
/// and [`FileSystem::upload_file`] then streams it from disk.
///
/// ## Why the cache directory, `0700`/`0600`
///
/// A dropped file is frequently private, and a world-readable staging copy of
/// it is a leak that outlives the upload. The app's own cache directory rather
/// than `/tmp`: on Linux that is shared, and a predictable name there can be
/// pre-created by another account.
fn staging_dir(host: &dyn Host) -> Result<std::path::PathBuf, String> {
    let dir = host.cache_dir().map_err(err)?.join("uploads");
    std::fs::create_dir_all(&dir).map_err(err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(err)?;
    }
    Ok(dir)
}

/// The staged file for `id`.
///
/// The id arrives from the webview, so it is checked to be the shape
/// [`fs_upload_begin`] issues — hex only — before it is joined onto a path. An
/// id of `../../.ssh/authorized_keys` would otherwise be an append to any file
/// this user can write.
fn staged(host: &dyn Host, id: &str) -> Result<std::path::PathBuf, String> {
    let well_formed = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit());
    if !well_formed {
        return Err("not an upload this app started".to_owned());
    }
    Ok(staging_dir(host)?.join(format!("{id}.part")))
}

/// Staged files older than this belong to an upload that never finished — the
/// app quit or crashed mid-transfer. Nobody can resume them, so they are litter.
const STALE_STAGING: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Start staging an upload; returns the id its chunks are sent under.
pub async fn fs_upload_begin(host: &dyn Host) -> Result<String, String> {
    let dir = staging_dir(host)?;

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > STALE_STAGING);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let id = format!(
        "{nanos:x}{:x}{:04x}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(dir.join(format!("{id}.part"))).map_err(err)?;
    Ok(id)
}

/// Append one base64 chunk to a staged upload.
pub async fn fs_upload_chunk(host: &dyn Host, id: String, base64: String) -> Result<(), String> {
    use base64::Engine as _;
    use std::io::Write as _;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64.as_bytes())
        .map_err(|e| format!("could not decode the upload: {e}"))?;
    // `append` without `create`: a chunk for an upload that was never begun, or
    // already finished, is refused rather than starting a fresh file.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(staged(host, &id)?)
        .map_err(|_| "this upload is no longer in progress".to_owned())?;
    file.write_all(&bytes).map_err(err)?;
    Ok(())
}

/// Progress of [`fs_upload_finish`], emitted while it streams.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadProgress {
    id: String,
    sent: u64,
    size: u64,
}

/// Send a staged upload to the target, then discard the staging copy.
///
/// Emits `fs-upload-progress` twice a second while it runs: the network leg is
/// the slow one, and a large upload that reports nothing for minutes reads as
/// a hung app. The count is bytes handed to `ssh`, so it is measured, not
/// estimated — it runs ahead of the far disk only by the pipe's buffer.
///
/// `size` is the length the webview read. A staged file of any other length
/// means a chunk went missing, and sending it would write a truncated file that
/// looks complete — so it is refused, the same rule reads follow.
pub async fn fs_upload_finish(
    host: &dyn Host,
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
    id: String,
    size: u64,
) -> Result<(), String> {
    let local = staged(host, &id)?;
    let transfers = ctx.store::<LiveTransfers>();
    let live = transfers.begin(&id);

    let result = async {
        let staged_len = std::fs::metadata(&local).map_err(err)?.len();
        if staged_len != size {
            return Err(format!(
                "{path} was not uploaded — only {staged_len} of {size} bytes arrived"
            ));
        }
        let fs = filesystem(ctx, &target).await?;
        let upload = fs.upload_file(&path, &local, &live.progress);
        tokio::pin!(upload);

        // ~4/s: often enough that a bar visibly moves, rarely enough that a
        // fast link does not flood the bridge with events nobody can read.
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                done = &mut upload => break done.map_err(err),
                _ = tick.tick() => {
                    let sent = live.progress.done();
                    let progress = UploadProgress { id: id.clone(), sent, size };
                    if let Ok(payload) = serde_json::to_value(progress) {
                        host.emit("fs-upload-progress", payload);
                    }
                }
            }
        }
    }
    .await;

    transfers.end(&id);
    let _ = std::fs::remove_file(&local);
    result
}

/// Discard a staged upload that will not be finished.
///
/// Also the upload's **cancel**: if its staged copy is already being sent, the
/// send is told to stop, and the far side removes what it had written (the
/// fs-serve `UploadAbort`, or the shell script's length check). Removing only
/// the staging copy would not stop a send already reading it — on Unix an open
/// file outlives its name.
pub async fn fs_upload_abort(host: &dyn Host, ctx: &Arc<Ctx>, id: String) -> Result<(), String> {
    let path = staged(host, &id)?;
    ctx.store::<LiveTransfers>().cancel(&id);
    let _ = std::fs::remove_file(path);
    Ok(())
}

/// Transfers in flight, by the id the UI knows them by, so a cancel can reach
/// one that is running in another call.
#[derive(Default)]
pub struct LiveTransfers {
    live: Mutex<HashMap<String, Arc<Live>>>,
}

pub struct Live {
    pub progress: Transfer,
    /// Wakes a waiting download at once, rather than at its next chunk — a
    /// chunk is 8 MB, which on a slow link is the better part of a minute.
    stop: tokio::sync::Notify,
}

impl LiveTransfers {
    fn begin(&self, id: &str) -> Arc<Live> {
        let live = Arc::new(Live { progress: Transfer::default(), stop: tokio::sync::Notify::new() });
        self.live.lock().insert(id.to_owned(), Arc::clone(&live));
        live
    }

    fn end(&self, id: &str) {
        self.live.lock().remove(id);
    }

    /// Whether anything was running under `id`.
    pub fn cancel(&self, id: &str) -> bool {
        let Some(live) = self.live.lock().get(id).cloned() else { return false };
        live.progress.cancel();
        // `notify_one` stores a permit, so a cancel that lands before the
        // download reaches its `select!` is not lost.
        live.stop.notify_one();
        true
    }
}

/// What a completed download tells the operator.
#[derive(Debug, serde::Serialize)]
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
pub async fn fs_download(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
    transfer: Option<String>,
    progress: Option<Arc<dyn crate::host::Sink>>,
) -> Result<Downloaded, String> {
    let dir = download_dir()?;
    let fs = filesystem(ctx, &target).await?;

    // Without an id the UI cannot cancel it, but it still gets a private one
    // so two concurrent downloads never share a live entry.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = transfer.unwrap_or_else(|| {
        format!("anon-{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    });
    let transfers = ctx.store::<LiveTransfers>();
    let live = transfers.begin(&id);
    let result = download_into(fs.as_ref(), &path, &dir, &live, |done, total| {
        if let Some(sink) = &progress {
            let _ = sink.send_json(serde_json::json!({ "id": id, "done": done, "total": total }));
        }
    })
    .await;
    transfers.end(&id);
    result
}

/// Where downloads land: Downloads, else the Desktop, else home.
fn download_dir() -> Result<std::path::PathBuf, String> {
    let dir = dirs::download_dir()
        .or_else(dirs::desktop_dir)
        .or_else(dirs::home_dir)
        .ok_or("could not find a folder to download into")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not open {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The body of [`fs_download`]: stage, then move into place.
///
/// ## Temp name, then rename — and why it matters more than it looks
///
/// The bytes go to `<name>.part` and only a *complete* file takes the real
/// name. Written straight to the final name, a download that failed half way —
/// a dropped connection, a short read, a cancel — left a truncated file sitting
/// exactly where the operator would look for the real one, indistinguishable
/// from it until opened. A failed or cancelled download now leaves nothing.
///
/// The move refuses to clobber: `hard_link` fails if the name was taken while
/// the download ran, and then the next free name is chosen instead.
pub async fn download_into(
    fs: &dyn FileSystem,
    path: &str,
    dir: &std::path::Path,
    live: &Live,
    mut report: impl FnMut(u64, Option<u64>),
) -> Result<Downloaded, String> {
    let name = path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("download");
    let staging = unique_path(dir, &format!("{name}.part"));

    // Boxed rather than `pin!`ned so the `drop` below really ends it: the
    // future holds the staging file open, and on Windows an open file cannot
    // be removed.
    let mut download = Box::pin(fs.download(path, &staging, &live.progress));
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    let outcome = loop {
        tokio::select! {
            done = &mut download => break done.map_err(err),
            _ = live.stop.notified() => break Err(rmux_fs::CANCELLED.to_owned()),
            _ = tick.tick() => report(live.progress.done(), live.progress.total()),
        }
    };
    // Dropped before the cleanup, so nothing is still writing the staging file
    // when it is removed.
    drop(download);

    let bytes = match outcome {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            return Err(if live.progress.is_cancelled() { rmux_fs::CANCELLED.to_owned() } else { e });
        }
    };
    report(bytes, Some(bytes));

    let mut dest = unique_path(dir, name);
    let mut placed = false;
    for _ in 0..16 {
        match std::fs::hard_link(&staging, &dest) {
            Ok(()) => {
                let _ = std::fs::remove_file(&staging);
                placed = true;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => dest = unique_path(dir, name),
            // A filesystem without hard links (FAT, some network shares):
            // check-then-rename, the one non-atomic step, named here.
            Err(_) if !dest.exists() => {
                if std::fs::rename(&staging, &dest).is_ok() {
                    placed = true;
                }
                break;
            }
            Err(_) => dest = unique_path(dir, name),
        }
    }
    if !placed {
        let _ = std::fs::remove_file(&staging);
        return Err(format!("downloaded {name}, but could not move it into {}", dir.display()));
    }

    Ok(Downloaded { path: dest.to_string_lossy().into_owned(), bytes })
}

/// Stop a running download; its staging file is removed.
pub fn fs_download_cancel(ctx: &Arc<Ctx>, transfer: &str) -> bool {
    ctx.store::<LiveTransfers>().cancel(transfer)
}

/// Show a finished download in Finder (or open its folder elsewhere).
///
/// **Only inside the downloads folder.** This is a "launch something on this
/// machine" bridge reachable from the webview; restricting it to files that
/// [`fs_download`] could have written keeps it from opening an arbitrary path.
pub async fn fs_reveal_download(path: String) -> Result<(), String> {
    use rmux_transport::NoConsoleWindow as _;

    let dir = download_dir()?.canonicalize().map_err(err)?;
    let file = std::path::Path::new(&path)
        .canonicalize()
        .map_err(|_| "that file is no longer there — it may have been moved or deleted".to_owned())?;
    if !file.starts_with(&dir) {
        return Err("only files in the downloads folder can be shown".to_owned());
    }

    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = tokio::process::Command::new("open");
        c.arg("-R").arg(&file);
        c
    } else {
        let folder = file.parent().unwrap_or(&dir).to_path_buf();
        let mut c = tokio::process::Command::new(if cfg!(target_os = "windows") { "explorer" } else { "xdg-open" });
        c.arg(folder);
        c
    };
    cmd.no_console_window().spawn().map_err(|e| format!("could not open the folder: {e}"))?;
    Ok(())
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

pub async fn fs_rename(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    from: String,
    to: String,
) -> Result<(), String> {
    filesystem(ctx, &target).await?.rename(&from, &to).await.map_err(err)
}

pub async fn fs_delete(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
) -> Result<(), String> {
    filesystem(ctx, &target).await?.delete(&path).await.map_err(err)
}

/// Used by the UI to build child paths without duplicating separator rules.
///
/// Done in Rust because the remote host's separator is not necessarily the local
/// one, and the webview has no way to know which it is talking to.
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
pub fn ssh_config_hosts() -> Vec<rmux_ssh::ConfigHost> {
    rmux_ssh::list_hosts()
}

/// The parent of a path, or `None` at the root.
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

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rmux-dl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn live() -> Live {
        Live { progress: Transfer::default(), stop: tokio::sync::Notify::new() }
    }

    #[tokio::test]
    async fn a_download_is_staged_then_renamed_into_place() {
        let src = scratch("src-ok");
        let out = scratch("out-ok");
        let file = src.join("report.txt");
        std::fs::write(&file, vec![7u8; 3 * 1024 * 1024 + 5]).unwrap();
        // A file already holding the name: the download must not replace it.
        std::fs::write(out.join("report.txt"), b"mine").unwrap();

        let mut reports = Vec::new();
        let got = download_into(&LocalFs, file.to_str().unwrap(), &out, &live(), |d, t| reports.push((d, t)))
            .await
            .unwrap();

        assert_eq!(got.bytes, 3 * 1024 * 1024 + 5);
        assert!(got.path.ends_with("report (2).txt"), "{}", got.path);
        assert_eq!(names(&out), vec!["report (2).txt", "report.txt"], "a staging file was left behind");
        assert_eq!(std::fs::read(out.join("report.txt")).unwrap(), b"mine");
        let last = reports.last().copied().unwrap();
        assert_eq!(last, (got.bytes, Some(got.bytes)));
        assert!(reports.iter().all(|(d, t)| t.is_none_or(|t| *d <= t)), "progress passed the total");
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(out);
    }

    #[tokio::test]
    async fn a_cancelled_download_leaves_no_file_under_any_name() {
        let src = scratch("src-cancel");
        let out = scratch("out-cancel");
        let file = src.join("big.bin");
        std::fs::write(&file, vec![1u8; 8 * 1024 * 1024]).unwrap();

        let l = live();
        l.progress.cancel();
        l.stop.notify_one();
        let e = download_into(&LocalFs, file.to_str().unwrap(), &out, &l, |_, _| {}).await.unwrap_err();
        assert_eq!(e, rmux_fs::CANCELLED);
        assert!(names(&out).is_empty(), "left behind: {:?}", names(&out));
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(out);
    }

    #[tokio::test]
    async fn a_failed_download_leaves_no_partial_file() {
        let out = scratch("out-fail");
        let e = download_into(&LocalFs, "/definitely/not/here.bin", &out, &live(), |_, _| {}).await.unwrap_err();
        assert!(!e.is_empty());
        assert!(names(&out).is_empty(), "left behind: {:?}", names(&out));
        let _ = std::fs::remove_dir_all(out);
    }

    #[tokio::test]
    async fn a_cancel_reaches_a_running_transfer_by_id() {
        let t = LiveTransfers::default();
        assert!(!t.cancel("nope"));
        let l = t.begin("tx-1");
        assert!(t.cancel("tx-1"));
        assert!(l.progress.is_cancelled());
        t.end("tx-1");
        assert!(!t.cancel("tx-1"));
    }
}

/// Find text under `root`, on whichever machine it lives on.
///
/// One command for local and remote, like every other filesystem call — the
/// branch is inside the `FileSystem` impl, never here.
pub async fn fs_search(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    root: String,
    query: SearchQuery,
) -> Result<Vec<SearchHit>, String> {
    // An empty query would match every line in the project and take a long time
    // to say nothing. Refused before it reaches a shell.
    if query.text.trim().is_empty() {
        return Ok(Vec::new());
    }
    filesystem(ctx, &target).await?.search(&root, &query).await.map_err(err)
}

/// A file plus its version token, for an editor that will save it back.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Versioned {
    pub content: FileContent,
    /// Opaque; `None` when the backend cannot vouch for one (the shell path),
    /// in which case saves are not conflict-checked.
    pub version: Option<String>,
}

pub async fn fs_read_versioned(ctx: &Arc<Ctx>, target: TargetRef, path: String) -> Result<Versioned, String> {
    let (content, version) = filesystem(ctx, &target).await?.read_versioned(&path).await.map_err(err)?;
    Ok(Versioned { content, version })
}

/// Save, refusing when the file changed on disk since `expected` was read.
///
/// A refusal is an error that starts with [`crate::worktree::CONFLICT_PREFIX`],
/// so the editor can offer *reload / keep mine* rather than a bare failure —
/// never silently overwriting either side.
pub async fn fs_write_versioned(
    ctx: &Arc<Ctx>,
    target: TargetRef,
    path: String,
    contents: String,
    expected: Option<String>,
) -> Result<Option<String>, String> {
    filesystem(ctx, &target)
        .await?
        .write_versioned(&path, &contents, expected.as_deref())
        .await
        .map_err(err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsWriteVersionedArgs {
    pub target: TargetRef,
    pub path: String,
    pub contents: String,
    #[serde(default)]
    pub expected: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeOpenArgs {
    pub target: TargetRef,
    pub root: String,
}

// ── Registration ─────────────────────────────────────────────────────────────
//
// Argument objects, keyed exactly as `ui/src/lib/api.ts` sends them.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetArgs {
    pub target: TargetRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetPathArgs {
    pub target: TargetRef,
    pub path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsWriteArgs {
    pub target: TargetRef,
    pub path: String,
    pub contents: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsUploadArgs {
    pub target: TargetRef,
    pub path: String,
    pub base64: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsUploadChunkArgs {
    pub id: String,
    pub base64: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsUploadFinishArgs {
    pub target: TargetRef,
    pub path: String,
    pub id: String,
    pub size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsDownloadArgs {
    pub target: TargetRef,
    pub path: String,
    /// The UI's id for this transfer — what a cancel names.
    #[serde(default)]
    pub transfer: Option<String>,
    /// A channel for `{id, done, total}` progress, ~4/s.
    #[serde(default)]
    pub progress: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferIdArgs {
    pub transfer: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadIdArgs {
    pub id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsRenameArgs {
    pub target: TargetRef,
    pub from: String,
    pub to: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsJoinArgs {
    pub parent: String,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathArgs {
    pub path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsSearchArgs {
    pub target: TargetRef,
    pub root: String,
    pub query: SearchQuery,
}

fn store(call: &Call) -> Arc<Ctx> {
    call.ctx_arc()
}

pub fn register(r: &mut Registry) {
    r.add("fs_list", Web::Yes, |call: Call, a: TargetPathArgs| async move {
        fs_list(&store(&call), a.target, a.path).await
    });
    r.add("fs_read", Web::Yes, |call: Call, a: TargetPathArgs| async move {
        fs_read(&store(&call), a.target, a.path).await
    });
    r.add("fs_write", Web::Yes, |call: Call, a: FsWriteArgs| async move {
        fs_write(&store(&call), a.target, a.path, a.contents).await
    });
    r.add("fs_preview", Web::Yes, |call: Call, a: TargetPathArgs| async move {
        fs_preview(&store(&call), a.target, a.path).await
    });
    r.add("fs_home", Web::Yes, |call: Call, a: TargetArgs| async move {
        fs_home(&store(&call), a.target).await
    });
    r.add("fs_create_file", Web::Yes, |call: Call, a: TargetPathArgs| async move {
        fs_create_file(&store(&call), a.target, a.path).await
    });
    r.add("fs_create_dir", Web::Yes, |call: Call, a: TargetPathArgs| async move {
        fs_create_dir(&store(&call), a.target, a.path).await
    });
    r.add("fs_upload", Web::Yes, |call: Call, a: FsUploadArgs| async move {
        fs_upload(&store(&call), a.target, a.path, a.base64).await
    });
    r.add("fs_upload_begin", Web::Yes, |call: Call, _: NoArgs| async move {
        fs_upload_begin(call.ctx().host()).await
    });
    r.add("fs_upload_chunk", Web::Yes, |call: Call, a: FsUploadChunkArgs| async move {
        fs_upload_chunk(call.ctx().host(), a.id, a.base64).await
    });
    r.add("fs_upload_finish", Web::Yes, |call: Call, a: FsUploadFinishArgs| async move {
        fs_upload_finish(call.ctx().host(), &store(&call), a.target, a.path, a.id, a.size).await
    });
    r.add("fs_upload_abort", Web::Yes, |call: Call, a: UploadIdArgs| async move {
        fs_upload_abort(call.ctx().host(), &store(&call), a.id).await
    });
    // **Not on the web.** It writes into the Downloads folder of the machine
    // rmux runs on — for rmux-web that is the dev host, not the device in the
    // operator's hand, so it would report a path they cannot open. The web
    // needs a streamed download to the browser instead, which does not exist yet.
    r.add("fs_download", Web::No, |call: Call, a: FsDownloadArgs| async move {
        let progress = match &a.progress {
            Some(channel) => Some(call.sink(channel).map_err(err)?),
            None => None,
        };
        fs_download(&store(&call), a.target, a.path, a.transfer, progress).await
    });
    // Same reasoning as `fs_download`: only the desktop runs downloads.
    r.add("fs_download_cancel", Web::No, |call: Call, a: TransferIdArgs| async move {
        Ok::<_, String>(fs_download_cancel(&store(&call), &a.transfer))
    });
    // Launches Finder/Explorer on *this* machine, which on the web is the
    // dev host rather than the device in the operator's hand.
    r.add("fs_reveal_download", Web::No, |_, a: PathArgs| async move {
        fs_reveal_download(a.path).await
    });
    r.add("fs_rename", Web::Yes, |call: Call, a: FsRenameArgs| async move {
        fs_rename(&store(&call), a.target, a.from, a.to).await
    });
    r.add("fs_delete", Web::Yes, |call: Call, a: TargetPathArgs| async move {
        fs_delete(&store(&call), a.target, a.path).await
    });
    r.add("fs_join", Web::Yes, |_, a: FsJoinArgs| async move {
        Ok::<_, String>(fs_join(a.parent, a.name))
    });
    r.add("ssh_config_hosts", Web::Yes, |_, _: NoArgs| async move {
        Ok::<_, String>(ssh_config_hosts())
    });
    r.add("fs_parent", Web::Yes, |_, a: PathArgs| async move {
        Ok::<_, String>(fs_parent(a.path))
    });
    r.add("fs_search", Web::Yes, |call: Call, a: FsSearchArgs| async move {
        fs_search(&store(&call), a.target, a.root, a.query).await
    });
    r.add("fs_read_versioned", Web::Yes, |call: Call, a: TargetPathArgs| async move {
        fs_read_versioned(&store(&call), a.target, a.path).await
    });
    r.add("fs_write_versioned", Web::Yes, |call: Call, a: FsWriteVersionedArgs| async move {
        fs_write_versioned(&store(&call), a.target, a.path, a.contents, a.expected).await
    });
    // The worktree mirror (`worktree`): a snapshot now, `fs-worktree` patches
    // after. An `unsupported: …` error means this host is on the shell path and
    // the tree should list lazily instead.
    r.add("fs_worktree_open", Web::Yes, |call: Call, a: WorktreeOpenArgs| async move {
        crate::worktree::open(&call.ctx_arc(), a.target, a.root).await
    });
}
