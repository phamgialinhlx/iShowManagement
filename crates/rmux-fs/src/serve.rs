//! `fs-serve` — the filesystem server, on the machine that owns the disk.
//!
//! This is rmux's version of Zed's headless remote server. One long-lived
//! process per host speaks [`crate::wire`] over stdin/stdout — which, through
//! `ssh -T`, means over the ControlMaster connection rmux already holds. There
//! is no listening socket and no new network surface: it runs with the user's
//! own rights and exits when its stdin closes.
//!
//! ## What it does that a shell command per operation cannot
//!
//! - **A worktree snapshot**, streamed breadth-first: every entry under a root,
//!   `.gitignore`-aware, sent once. The client keeps the tree, so expanding a
//!   folder or fuzzy-finding a file never touches the network again.
//! - **A watcher** (inotify on Linux, FSEvents on macOS) that streams
//!   incremental changes — debounced and coalesced — so an agent's edit shows up
//!   in the tree and in an open editor within a moment rather than "eventually".
//! - **Search in-process**, walking with the same ignore rules.
//!
//! Ignored directories (`node_modules`, `target`) appear as one *lazy* entry and
//! are listed on demand, never scanned: they routinely outweigh the project.
//!
//! ## The filesystem invariants live here too
//!
//! Every one of `CLAUDE.md`'s rules for the shell path holds for this one:
//! saving copies over the original (never renames onto it), create and rename
//! refuse to clobber atomically, uploads are `O_EXCL`, reads are length-framed,
//! and names are bytes.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::Watcher as _;
use parking_lot::Mutex;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::wire::{
    self, Change, Entry, Envelope, ErrorCode, Found, Frame, Hit, Kind, Listed, Opened, ReadReply,
    Request, ServerMsg, Stat, WireError, WirePath,
};

/// Default ceiling on a worktree snapshot.
///
/// Past this the scan stops, says `truncated`, and the directories it did not
/// reach become lazy — listed on demand like an ignored one. A checkout this
/// size is real (a monorepo with vendored code) and a snapshot that never
/// finishes is worse than one that admits where it stopped.
pub const DEFAULT_LIMIT: usize = 250_000;

/// Entries per snapshot frame. Small enough that the top of the tree reaches
/// the client while the scan is still descending.
const CHUNK: usize = 2_000;

/// Quiet period that ends a burst of watcher events.
const DEBOUNCE: Duration = Duration::from_millis(40);
/// Longest a burst is held before it is flushed anyway — a build writing
/// continuously must not starve the tree of updates.
const MAX_HOLD: Duration = Duration::from_millis(250);

/// Same ceiling as the grep path, and reported the same way.
pub const SEARCH_LIMIT: usize = crate::search::LIMIT;

/// Files larger than this are not searched — a 2GB log is not source.
const SEARCH_MAX_FILE: u64 = 8 * 1024 * 1024;

/// Directories the search skips even when nothing ignores them — the same list
/// as the grep path, so the two backends agree on what "the project" is.
const SEARCH_SKIP: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".next", "vendor"];

type Out = mpsc::UnboundedSender<Frame>;

fn send(out: &Out, msg: &ServerMsg, payload: Vec<u8>) {
    if let Ok(header) = serde_json::to_value(msg) {
        let _ = out.send(Frame::with_payload(header, payload));
    }
}

/// Serve on this process's stdin/stdout until stdin closes.
pub async fn serve_stdio() -> io::Result<()> {
    serve(tokio::io::stdin(), tokio::io::stdout()).await
}

/// Serve over any pipe — stdio on a host, a `tokio::io::duplex` in-process for
/// the local machine. One implementation either way, which is the whole point.
pub async fn serve<R, W>(mut reader: R, mut writer: W) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, mut rx) = mpsc::unbounded_channel::<Frame>();
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if wire::write_frame(&mut writer, &frame).await.is_err() {
                break;
            }
        }
    });

    send(
        &out,
        &ServerMsg::Hello { protocol: wire::PROTOCOL, version: env!("CARGO_PKG_VERSION").to_owned() },
        Vec::new(),
    );

    let state = Arc::new(State::default());
    let result = loop {
        let frame = match wire::read_frame(&mut reader).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        };
        let envelope: Envelope = match serde_json::from_value(frame.header.clone()) {
            Ok(envelope) => envelope,
            Err(e) => {
                // **Answered, never ignored** — the bridge's rule. A newer client
                // asking an older server for something it does not know must get
                // a reply, or it hangs until a timeout and reads that as a dead
                // host. Only a frame with no id at all is unanswerable.
                if let Some(id) = frame.header.get("id").and_then(serde_json::Value::as_u64) {
                    let error = WireError::new(ErrorCode::Unsupported, format!("unsupported request: {e}"));
                    send(&out, &ServerMsg::Err { id, error }, Vec::new());
                }
                continue;
            }
        };
        let id = envelope.id;

        match envelope.request {
            // Uploads are ordered: chunks must land in the order they were sent,
            // so they are handled inline rather than spawned. Everything else
            // runs concurrently — a slow search must not hold up a file read.
            request @ (Request::UploadChunk { .. }
            | Request::UploadFinish { .. }
            | Request::UploadAbort { .. }) => {
                let state = Arc::clone(&state);
                let payload = frame.payload;
                let reply = tokio::task::spawn_blocking(move || handle(&state, None, request, payload))
                    .await
                    .unwrap_or_else(|e| Err(WireError::new(ErrorCode::Io, e.to_string())));
                reply_to(&out, id, reply);
            }
            request => {
                let state = Arc::clone(&state);
                let out = out.clone();
                let payload = frame.payload;
                tokio::task::spawn_blocking(move || {
                    let reply = handle(&state, Some(&out), request, payload);
                    reply_to(&out, id, reply);
                });
            }
        }
    };

    // Stdin closed: stop every watcher thread and let the writer drain.
    state.shutdown();
    drop(out);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer_task).await;
    result
}

fn reply_to(out: &Out, id: u64, reply: Result<(serde_json::Value, Vec<u8>), WireError>) {
    match reply {
        Ok((result, payload)) => send(out, &ServerMsg::Ok { id, result }, payload),
        Err(error) => send(out, &ServerMsg::Err { id, error }, Vec::new()),
    }
}

#[derive(Default)]
struct State {
    worktrees: Mutex<HashMap<u64, std_mpsc::Sender<WtMsg>>>,
    uploads: Mutex<HashMap<u64, Upload>>,
    next: AtomicU64,
}

impl State {
    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn shutdown(&self) {
        for (_, tx) in self.worktrees.lock().drain() {
            let _ = tx.send(WtMsg::Stop);
        }
        // An upload that never finished is a partial file nobody asked for.
        for (_, upload) in self.uploads.lock().drain() {
            drop(upload.file);
            let _ = fs::remove_file(&upload.path);
        }
    }
}

struct Upload {
    path: PathBuf,
    file: fs::File,
    written: u64,
}

type Reply = Result<(serde_json::Value, Vec<u8>), WireError>;

fn ok<T: serde::Serialize>(value: T) -> Reply {
    Ok((serde_json::to_value(value).map_err(|e| WireError::new(ErrorCode::Io, e.to_string()))?, Vec::new()))
}

fn io_err(path: &Path, e: io::Error) -> WireError {
    WireError::new(ErrorCode::from_io(&e), format!("{}: {e}", path.display()))
}

fn handle(state: &State, out: Option<&Out>, request: Request, payload: Vec<u8>) -> Reply {
    match request {
        Request::OpenWorktree { root, limit } => {
            let out = out.ok_or_else(|| WireError::new(ErrorCode::Invalid, "no output"))?;
            let shown = resolve(&root);
            let real = fs::canonicalize(&shown).map_err(|e| io_err(&shown, e))?;
            if !real.is_dir() {
                return Err(WireError::new(ErrorCode::Invalid, format!("{} is not a directory", shown.display())));
            }
            let wt = state.id();
            let (tx, rx) = std_mpsc::channel();
            let worktree = Worktree::new(wt, real, out.clone(), limit.unwrap_or(DEFAULT_LIMIT));
            let watch_tx = tx.clone();
            std::thread::Builder::new()
                .name(format!("fs-worktree-{wt}"))
                .spawn(move || worktree.run(watch_tx, rx))
                .map_err(|e| WireError::new(ErrorCode::Io, e.to_string()))?;
            state.worktrees.lock().insert(wt, tx);
            ok(Opened { wt, root: WirePath::from_path(&shown) })
        }
        Request::CloseWorktree { wt } => {
            if let Some(tx) = state.worktrees.lock().remove(&wt) {
                let _ = tx.send(WtMsg::Stop);
            }
            ok(json!({}))
        }
        Request::ListDir { path } => ok(list_dir(&resolve(&path))?),
        Request::Stat { path } => {
            let path = resolve(&path);
            let meta = fs::symlink_metadata(&path).map_err(|e| io_err(&path, e))?;
            ok(stat_of(&meta))
        }
        Request::Read { path, max } => read(&resolve(&path), max.unwrap_or(crate::MAX_READ_BYTES)),
        Request::Write { path, expected_mtime } => ok(write(&resolve(&path), expected_mtime, &payload)?),
        Request::CreateFile { path } => {
            let path = resolve(&path);
            let file = create_new(&path)?;
            let meta = file.metadata().map_err(|e| io_err(&path, e))?;
            ok(stat_of(&meta))
        }
        Request::CreateDir { path } => {
            let path = resolve(&path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            }
            // `create_dir`, not `create_dir_all`: the latter succeeds on an
            // existing directory, which is a silent "done" for a name the
            // operator thought was new.
            fs::create_dir(&path).map_err(|e| exists_or(&path, e))?;
            ok(json!({}))
        }
        Request::Rename { from, to } => {
            rename_noclobber(&resolve(&from), &resolve(&to))?;
            ok(json!({}))
        }
        Request::Delete { path, recursive } => {
            delete(&resolve(&path), recursive)?;
            ok(json!({}))
        }
        Request::UploadBegin { path } => {
            let path = resolve(&path);
            let file = create_new(&path)?;
            let upload = state.id();
            state.uploads.lock().insert(upload, Upload { path, file, written: 0 });
            ok(json!({ "upload": upload }))
        }
        Request::UploadChunk { upload } => {
            let mut uploads = state.uploads.lock();
            let entry = uploads
                .get_mut(&upload)
                .ok_or_else(|| WireError::new(ErrorCode::Invalid, "this upload is no longer in progress"))?;
            if let Err(e) = entry.file.write_all(&payload) {
                let entry = uploads.remove(&upload).expect("present");
                let _ = fs::remove_file(&entry.path);
                return Err(io_err(&entry.path, e));
            }
            entry.written += payload.len() as u64;
            ok(json!({ "written": entry.written }))
        }
        Request::UploadFinish { upload, size } => {
            let entry = state
                .uploads
                .lock()
                .remove(&upload)
                .ok_or_else(|| WireError::new(ErrorCode::Invalid, "this upload is no longer in progress"))?;
            // A short upload is refused, the same rule reads follow: a file that
            // stops early must not be left looking complete.
            if entry.written != size {
                drop(entry.file);
                let _ = fs::remove_file(&entry.path);
                return Err(WireError::new(
                    ErrorCode::Io,
                    format!("{} was not uploaded — only {} of {size} bytes arrived", entry.path.display(), entry.written),
                ));
            }
            entry.file.sync_all().map_err(|e| io_err(&entry.path, e))?;
            ok(json!({ "written": entry.written }))
        }
        Request::UploadAbort { upload } => {
            if let Some(entry) = state.uploads.lock().remove(&upload) {
                drop(entry.file);
                let _ = fs::remove_file(&entry.path);
            }
            ok(json!({}))
        }
        Request::Search { root, query } => ok(search(&resolve(&root), &query)?),
        Request::Home => {
            let home = dirs::home_dir().ok_or_else(|| WireError::new(ErrorCode::NotFound, "no home directory"))?;
            ok(WirePath::from_path(&home))
        }
    }
}

/// Expand a leading `~` — the shell path does this through `shell_quote_path`,
/// and a project folder stored as `~/src/app` must mean the same thing here.
fn resolve(path: &WirePath) -> PathBuf {
    let bytes = path.as_bytes();
    if (bytes == b"~" || bytes.starts_with(b"~/"))
        && let Some(home) = dirs::home_dir() {
            let rest = WirePath(bytes[1..].trim_ascii_start_slash().to_vec());
            return if rest.0.is_empty() { home } else { home.join(rest.to_path_buf()) };
        }
    path.to_path_buf()
}

trait TrimSlash {
    fn trim_ascii_start_slash(&self) -> &[u8];
}
impl TrimSlash for [u8] {
    fn trim_ascii_start_slash(&self) -> &[u8] {
        let mut s = self;
        while let [b'/', rest @ ..] = s {
            s = rest;
        }
        s
    }
}

fn mtime_ns(meta: &fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap_or(i64::MAX),
        },
        Err(_) => 0,
    }
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn mode_of(meta: &fs::Metadata) -> u32 {
    if meta.permissions().readonly() { 0o444 } else { 0o644 }
}

fn kind_of(file_type: fs::FileType) -> Kind {
    if file_type.is_symlink() {
        Kind::Symlink
    } else if file_type.is_dir() {
        Kind::Dir
    } else {
        Kind::File
    }
}

fn stat_of(meta: &fs::Metadata) -> Stat {
    Stat { kind: kind_of(meta.file_type()), size: meta.len(), mtime: mtime_ns(meta), mode: mode_of(meta) }
}

fn list_dir(path: &Path) -> Result<Vec<Listed>, WireError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(path).map_err(|e| io_err(path, e))? {
        let Ok(entry) = entry else { continue };
        // `symlink_metadata`: a link to a directory is reported as a link, the
        // same as the shell path — following it would let a tree loop forever.
        let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
        out.push(Listed {
            name: WirePath::from_path(Path::new(&entry.file_name())),
            kind: kind_of(meta.file_type()),
            size: meta.len(),
            mtime: mtime_ns(&meta),
            mode: mode_of(&meta),
        });
    }
    Ok(out)
}

fn read(path: &Path, max: u64) -> Reply {
    let mut file = fs::File::open(path).map_err(|e| io_err(path, e))?;
    let meta = file.metadata().map_err(|e| io_err(path, e))?;
    if meta.is_dir() {
        return Err(WireError::new(ErrorCode::IsDir, format!("{} is a directory", path.display())));
    }
    // Checked before reading, so a huge file never enters memory.
    if meta.len() > max {
        return Err(WireError {
            code: ErrorCode::TooLarge,
            message: format!("{} is {} bytes — too large to open", path.display(), meta.len()),
            current: i64::try_from(meta.len()).ok(),
        });
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // `take(max + 1)` so a file growing under us is still bounded, and still
    // detectably over the limit.
    (&mut file).take(max + 1).read_to_end(&mut bytes).map_err(|e| io_err(path, e))?;
    if bytes.len() as u64 > max {
        return Err(WireError::new(ErrorCode::TooLarge, format!("{} grew past the limit while reading", path.display())));
    }
    let reply = ReadReply { size: bytes.len() as u64, mtime: mtime_ns(&meta), binary: crate::protocol::looks_binary(&bytes) };
    Ok((serde_json::to_value(reply).expect("plain struct"), bytes))
}

/// Save, by **copying over the original**.
///
/// Opened for writing and truncated in place — the same inode, so the mode,
/// the owner and every hard link survive. A write-to-temp-then-rename would be
/// "atomic" and would silently turn a `0600` secret into a `0644` one and split
/// a hard link into two files. `CLAUDE.md` has the rule; the tests have the proof.
///
/// `expected_mtime` is the editor's version token. Checked immediately before
/// the write; the window between the check and the `open` is a few
/// microseconds, which is the same race every editor accepts — what this
/// catches is the agent that rewrote the file ten minutes ago.
fn write(path: &Path, expected_mtime: Option<i64>, bytes: &[u8]) -> Result<Stat, WireError> {
    if let Some(expected) = expected_mtime {
        match fs::metadata(path) {
            Ok(meta) => {
                let current = mtime_ns(&meta);
                if current != expected {
                    return Err(WireError {
                        code: ErrorCode::Conflict,
                        message: format!("{} changed on disk since it was opened", path.display()),
                        current: Some(current),
                    });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(WireError::new(
                    ErrorCode::Conflict,
                    format!("{} was deleted on disk since it was opened", path.display()),
                ));
            }
            Err(e) => return Err(io_err(path, e)),
        }
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| io_err(path, e))?;
    file.write_all(bytes).map_err(|e| io_err(path, e))?;
    file.flush().map_err(|e| io_err(path, e))?;
    let meta = file.metadata().map_err(|e| io_err(path, e))?;
    Ok(stat_of(&meta))
}

fn exists_or(path: &Path, e: io::Error) -> WireError {
    if e.kind() == io::ErrorKind::AlreadyExists {
        WireError::new(ErrorCode::Exists, format!("{} already exists", path.display()))
    } else {
        io_err(path, e)
    }
}

/// `O_EXCL` — the refusal *is* the open, so there is no check to race.
fn create_new(path: &Path) -> Result<fs::File, WireError> {
    fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| exists_or(path, e))
}

/// Rename without ever replacing the destination.
///
/// Linux: `renameat2(RENAME_NOREPLACE)`, atomic. macOS: `renamex_np(RENAME_EXCL)`,
/// atomic. Where the filesystem refuses those flags (some network and FUSE
/// filesystems answer `EINVAL`), a file falls back to `link` + `unlink` — `link`
/// fails with `EEXIST` atomically, so the refusal is still not a check. Only a
/// *directory* on such a filesystem uses check-then-rename, which can race with
/// a concurrent create of the same name; that is the one case with no atomic
/// primitive available, and it is named here rather than pretended away.
fn rename_noclobber(from: &Path, to: &Path) -> Result<(), WireError> {
    match atomic_rename_noreplace(from, to) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(exists_or(to, e)),
        Err(e) if !unsupported_flag(&e) => return Err(io_err(from, e)),
        Err(_) => {}
    }
    let meta = fs::symlink_metadata(from).map_err(|e| io_err(from, e))?;
    if !meta.is_dir() {
        fs::hard_link(from, to).map_err(|e| exists_or(to, e))?;
        if let Err(e) = fs::remove_file(from) {
            let _ = fs::remove_file(to);
            return Err(io_err(from, e));
        }
        return Ok(());
    }
    if fs::symlink_metadata(to).is_ok() {
        return Err(WireError::new(ErrorCode::Exists, format!("{} already exists", to.display())));
    }
    fs::rename(from, to).map_err(|e| io_err(from, e))
}

fn unsupported_flag(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(code) if code == libc::EINVAL || code == libc::ENOSYS || code == libc::ENOTSUP)
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        true
    }
}

#[cfg(target_os = "linux")]
fn atomic_rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    let from = CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let to = CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // The raw syscall rather than the libc wrapper: musl only grew `renameat2`
    // recently, and the agent is a static musl binary.
    const RENAME_NOREPLACE: libc::c_uint = 1;
    // SAFETY: both strings are valid NUL-terminated C strings for the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(target_os = "macos")]
fn atomic_rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    let from = CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let to = CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: both strings are valid NUL-terminated C strings for the call.
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn atomic_rename_noreplace(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no atomic no-replace rename here"))
}

fn delete(path: &Path, recursive: bool) -> Result<(), WireError> {
    let meta = fs::symlink_metadata(path).map_err(|e| io_err(path, e))?;
    // `symlink_metadata`, so a link to a directory is unlinked — never followed
    // into and emptied.
    if meta.is_dir() {
        if recursive {
            fs::remove_dir_all(path).map_err(|e| io_err(path, e))
        } else {
            fs::remove_dir(path).map_err(|e| io_err(path, e))
        }
    } else {
        fs::remove_file(path).map_err(|e| io_err(path, e))
    }
}

fn search(root: &Path, query: &wire::Search) -> Result<Found, WireError> {
    use std::sync::atomic::AtomicBool;

    let pattern = if query.regex { query.text.clone() } else { regex::escape(&query.text) };
    let matcher = regex::bytes::RegexBuilder::new(&pattern)
        .case_insensitive(!query.case_sensitive)
        .build()
        .map_err(|e| WireError::new(ErrorCode::Invalid, format!("not a valid pattern: {e}")))?;
    let limit = query.limit.unwrap_or(SEARCH_LIMIT).min(SEARCH_LIMIT);

    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        // A project that is not a git checkout still means its `.gitignore`.
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| !(e.file_type().is_some_and(|t| t.is_dir()) && SEARCH_SKIP.iter().any(|s| e.file_name() == *s)));
    if !query.globs.is_empty() {
        let mut overrides = ignore::overrides::OverrideBuilder::new(root);
        for glob in &query.globs {
            overrides.add(glob).map_err(|e| WireError::new(ErrorCode::Invalid, e.to_string()))?;
        }
        builder.overrides(overrides.build().map_err(|e| WireError::new(ErrorCode::Invalid, e.to_string()))?);
    }

    let hits = Mutex::new(Vec::<Hit>::new());
    let full = AtomicBool::new(false);
    builder.build_parallel().run(|| {
        let matcher = &matcher;
        let hits = &hits;
        let full = &full;
        Box::new(move |entry| {
            use ignore::WalkState;
            if full.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else { return WalkState::Continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            if entry.metadata().map(|m| m.len() > SEARCH_MAX_FILE).unwrap_or(true) {
                return WalkState::Continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else { return WalkState::Continue };
            // Binaries are skipped, not mangled — `grep -I`'s rule.
            if crate::protocol::looks_binary(&bytes) {
                return WalkState::Continue;
            }
            let mut found = Vec::new();
            for (n, line) in bytes.split(|b| *b == b'\n').enumerate() {
                if matcher.is_match(line) {
                    let text = String::from_utf8_lossy(line);
                    let text: String = text.trim_end().chars().take(1000).collect();
                    found.push(Hit { path: WirePath::from_path(entry.path()), line: n as u32 + 1, text });
                }
            }
            if !found.is_empty() {
                let mut hits = hits.lock();
                hits.extend(found);
                if hits.len() >= limit {
                    full.store(true, Ordering::Relaxed);
                    return WalkState::Quit;
                }
            }
            WalkState::Continue
        })
    });

    let mut hits = hits.into_inner();
    hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    let truncated = hits.len() >= limit;
    hits.truncate(limit);
    Ok(Found { hits, truncated })
}

// ── Worktrees ────────────────────────────────────────────────────────────────

enum WtMsg {
    Fs(notify::Result<notify::Event>),
    Stop,
}

/// One watched root, owned by its own thread.
///
/// A thread rather than a task because everything it does is blocking `std::fs`
/// and it lives as long as the root is open; the scan and the event loop share
/// its state without a lock because nothing else touches it.
struct Worktree {
    wt: u64,
    /// Canonical — FSEvents reports `/private/var/…` for a path opened as
    /// `/var/…`, and an event that does not strip against the root is dropped.
    root: PathBuf,
    out: Out,
    limit: usize,
    /// Every known entry, keyed by its path relative to the root. A `BTreeMap`
    /// so a subtree is a contiguous range (`a/b/` up to `a/b0`).
    entries: BTreeMap<Vec<u8>, Entry>,
    /// `.gitignore` + `.ignore` per directory that has either, keyed like
    /// `entries` (`""` is the root).
    ignores: HashMap<Vec<u8>, Gitignore>,
    /// Ignore files above the root, up to the repository root — deepest first.
    above: Vec<Gitignore>,
    /// `core.excludesFile` and `.git/info/exclude`.
    global: Vec<Gitignore>,
    watcher: Option<notify::RecommendedWatcher>,
    /// Linux watches each scanned directory on its own (inotify is per
    /// directory, and a recursive watch would descend into every
    /// `node_modules`); elsewhere one recursive watch on the root.
    per_dir: bool,
    watch_limited: bool,
}

impl Worktree {
    fn new(wt: u64, root: PathBuf, out: Out, limit: usize) -> Self {
        let mut global = Vec::new();
        let (excludes, _) = Gitignore::global();
        global.push(excludes);
        let mut above = Vec::new();
        if let Some(repo) = repo_root(&root) {
            let mut b = GitignoreBuilder::new(&repo);
            b.add(repo.join(".git").join("info").join("exclude"));
            if let Ok(g) = b.build() {
                global.push(g);
            }
            let mut dir = root.parent().map(Path::to_path_buf);
            while let Some(d) = dir {
                if !d.starts_with(&repo) {
                    break;
                }
                if let Some(g) = ignore_file_for(&d) {
                    above.push(g);
                }
                dir = d.parent().map(Path::to_path_buf);
            }
        }
        Self {
            wt,
            root,
            out,
            limit,
            entries: BTreeMap::new(),
            ignores: HashMap::new(),
            above,
            global,
            watcher: None,
            per_dir: cfg!(target_os = "linux"),
            watch_limited: false,
        }
    }

    fn abs(&self, rel: &[u8]) -> PathBuf {
        if rel.is_empty() { self.root.clone() } else { self.root.join(WirePath(rel.to_vec()).to_path_buf()) }
    }

    fn rel_of(&self, path: &Path) -> Option<Vec<u8>> {
        let rest = path.strip_prefix(&self.root).ok()?;
        let mut out = Vec::new();
        for (i, part) in rest.components().enumerate() {
            if i > 0 {
                out.push(b'/');
            }
            out.extend_from_slice(WirePath::from_path(Path::new(part.as_os_str())).as_bytes());
        }
        Some(out)
    }

    fn run(mut self, watch_tx: std_mpsc::Sender<WtMsg>, rx: std_mpsc::Receiver<WtMsg>) {
        let handler = move |res: notify::Result<notify::Event>| {
            let _ = watch_tx.send(WtMsg::Fs(res));
        };
        match notify::recommended_watcher(handler) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(_) => self.watch_limited = true,
        }

        if self.per_dir {
            // inotify: each directory is watched as the scan reaches it, so
            // nothing created mid-scan is missed and nothing needs a second look.
            self.initial_scan();
        } else {
            // **Scan first, watch second, then catch up.** Starting an FSEvents
            // stream was measured at 1.6 s on macOS — the snapshot must not wait
            // behind it. The gap between the two is closed by one resync, a
            // diff against the disk that is empty unless something changed.
            self.initial_scan();
            let root = self.root.clone();
            let ok = self
                .watcher
                .as_mut()
                .is_some_and(|w| w.watch(&root, notify::RecursiveMode::Recursive).is_ok());
            if !ok {
                self.watch_limited = true;
            }
            let mut changes = Vec::new();
            self.resync(&[], &mut changes);
            if !changes.is_empty() {
                send(&self.out, &ServerMsg::Changes { wt: self.wt, changes }, Vec::new());
            }
        }

        let mut dirty: Vec<PathBuf> = Vec::new();
        let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut rescan = false;
        loop {
            let first = match rx.recv() {
                Ok(WtMsg::Stop) | Err(_) => return,
                Ok(WtMsg::Fs(res)) => res,
            };
            collect(first, &mut dirty, &mut renames, &mut rescan);
            let started = Instant::now();
            // Coalesce the burst: a `git checkout` is thousands of events and
            // must arrive as one patch, not thousands.
            loop {
                let left = MAX_HOLD.saturating_sub(started.elapsed());
                if left.is_zero() {
                    break;
                }
                match rx.recv_timeout(DEBOUNCE.min(left)) {
                    Ok(WtMsg::Fs(res)) => collect(res, &mut dirty, &mut renames, &mut rescan),
                    Ok(WtMsg::Stop) | Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(std_mpsc::RecvTimeoutError::Timeout) => break,
                }
            }
            self.apply(std::mem::take(&mut dirty), std::mem::take(&mut renames), std::mem::take(&mut rescan));
        }
    }

    /// Is `abs` (whose parent directory is `parent_rel`) ignored?
    ///
    /// Deepest rule wins, as in git: the directory's own ignore files first,
    /// then each ancestor's, then the files above the root, then the global
    /// excludes. A whitelist (`!pattern`) stops the search as "not ignored".
    fn is_ignored(&self, parent_rel: &[u8], abs: &Path, is_dir: bool) -> bool {
        let mut dir = Some(parent_rel.to_vec());
        while let Some(d) = dir {
            if let Some(g) = self.ignores.get(&d) {
                let m = g.matched(abs, is_dir);
                if m.is_ignore() {
                    return true;
                }
                if m.is_whitelist() {
                    return false;
                }
            }
            dir = if d.is_empty() { None } else { Some(parent_of(&d).to_vec()) };
        }
        for g in self.above.iter().chain(self.global.iter()) {
            let m = g.matched(abs, is_dir);
            if m.is_ignore() {
                return true;
            }
            if m.is_whitelist() {
                return false;
            }
        }
        false
    }

    fn load_ignores(&mut self, rel: &[u8]) {
        match ignore_file_for(&self.abs(rel)) {
            Some(g) => {
                self.ignores.insert(rel.to_vec(), g);
            }
            None => {
                self.ignores.remove(rel);
            }
        }
    }

    fn watch_dir(&mut self, rel: &[u8]) {
        if !self.per_dir {
            return;
        }
        let abs = self.abs(rel);
        if let Some(w) = self.watcher.as_mut()
            && w.watch(&abs, notify::RecursiveMode::NonRecursive).is_err() {
                self.watch_limited = true;
            }
    }

    fn entry_for(&self, parent_rel: &[u8], name_rel: Vec<u8>, abs: &Path, meta: &fs::Metadata) -> Entry {
        let kind = kind_of(meta.file_type());
        let ignored = self.is_ignored(parent_rel, abs, kind == Kind::Dir);
        Entry {
            path: WirePath(name_rel),
            kind,
            size: meta.len(),
            mtime: mtime_ns(meta),
            mode: mode_of(meta),
            ignored,
            lazy: kind == Kind::Dir && ignored,
        }
    }

    /// Read one directory's children into entries. Returns the subdirectories
    /// to descend into. `.git` is skipped as an entry but watched for the git
    /// hint.
    fn read_children(&mut self, rel: &[u8], emit: &mut Vec<Entry>) -> Vec<Vec<u8>> {
        self.load_ignores(rel);
        self.watch_dir(rel);
        let abs_dir = self.abs(rel);
        let Ok(read) = fs::read_dir(&abs_dir) else { return Vec::new() };
        let mut subdirs = Vec::new();
        for child in read.flatten() {
            let name = child.file_name();
            if name == ".git" {
                if self.per_dir
                    && let Some(w) = self.watcher.as_mut() {
                        let _ = w.watch(&child.path(), notify::RecursiveMode::NonRecursive);
                    }
                continue;
            }
            let abs = child.path();
            let Ok(meta) = fs::symlink_metadata(&abs) else { continue };
            let mut child_rel = rel.to_vec();
            if !child_rel.is_empty() {
                child_rel.push(b'/');
            }
            child_rel.extend_from_slice(WirePath::from_path(Path::new(&name)).as_bytes());
            let entry = self.entry_for(rel, child_rel.clone(), &abs, &meta);
            if entry.kind == Kind::Dir && !entry.lazy {
                subdirs.push(child_rel.clone());
            }
            self.entries.insert(child_rel, entry.clone());
            emit.push(entry);
        }
        subdirs
    }

    fn initial_scan(&mut self) {
        let mut queue: VecDeque<Vec<u8>> = VecDeque::from([Vec::new()]);
        let mut chunk = Vec::new();
        let mut unscanned = Vec::new();
        let mut truncated = false;
        while let Some(dir) = queue.pop_front() {
            if self.entries.len() >= self.limit {
                truncated = true;
                unscanned.push(dir);
                unscanned.extend(queue.drain(..));
                break;
            }
            let subdirs = self.read_children(&dir, &mut chunk);
            queue.extend(subdirs);
            if chunk.len() >= CHUNK {
                send(&self.out, &ServerMsg::Entries { wt: self.wt, entries: std::mem::take(&mut chunk) }, Vec::new());
            }
        }
        for dir in &unscanned {
            if let Some(e) = self.entries.get_mut(dir) {
                e.lazy = true;
            }
        }
        if !chunk.is_empty() {
            send(&self.out, &ServerMsg::Entries { wt: self.wt, entries: chunk }, Vec::new());
        }
        send(
            &self.out,
            &ServerMsg::Scanned {
                wt: self.wt,
                count: self.entries.len() as u64,
                truncated,
                unscanned: unscanned.into_iter().map(WirePath).collect(),
                watch_limited: self.watch_limited,
            },
            Vec::new(),
        );
    }

    /// Scan a directory that appeared after the snapshot, emitting upserts.
    fn scan_subtree(&mut self, rel: &[u8], changes: &mut Vec<Change>) {
        let mut queue = VecDeque::from([rel.to_vec()]);
        while let Some(dir) = queue.pop_front() {
            let mut found = Vec::new();
            let subdirs = self.read_children(&dir, &mut found);
            changes.extend(found.into_iter().map(|entry| Change::Upsert { entry }));
            queue.extend(subdirs);
        }
    }

    fn remove_subtree(&mut self, rel: &[u8]) {
        self.entries.remove(rel);
        let (lo, hi) = subtree_bounds(rel);
        let doomed: Vec<Vec<u8>> = self.entries.range(lo..hi).map(|(k, _)| k.clone()).collect();
        for key in doomed {
            self.entries.remove(&key);
            self.ignores.remove(&key);
        }
        self.ignores.remove(rel);
    }

    fn move_subtree(&mut self, from: &[u8], to: &[u8]) {
        let (lo, hi) = subtree_bounds(from);
        let mut moved: Vec<(Vec<u8>, Entry)> = self.entries.range(lo..hi).map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Some(e) = self.entries.get(from) {
            moved.push((from.to_vec(), e.clone()));
        }
        for (key, _) in &moved {
            self.entries.remove(key);
        }
        for (key, mut entry) in moved {
            let mut new_key = to.to_vec();
            new_key.extend_from_slice(&key[from.len()..]);
            entry.path = WirePath(new_key.clone());
            self.entries.insert(new_key, entry);
        }
    }

    /// Is `rel` below a directory whose children are not tracked?
    fn under_lazy(&self, rel: &[u8]) -> bool {
        let mut cur = rel;
        while !cur.is_empty() {
            cur = parent_of(cur);
            if cur.is_empty() {
                return false;
            }
            if self.entries.get(cur).is_some_and(|e| e.lazy || e.kind != Kind::Dir) {
                return true;
            }
            if !self.entries.contains_key(cur) {
                // An ancestor we do not know yet — its own event (or the parent
                // rescan below) will bring it in with everything under it.
                return true;
            }
        }
        false
    }

    fn apply(&mut self, dirty: Vec<PathBuf>, renames: Vec<(PathBuf, PathBuf)>, rescan: bool) {
        let mut changes = Vec::new();
        let mut git = false;

        if rescan {
            // The kernel dropped events (FSEvents `MustScanSubDirs`, an inotify
            // queue overflow). The only honest answer is to look again.
            self.resync(&[], &mut changes);
        }

        for (from, to) in renames {
            let (Some(from_rel), Some(to_rel)) = (self.rel_of(&from), self.rel_of(&to)) else { continue };
            if from_rel.is_empty() || to_rel.is_empty() || is_git(&from_rel) || is_git(&to_rel) {
                continue;
            }
            if self.entries.contains_key(&from_rel) && !self.entries.contains_key(&to_rel) && !self.under_lazy(&to_rel) {
                self.move_subtree(&from_rel, &to_rel);
                // inotify watches follow the inode, but the watcher's own
                // path for each one is now stale — re-adding them under the
                // new paths is what keeps later events correctly named.
                let (lo, hi) = subtree_bounds(&to_rel);
                let mut dirs: Vec<Vec<u8>> = self
                    .entries
                    .range(lo..hi)
                    .filter(|(_, e)| e.kind == Kind::Dir && !e.lazy)
                    .map(|(k, _)| k.clone())
                    .collect();
                if self.entries.get(&to_rel).is_some_and(|e| e.kind == Kind::Dir && !e.lazy) {
                    dirs.push(to_rel.clone());
                }
                for dir in dirs {
                    self.watch_dir(&dir);
                }
                changes.push(Change::Rename { from: WirePath(from_rel), to: WirePath(to_rel.clone()) });
                self.refresh(&to_rel, &mut changes);
            }
        }

        let mut seen = HashSet::new();
        let mut rels: Vec<Vec<u8>> = dirty.iter().filter_map(|p| self.rel_of(p)).collect();
        rels.sort();
        for rel in rels {
            if !seen.insert(rel.clone()) {
                continue;
            }
            if is_git(&rel) {
                git = true;
                continue;
            }
            if rel.is_empty() {
                continue;
            }
            // An event whose parent is not known: refresh the nearest known
            // ancestor's child instead, which scans the whole new branch.
            let mut target = rel;
            loop {
                let parent = parent_of(&target);
                if parent.is_empty() || self.entries.get(parent).is_some_and(|e| e.kind == Kind::Dir && !e.lazy) {
                    break;
                }
                if self.entries.get(parent).is_some_and(|e| e.lazy) {
                    target.clear();
                    break;
                }
                target = parent.to_vec();
            }
            if target.is_empty() {
                continue;
            }
            if !self.under_lazy(&target) {
                self.refresh(&target, &mut changes);
            }
        }

        if !changes.is_empty() {
            for batch in changes.chunks(CHUNK) {
                send(&self.out, &ServerMsg::Changes { wt: self.wt, changes: batch.to_vec() }, Vec::new());
            }
        }
        if git {
            send(&self.out, &ServerMsg::GitChanged { wt: self.wt }, Vec::new());
        }
    }

    /// Bring one path in line with the disk.
    fn refresh(&mut self, rel: &[u8], changes: &mut Vec<Change>) {
        let abs = self.abs(rel);
        let old = self.entries.get(rel).cloned();
        let meta = match fs::symlink_metadata(&abs) {
            Ok(meta) => meta,
            Err(_) => {
                if old.is_some() {
                    self.remove_subtree(rel);
                    changes.push(Change::Remove { path: WirePath(rel.to_vec()) });
                }
                return;
            }
        };
        let parent = parent_of(rel).to_vec();
        let entry = self.entry_for(&parent, rel.to_vec(), &abs, &meta);
        let was_dir = old.as_ref().is_some_and(|e| e.kind == Kind::Dir);
        if was_dir && entry.kind != Kind::Dir {
            self.remove_subtree(rel);
        }
        let ignore_changed = old.as_ref().is_some_and(|e| e.ignored != entry.ignored);
        if old.as_ref() != Some(&entry) {
            let mut entry = entry.clone();
            // Keep a directory's laziness unless its ignore state changed.
            if let Some(o) = &old
                && o.kind == Kind::Dir && entry.kind == Kind::Dir && !ignore_changed {
                    entry.lazy = o.lazy;
                }
            self.entries.insert(rel.to_vec(), entry.clone());
            changes.push(Change::Upsert { entry: entry.clone() });
            if entry.kind == Kind::Dir && !entry.lazy && (!was_dir || ignore_changed) {
                self.scan_subtree(rel, changes);
            }
            if entry.kind == Kind::Dir && entry.lazy && was_dir && ignore_changed {
                // Newly ignored: drop what we were tracking under it.
                let keep = entry.clone();
                self.remove_subtree(rel);
                self.entries.insert(rel.to_vec(), keep);
            }
        }
        // An ignore file changed: its directory's children may have flipped.
        // Only when it *did* change — `resync` refreshes every child, this file
        // included, so reacting to an unchanged one recursed without end.
        let name = rel.rsplit(|b| *b == b'/').next().unwrap_or(rel);
        let changed = old.as_ref() != self.entries.get(rel);
        if changed && (name == b".gitignore" || name == b".ignore") {
            self.load_ignores(&parent);
            self.resync(&parent, changes);
        }
    }

    /// Re-read a directory subtree and emit the difference.
    fn resync(&mut self, rel: &[u8], changes: &mut Vec<Change>) {
        self.load_ignores(rel);
        let abs_dir = self.abs(rel);
        let Ok(read) = fs::read_dir(&abs_dir) else { return };
        let mut present = HashSet::new();
        for child in read.flatten() {
            let name = child.file_name();
            if name == ".git" {
                continue;
            }
            let mut child_rel = rel.to_vec();
            if !child_rel.is_empty() {
                child_rel.push(b'/');
            }
            child_rel.extend_from_slice(WirePath::from_path(Path::new(&name)).as_bytes());
            present.insert(child_rel.clone());
            self.refresh(&child_rel, changes);
            if self.entries.get(&child_rel).is_some_and(|e| e.kind == Kind::Dir && !e.lazy) {
                self.resync(&child_rel, changes);
            }
        }
        let (lo, hi) = subtree_bounds(rel);
        let direct: Vec<Vec<u8>> = self
            .entries
            .range(lo..hi)
            .map(|(k, _)| k.clone())
            .filter(|k| parent_of(k) == rel)
            .collect();
        for key in direct {
            if !present.contains(&key) {
                self.remove_subtree(&key);
                changes.push(Change::Remove { path: WirePath(key) });
            }
        }
    }
}

fn collect(
    res: notify::Result<notify::Event>,
    dirty: &mut Vec<PathBuf>,
    renames: &mut Vec<(PathBuf, PathBuf)>,
    rescan: &mut bool,
) {
    use notify::event::{EventKind, ModifyKind, RenameMode};
    let Ok(event) = res else {
        *rescan = true;
        return;
    };
    if event.need_rescan() {
        *rescan = true;
    }
    if let EventKind::Modify(ModifyKind::Name(RenameMode::Both)) = event.kind
        && let [from, to] = event.paths.as_slice() {
            renames.push((from.clone(), to.clone()));
        }
    dirty.extend(event.paths);
}

fn is_git(rel: &[u8]) -> bool {
    rel == b".git" || rel.starts_with(b".git/")
}

fn parent_of(rel: &[u8]) -> &[u8] {
    match rel.iter().rposition(|b| *b == b'/') {
        Some(i) => &rel[..i],
        None => &[],
    }
}

/// `[rel/, rel0)` — every key strictly inside `rel` (`'0'` is `'/' + 1`).
fn subtree_bounds(rel: &[u8]) -> (Vec<u8>, Vec<u8>) {
    if rel.is_empty() {
        return (Vec::new(), vec![0xFF; 1]);
    }
    let mut lo = rel.to_vec();
    lo.push(b'/');
    let mut hi = rel.to_vec();
    hi.push(b'/' + 1);
    (lo, hi)
}

fn repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// `.gitignore` then `.ignore` in `dir` — later rules win, so `.ignore` (the
/// ripgrep convention) can override git's.
fn ignore_file_for(dir: &Path) -> Option<Gitignore> {
    let git = dir.join(".gitignore");
    let local = dir.join(".ignore");
    let (has_git, has_local) = (git.is_file(), local.is_file());
    if !has_git && !has_local {
        return None;
    }
    let mut b = GitignoreBuilder::new(dir);
    if has_git {
        b.add(git);
    }
    if has_local {
        b.add(local);
    }
    b.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rmux-serve-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn saving_copies_over_the_original_so_mode_and_hard_links_survive() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = temp("save");
        let secret = dir.join("secret.env");
        fs::write(&secret, "A=1").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("hardlink.env");
        fs::hard_link(&secret, &link).unwrap();
        let inode = fs::metadata(&secret).unwrap().ino();

        let stat = write(&secret, None, b"A=2").unwrap();
        assert_eq!(fs::metadata(&secret).unwrap().permissions().mode() & 0o777, 0o600, "a 0600 secret stays 0600");
        assert_eq!(fs::metadata(&secret).unwrap().ino(), inode, "same inode — never renamed onto");
        assert_eq!(fs::read(&link).unwrap(), b"A=2", "the hard link sees the save");
        assert_eq!(stat.size, 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_mtime_is_a_conflict_and_writes_nothing() {
        let dir = temp("conflict");
        let file = dir.join("f");
        fs::write(&file, "disk").unwrap();
        let mtime = mtime_ns(&fs::metadata(&file).unwrap());
        let err = write(&file, Some(mtime - 1), b"mine").unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert_eq!(err.current, Some(mtime));
        assert_eq!(fs::read(&file).unwrap(), b"disk");
        write(&file, Some(mtime), b"mine").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"mine");

        fs::remove_file(&file).unwrap();
        assert_eq!(write(&file, Some(mtime), b"x").unwrap_err().code, ErrorCode::Conflict, "deleted on disk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_and_rename_refuse_to_clobber() {
        let dir = temp("clobber");
        let a = dir.join("a");
        let b = dir.join("b");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        assert_eq!(create_new(&a).unwrap_err().code, ErrorCode::Exists);
        assert_eq!(fs::read(&a).unwrap(), b"a", "create never truncates");
        assert_eq!(rename_noclobber(&a, &b).unwrap_err().code, ErrorCode::Exists);
        assert_eq!(fs::read(&b).unwrap(), b"b", "rename never replaces");
        assert!(a.exists());

        let c = dir.join("c");
        rename_noclobber(&a, &c).unwrap();
        assert!(!a.exists() && c.exists());

        // Directories too.
        let d1 = dir.join("d1");
        let d2 = dir.join("d2");
        fs::create_dir(&d1).unwrap();
        fs::create_dir(&d2).unwrap();
        assert_eq!(rename_noclobber(&d1, &d2).unwrap_err().code, ErrorCode::Exists);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_dir_refuses_an_existing_one() {
        let state = State::default();
        let dir = temp("mkdir");
        let path = WirePath::from_path(&dir.join("new"));
        handle(&state, None, Request::CreateDir { path: path.clone() }, Vec::new()).unwrap();
        let err = handle(&state, None, Request::CreateDir { path }, Vec::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Exists);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_non_utf8_name_round_trips_through_list_and_read() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = temp("bytes");
        let name = std::ffi::OsStr::from_bytes(b"caf\xe9 \n\tx.txt");
        fs::write(dir.join(name), "bytes").unwrap();
        let listed = list_dir(&dir).unwrap();
        let entry = listed.iter().find(|e| e.name.as_bytes() == name.as_bytes()).expect("listed exactly");
        let json = serde_json::to_string(&entry.name).unwrap();
        let back: WirePath = serde_json::from_str(&json).unwrap();
        let (_, payload) = read(&dir.join(back.to_path_buf()), 1024).unwrap();
        assert_eq!(payload, b"bytes");
        let _ = fs::remove_dir_all(&dir);
    }

    /// macOS (APFS) refuses non-UTF-8 names outright, so the byte-exact claim is
    /// proven on Linux above; here the escaping path must at least not corrupt a
    /// name with every awkward-but-legal character.
    #[test]
    fn an_awkward_name_survives_list_and_read() {
        let dir = temp("awkward");
        let name = "a b\n\tc %PATH% & 'q' \"d\" é.txt";
        fs::write(dir.join(name), "ok").unwrap();
        let listed = list_dir(&dir).unwrap();
        let entry = listed.iter().find(|e| e.name.as_bytes() == name.as_bytes()).expect("listed");
        let (_, payload) = read(&dir.join(entry.name.to_path_buf()), 1024).unwrap();
        assert_eq!(payload, b"ok");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_are_bounded_and_flag_binaries() {
        let dir = temp("read");
        let big = dir.join("big");
        fs::write(&big, vec![b'x'; 100]).unwrap();
        assert_eq!(read(&big, 10).unwrap_err().code, ErrorCode::TooLarge);
        let bin = dir.join("bin");
        fs::write(&bin, [1u8, 0, 2]).unwrap();
        let (meta, _) = read(&bin, 10).unwrap();
        assert_eq!(meta["binary"], true);
        assert_eq!(read(&dir, 10).unwrap_err().code, ErrorCode::IsDir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn deleting_a_symlink_never_follows_it() {
        let dir = temp("symlink");
        let target = dir.join("keep");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("precious"), "x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        delete(&link, true).unwrap();
        assert!(target.join("precious").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_upload_is_exclusive_and_a_short_one_is_removed() {
        let state = State::default();
        let dir = temp("upload");
        let path = WirePath::from_path(&dir.join("up.bin"));
        let begin = |p: &WirePath| handle(&state, None, Request::UploadBegin { path: p.clone() }, Vec::new());
        let (v, _) = begin(&path).unwrap();
        let upload = v["upload"].as_u64().unwrap();
        assert_eq!(begin(&path).unwrap_err().code, ErrorCode::Exists, "noclobber, atomically");
        handle(&state, None, Request::UploadChunk { upload }, vec![0, 0xFF, 1]).unwrap();
        handle(&state, None, Request::UploadChunk { upload }, vec![2]).unwrap();
        handle(&state, None, Request::UploadFinish { upload, size: 4 }, Vec::new()).unwrap();
        assert_eq!(fs::read(dir.join("up.bin")).unwrap(), vec![0, 0xFF, 1, 2]);

        let short = WirePath::from_path(&dir.join("short.bin"));
        let (v, _) = begin(&short).unwrap();
        let upload = v["upload"].as_u64().unwrap();
        handle(&state, None, Request::UploadChunk { upload }, vec![1, 2]).unwrap();
        assert!(handle(&state, None, Request::UploadFinish { upload, size: 10 }, Vec::new()).is_err());
        assert!(!dir.join("short.bin").exists(), "a short upload never looks complete");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_honours_ignore_rules_and_skips_binaries() {
        let dir = temp("search");
        fs::write(dir.join(".gitignore"), "skipme/\n").unwrap();
        fs::create_dir_all(dir.join("skipme")).unwrap();
        fs::write(dir.join("skipme/a.txt"), "needle").unwrap();
        fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        fs::write(dir.join("node_modules/x/a.js"), "needle").unwrap();
        fs::write(dir.join("hit.txt"), "hay\nNeedle here\n").unwrap();
        fs::write(dir.join("bin.dat"), b"needle\0").unwrap();
        let found = search(&dir, &wire::Search { text: "needle".into(), ..Default::default() }).unwrap();
        assert_eq!(found.hits.len(), 1, "{:?}", found.hits);
        assert_eq!(found.hits[0].line, 2);
        assert!(!found.truncated);

        let cased = search(&dir, &wire::Search { text: "needle".into(), case_sensitive: true, ..Default::default() }).unwrap();
        assert!(cased.hits.is_empty());
        // A literal search treats `.` literally.
        let literal = search(&dir, &wire::Search { text: "h.re".into(), ..Default::default() }).unwrap();
        assert!(literal.hits.is_empty());
        let regex = search(&dir, &wire::Search { text: "h.re".into(), regex: true, ..Default::default() }).unwrap();
        assert_eq!(regex.hits.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_reports_truncation_at_the_ceiling() {
        let dir = temp("searchcap");
        let body: String = (0..(SEARCH_LIMIT + 20)).map(|i| format!("match {i}\n")).collect();
        fs::write(dir.join("many.txt"), body).unwrap();
        let found = search(&dir, &wire::Search { text: "match".into(), ..Default::default() }).unwrap();
        assert_eq!(found.hits.len(), SEARCH_LIMIT);
        assert!(found.truncated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_leading_tilde_means_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(resolve(&WirePath::text("~")), home);
        assert_eq!(resolve(&WirePath::text("~/x/y")), home.join("x/y"));
        assert_eq!(resolve(&WirePath::text("/abs/~")), PathBuf::from("/abs/~"));
    }

    /// Not a gate — a number. How long a snapshot of this repository takes
    /// (`target/` and `node_modules/` are ignored, so they cost one entry each).
    #[tokio::test]
    async fn benchmark_snapshot_of_this_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let (out, mut rx) = mpsc::unbounded_channel();
        let started = Instant::now();
        let mut wt = Worktree::new(1, root.clone(), out, DEFAULT_LIMIT);
        wt.initial_scan();
        let elapsed = started.elapsed();
        let count = wt.entries.len();
        drop(wt);
        let mut frames = 0;
        while rx.try_recv().is_ok() {
            frames += 1;
        }
        eprintln!("snapshot of {}: {} entries in {:?} ({frames} frames)", root.display(), count, elapsed);
    }
}
