//! Files follow Zed's remote model: one server per host, a local mirror of the
//! tree, and a stream of changes.
//!
//! ## The shape
//!
//! - **One `fs-serve` per server**, started over the existing ControlMaster
//!   (`ssh -T host -- <agent> fs-serve`) and kept for the life of the app. On
//!   the local machine the very same server runs in-process over a pipe.
//! - **A [`Mirror`] per (server, root)**: the whole worktree, held here and in
//!   the webview, fed first by the snapshot and then by the watcher. Expanding a
//!   folder or fuzzy-finding a file reads the mirror — no round trip.
//! - **[`AgentFs`]** implements the ordinary [`FileSystem`] trait over the same
//!   connection, so every existing `fs_*` command gets faster without changing
//!   its signature, and the file contents it reads are cached by the version
//!   the watcher last reported.
//!
//! ## Fallback is transparent
//!
//! Windows hosts have no agent, an older agent does not know `fs-serve`, and a
//! dev build may ship no agents at all. In every such case the server is
//! marked `Unsupported` and [`AgentFs`] answers through the shell-based
//! `rmux-fs` path exactly as before. While a connection is still coming up the
//! shell path answers too — a first listing must never wait on an agent upload.
//!
//! ## Reconnecting
//!
//! When the pipe ends the supervisor restarts it with backoff, and the backoff
//! resets only after a connection that **lasted** (the bridge's rule: a server
//! that dies on arrival must not be redialled every second for ever). Each
//! mirror is rebuilt off to the side and swapped in as **one diff**, so the
//! operator's tree does not empty and refill under their hands.

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use rmux_fs::client::{Conn, StartError};
use rmux_fs::wire::{
    self, Change, Entry, ErrorCode, Found, Kind, Listed, Opened, ReadReply, Request, ServerMsg, Stat,
    WireError, WirePath,
};
use rmux_fs::{DirEntry, EntryKind, FileContent, FileSystem, PreviewContent, SearchHit, SearchQuery};
use rmux_transport::{CommandSpec, NoConsoleWindow, Platform, TargetId, Tty};
use serde::Serialize;
use serde_json::json;
use tokio::sync::{Notify, watch};

use crate::ctx::Ctx;
use crate::terminal::TargetRef;

/// The event the UI listens on. Every payload carries the mirror `id`.
pub const EVENT: &str = "fs-worktree";

const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A connection that stayed up this long was healthy; only then does the
/// backoff reset.
const HEALTHY: Duration = Duration::from_secs(30);
/// How long `fs-serve` gets to say hello. Generous: over a slow link the ssh
/// session and the agent's own start-up both sit inside it.
const HELLO_TIMEOUT: Duration = Duration::from_secs(20);
/// Content cache ceiling, in bytes of text.
const CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Upload chunk — big enough that a file is few frames, small enough that the
/// pipe never holds much more than one.
const UPLOAD_CHUNK: usize = 1024 * 1024;

#[derive(Clone)]
enum ConnState {
    Connecting,
    Live(Arc<Conn>),
    Down(String),
    Unsupported(String),
}

/// One supervised `fs-serve` per target.
#[derive(Default)]
pub struct WorktreeStore {
    servers: Mutex<HashMap<TargetId, Arc<Server>>>,
    next_mirror: AtomicU64,
}

impl WorktreeStore {
    /// Stop this target's server and forget its mirrors.
    ///
    /// The supervisor is **aborted**, not dropped — like the status watcher, a
    /// merely forgotten supervisor would notice its pipe close and dial straight
    /// back out, re-opening the connection `server_disconnect` is closing.
    /// Aborting drops the child process with it (`kill_on_drop`).
    pub fn evict_target(&self, id: &TargetId) -> bool {
        match self.servers.lock().remove(id) {
            Some(server) => {
                server.shutdown();
                true
            }
            None => false,
        }
    }

    /// The server for `target`, starting its supervisor on first use.
    pub fn server(&self, ctx: &Arc<Ctx>, target: &TargetRef) -> Arc<Server> {
        let id = target.id();
        let mut servers = self.servers.lock();
        if let Some(existing) = servers.get(&id) {
            return Arc::clone(existing);
        }
        let (state, _) = watch::channel(ConnState::Connecting);
        let server = Arc::new(Server {
            state,
            supervisor: Mutex::new(None),
            routing: Mutex::new(Routing::default()),
            mirrors: Mutex::new(HashMap::new()),
            cache: Mutex::new(ContentCache::default()),
            host: ctx.host_arc(),
        });
        let task = tokio::spawn(supervise(Arc::downgrade(&server), Arc::clone(ctx), target.clone()));
        *server.supervisor.lock() = Some(task);
        servers.insert(id, Arc::clone(&server));
        server
    }

    fn mirror_id(&self) -> u64 {
        self.next_mirror.fetch_add(1, Ordering::Relaxed) + 1
    }
}

#[derive(Default)]
struct Routing {
    by_wt: HashMap<u64, Arc<Mirror>>,
    /// Events for a worktree whose `open` reply has not been routed yet — the
    /// server's scan thread can beat its own reply onto the pipe.
    orphans: HashMap<u64, Vec<ServerMsg>>,
}

pub struct Server {
    state: watch::Sender<ConnState>,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
    routing: Mutex<Routing>,
    /// Keyed by the root exactly as the UI named it.
    mirrors: Mutex<HashMap<String, Arc<Mirror>>>,
    cache: Mutex<ContentCache>,
    host: Arc<dyn crate::Host>,
}

impl Server {
    fn shutdown(&self) {
        if let Some(task) = self.supervisor.lock().take() {
            task.abort();
        }
        let _ = self.state.send(ConnState::Down("disconnected".into()));
    }

    /// The live connection, if there is one right now.
    fn live(&self) -> Option<Arc<Conn>> {
        match &*self.state.borrow() {
            ConnState::Live(conn) if !conn.is_closed() => Some(Arc::clone(conn)),
            _ => None,
        }
    }

    /// Wait for the connection to be live, up to `timeout`. `Err` names why it
    /// will not be: unsupported here, or still down.
    async fn wait_live(&self, timeout: Duration) -> Result<Arc<Conn>, String> {
        let mut rx = self.state.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            let snapshot = rx.borrow().clone();
            match snapshot {
                ConnState::Live(conn) if !conn.is_closed() => return Ok(conn),
                ConnState::Unsupported(reason) => return Err(format!("unsupported: {reason}")),
                ConnState::Down(reason) if Instant::now() >= deadline => return Err(reason),
                _ if Instant::now() >= deadline => return Err("the file server is still starting".into()),
                _ => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if tokio::time::timeout(left, rx.changed()).await.is_err() {
                continue;
            }
        }
    }

    fn unsupported(&self) -> bool {
        matches!(&*self.state.borrow(), ConnState::Unsupported(_))
    }

    fn on_event(&self, msg: ServerMsg) {
        let wt = match &msg {
            ServerMsg::Entries { wt, .. }
            | ServerMsg::Scanned { wt, .. }
            | ServerMsg::Changes { wt, .. }
            | ServerMsg::GitChanged { wt } => *wt,
            _ => return,
        };
        let mirror = {
            let mut routing = self.routing.lock();
            match routing.by_wt.get(&wt) {
                Some(m) => Arc::clone(m),
                None => {
                    // Bounded twice over: per worktree, and in how many
                    // unclaimed worktrees are remembered at all.
                    if !routing.orphans.contains_key(&wt) && routing.orphans.len() >= 16 {
                        return;
                    }
                    let queue = routing.orphans.entry(wt).or_default();
                    // Bounded: a worktree nobody claims must not grow without end.
                    if queue.len() < 100_000 {
                        queue.push(msg);
                    }
                    return;
                }
            }
        };
        mirror.apply(self.host.as_ref(), msg);
    }

    /// Ask the server to open `mirror`'s root and route its events to it.
    async fn open(&self, conn: &Conn, mirror: &Arc<Mirror>, rebuild: bool) -> Result<(), String> {
        let opened: Opened = conn
            .call_as(Request::OpenWorktree { root: WirePath(wire::ui_unescape(&mirror.root)), limit: None })
            .await
            .map_err(|e| e.message)?;
        let previous = mirror.inner.lock().wt;
        mirror.begin(opened.wt, rebuild);
        let backlog = {
            let mut routing = self.routing.lock();
            routing.by_wt.retain(|_, m| !Arc::ptr_eq(m, mirror));
            routing.by_wt.insert(opened.wt, Arc::clone(mirror));
            if let Some(old) = previous {
                routing.orphans.remove(&old);
            }
            routing.orphans.remove(&opened.wt).unwrap_or_default()
        };
        for msg in backlog {
            mirror.apply(self.host.as_ref(), msg);
        }
        // Two opens raced (the supervisor's reopen and a command's): the loser's
        // watcher would stream into nothing for ever, so it is closed.
        if let Some(old) = previous.filter(|old| *old != opened.wt) {
            let _ = conn.call(Request::CloseWorktree { wt: old }, Vec::new()).await;
        }
        Ok(())
    }

    /// The version the watcher last reported for an absolute path, when a live
    /// mirror covers it. `None` means "not known to be fresh" — never cache.
    fn version_of(&self, abs: &[u8]) -> Option<i64> {
        self.live()?;
        let mirrors = self.mirrors.lock();
        for mirror in mirrors.values() {
            let root = wire::ui_unescape(&mirror.root);
            let root = root.strip_suffix(b"/").unwrap_or(&root);
            let Some(rest) = abs.strip_prefix(root) else { continue };
            let Some(rel) = rest.strip_prefix(b"/") else { continue };
            let inner = mirror.inner.lock();
            if inner.wt.is_none() || inner.watch_limited || !inner.scanned {
                continue;
            }
            if let Some(entry) = inner.entries.get(rel) {
                return (entry.kind == Kind::File).then_some(entry.mtime);
            }
        }
        None
    }
}

/// One watched root.
pub struct Mirror {
    pub id: u64,
    /// The root as the UI named it — the prefix of every path the UI builds.
    pub root: String,
    inner: Mutex<MirrorInner>,
    scanned: Notify,
}

#[derive(Default)]
struct MirrorInner {
    wt: Option<u64>,
    entries: BTreeMap<Vec<u8>, Entry>,
    /// While reconnecting: the fresh snapshot, built off to the side and
    /// swapped in as one diff when it completes.
    rebuilding: Option<BTreeMap<Vec<u8>, Entry>>,
    scanned: bool,
    truncated: bool,
    watch_limited: bool,
    seq: u64,
}

/// One entry as the webview sees it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UiEntry {
    /// Relative to the root, `/`-separated, [`wire::ui_escape`]d.
    pub path: String,
    pub kind: &'static str,
    pub size: u64,
    /// The mtime in nanoseconds, as a string — a JavaScript number cannot hold
    /// it exactly, and this is compared, never computed with.
    pub version: String,
    pub ignored: bool,
    pub lazy: bool,
}

fn ui_entry(e: &Entry) -> UiEntry {
    UiEntry {
        path: wire::ui_escape(e.path.as_bytes()),
        kind: match e.kind {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
        },
        size: e.size,
        version: e.mtime.to_string(),
        ignored: e.ignored,
        lazy: e.lazy,
    }
}

/// What `fs_worktree_open` returns.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: u64,
    pub root: String,
    /// Patches with a `seq` at or below this are already in `entries`.
    pub seq: u64,
    pub entries: Vec<UiEntry>,
    pub scanned: bool,
    pub truncated: bool,
    pub watch_limited: bool,
}

fn remove_subtree(map: &mut BTreeMap<Vec<u8>, Entry>, rel: &[u8]) {
    map.remove(rel);
    let mut lo = rel.to_vec();
    lo.push(b'/');
    let mut hi = rel.to_vec();
    hi.push(b'/' + 1);
    let doomed: Vec<Vec<u8>> = map.range(lo..hi).map(|(k, _)| k.clone()).collect();
    for k in doomed {
        map.remove(&k);
    }
}

fn move_subtree(map: &mut BTreeMap<Vec<u8>, Entry>, from: &[u8], to: &[u8]) {
    let mut lo = from.to_vec();
    lo.push(b'/');
    let mut hi = from.to_vec();
    hi.push(b'/' + 1);
    let mut moved: Vec<(Vec<u8>, Entry)> = map.range(lo..hi).map(|(k, v)| (k.clone(), v.clone())).collect();
    if let Some(e) = map.get(from) {
        moved.push((from.to_vec(), e.clone()));
    }
    for (k, _) in &moved {
        map.remove(k);
    }
    for (k, mut e) in moved {
        let mut nk = to.to_vec();
        nk.extend_from_slice(&k[from.len()..]);
        e.path = WirePath(nk.clone());
        map.insert(nk, e);
    }
}

impl Mirror {
    fn begin(&self, wt: u64, rebuild: bool) {
        let mut inner = self.inner.lock();
        inner.wt = Some(wt);
        inner.rebuilding = rebuild.then(BTreeMap::new);
        if !rebuild {
            inner.scanned = false;
        }
    }

    fn emit(&self, host: &dyn crate::Host, inner: &mut MirrorInner, mut body: serde_json::Value) {
        inner.seq += 1;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("id".into(), json!(self.id));
            obj.insert("seq".into(), json!(inner.seq));
        }
        host.emit(EVENT, body);
    }

    fn apply(&self, host: &dyn crate::Host, msg: ServerMsg) {
        let mut inner = self.inner.lock();
        match msg {
            ServerMsg::Entries { entries, .. } => {
                if let Some(fresh) = inner.rebuilding.as_mut() {
                    for e in entries {
                        fresh.insert(e.path.0.clone(), e);
                    }
                    return;
                }
                let upserts: Vec<UiEntry> = entries.iter().map(ui_entry).collect();
                for e in entries {
                    inner.entries.insert(e.path.0.clone(), e);
                }
                self.emit(host, &mut inner, json!({ "kind": "patch", "upserts": upserts }));
            }
            ServerMsg::Changes { changes, .. } => {
                if let Some(fresh) = inner.rebuilding.as_mut() {
                    for change in changes {
                        apply_change(fresh, change);
                    }
                    return;
                }
                let mut upserts = Vec::new();
                let mut removes = Vec::new();
                let mut renames = Vec::new();
                for change in changes {
                    match &change {
                        Change::Upsert { entry } => upserts.push(ui_entry(entry)),
                        Change::Remove { path } => removes.push(wire::ui_escape(path.as_bytes())),
                        Change::Rename { from, to } => renames.push(json!({
                            "from": wire::ui_escape(from.as_bytes()),
                            "to": wire::ui_escape(to.as_bytes()),
                        })),
                    }
                    apply_change(&mut inner.entries, change);
                }
                self.emit(
                    host,
                    &mut inner,
                    json!({ "kind": "patch", "upserts": upserts, "removes": removes, "renames": renames }),
                );
            }
            ServerMsg::Scanned { truncated, unscanned, watch_limited, .. } => {
                inner.truncated = truncated;
                inner.watch_limited = watch_limited;
                let mut lazy = Vec::new();
                {
                    let MirrorInner { rebuilding, entries, .. } = &mut *inner;
                    let map = rebuilding.as_mut().unwrap_or(entries);
                    for dir in unscanned {
                        if let Some(e) = map.get_mut(dir.as_bytes()) {
                            e.lazy = true;
                            lazy.push(ui_entry(e));
                        }
                    }
                }
                if let Some(fresh) = inner.rebuilding.take() {
                    // The reconnect diff: everything that changed while the
                    // connection was down, in one patch.
                    let removes: Vec<String> = inner
                        .entries
                        .keys()
                        .filter(|k| !fresh.contains_key(*k))
                        .map(|k| wire::ui_escape(k))
                        .collect();
                    let upserts: Vec<UiEntry> = fresh
                        .iter()
                        .filter(|(k, v)| inner.entries.get(*k) != Some(v))
                        .map(|(_, v)| ui_entry(v))
                        .collect();
                    inner.entries = fresh;
                    self.emit(host, &mut inner, json!({ "kind": "patch", "upserts": upserts, "removes": removes }));
                } else if !lazy.is_empty() {
                    self.emit(host, &mut inner, json!({ "kind": "patch", "upserts": lazy }));
                }
                inner.scanned = true;
                let body = json!({ "kind": "scanned", "truncated": truncated, "watchLimited": watch_limited });
                self.emit(host, &mut inner, body);
                drop(inner);
                self.scanned.notify_waiters();
            }
            ServerMsg::GitChanged { .. } => {
                self.emit(host, &mut inner, json!({ "kind": "git" }));
            }
            _ => {}
        }
    }

    fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock();
        Snapshot {
            id: self.id,
            root: self.root.clone(),
            seq: inner.seq,
            entries: inner.entries.values().map(ui_entry).collect(),
            scanned: inner.scanned,
            truncated: inner.truncated,
            watch_limited: inner.watch_limited,
        }
    }
}

fn apply_change(map: &mut BTreeMap<Vec<u8>, Entry>, change: Change) {
    match change {
        Change::Upsert { entry } => {
            map.insert(entry.path.0.clone(), entry);
        }
        Change::Remove { path } => remove_subtree(map, path.as_bytes()),
        Change::Rename { from, to } => move_subtree(map, from.as_bytes(), to.as_bytes()),
    }
}

/// Byte-bounded LRU of file contents, keyed by absolute path and validated by
/// the version the watcher reports — a stale entry is never served, because
/// its version no longer matches.
#[derive(Default)]
struct ContentCache {
    map: HashMap<Vec<u8>, Cached>,
    bytes: usize,
    tick: u64,
}

struct Cached {
    version: i64,
    text: Arc<String>,
    used: u64,
}

impl ContentCache {
    fn get(&mut self, path: &[u8], version: i64) -> Option<Arc<String>> {
        self.tick += 1;
        let tick = self.tick;
        let hit = self.map.get_mut(path).filter(|c| c.version == version)?;
        hit.used = tick;
        Some(Arc::clone(&hit.text))
    }

    fn put(&mut self, path: Vec<u8>, version: i64, text: String) {
        if text.len() > CACHE_BYTES / 4 {
            return;
        }
        self.tick += 1;
        if let Some(old) = self.map.remove(&path) {
            self.bytes -= old.text.len();
        }
        self.bytes += text.len();
        self.map.insert(path, Cached { version, text: Arc::new(text), used: self.tick });
        while self.bytes > CACHE_BYTES {
            let Some(oldest) = self.map.iter().min_by_key(|(_, c)| c.used).map(|(k, _)| k.clone()) else { break };
            if let Some(gone) = self.map.remove(&oldest) {
                self.bytes -= gone.text.len();
            }
        }
    }

    fn forget(&mut self, path: &[u8]) {
        if let Some(old) = self.map.remove(path) {
            self.bytes -= old.text.len();
        }
    }
}

enum ConnectError {
    /// Never going to work here — fall back for the life of this server.
    Unsupported(String),
    /// Might work next time.
    Failed(String),
}

/// What keeps a spawned `ssh … fs-serve` alive: dropping it kills the child.
enum Keepalive {
    None,
    Child(#[allow(dead_code)] tokio::process::Child),
}

async fn connect(
    server: &Weak<Server>,
    ctx: &Arc<Ctx>,
    target: &TargetRef,
) -> Result<(Arc<Conn>, Keepalive), ConnectError> {
    let events = {
        let server = server.clone();
        move |msg: ServerMsg| {
            if let Some(server) = server.upgrade() {
                server.on_event(msg);
            }
        }
    };

    // The local machine runs the server in-process; a host runs it under its
    // agent. The protocol, the server code and everything above this point are
    // identical — this is the one place the two differ, and it is the pipe.
    if let TargetId::Local = target.id() {
        let conn = Conn::in_process(events).await.map_err(|e| ConnectError::Failed(e.to_string()))?;
        return Ok((conn, Keepalive::None));
    }

    let claude = ctx.store::<crate::claude::ClaudeStore>();
    let resolved = crate::claude::resolve(&claude, target).await.map_err(ConnectError::Failed)?;
    if resolved.platform() == Some(Platform::Windows) {
        return Err(ConnectError::Unsupported("Windows hosts are served by the shell path".into()));
    }
    let installed = crate::agent::ensure_agent(ctx, resolved.as_ref())
        .await
        .map_err(ConnectError::Unsupported)?;

    let spec = CommandSpec::new(&installed.program).arg("fs-serve").tty(Tty::None);
    let command = resolved.build_command(&spec).map_err(|e| ConnectError::Failed(e.to_string()))?;
    let mut cmd = tokio::process::Command::new(&command.program);
    cmd.args(&command.args);
    for (key, value) in &command.env {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.no_console_window();
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| ConnectError::Failed(e.to_string()))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(ConnectError::Failed("could not open the file server's pipes".into()));
    };

    // Keep the tail of stderr: it is how an older agent says it does not know
    // the subcommand, and how ssh says why it failed.
    let tail = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&tail);
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt as _;
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut t = sink.lock();
            if t.len() < 4096 {
                t.push_str(&line);
                t.push('\n');
            }
        }
    });

    match Conn::start(stdout, stdin, HELLO_TIMEOUT, events).await {
        Ok(conn) => Ok((conn, Keepalive::Child(child))),
        Err(StartError::NoHello) => {
            // Give stderr a moment to arrive before reading it.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let said = tail.lock().clone();
            if said.contains("unknown command") || said.contains("usage") {
                Err(ConnectError::Unsupported("this host's agent predates fs-serve".into()))
            } else {
                Err(ConnectError::Failed(format!("the file server did not start: {}", said.trim())))
            }
        }
        Err(e) => Err(ConnectError::Failed(e.to_string())),
    }
}

async fn supervise(server: Weak<Server>, ctx: Arc<Ctx>, target: TargetRef) {
    let mut backoff = BACKOFF_START;
    let mut first = true;
    loop {
        let Some(strong) = server.upgrade() else { return };
        let _ = strong.state.send(ConnState::Connecting);
        drop(strong);

        match connect(&server, &ctx, &target).await {
            Err(ConnectError::Unsupported(reason)) => {
                if let Some(s) = server.upgrade() {
                    tracing::info!("fs-serve unavailable for {}: {reason}", target.id().label());
                    let _ = s.state.send(ConnState::Unsupported(reason));
                }
                return;
            }
            Err(ConnectError::Failed(reason)) => {
                if let Some(s) = server.upgrade() {
                    let _ = s.state.send(ConnState::Down(reason));
                }
            }
            Ok((conn, keepalive)) => {
                let started = Instant::now();
                let Some(s) = server.upgrade() else { return };
                let _ = s.state.send(ConnState::Live(Arc::clone(&conn)));
                // Reopen every mirror on the new connection — rebuilt to the
                // side, so the tree swaps in as one diff rather than emptying.
                let mirrors: Vec<Arc<Mirror>> = s.mirrors.lock().values().cloned().collect();
                s.routing.lock().by_wt.clear();
                for mirror in mirrors {
                    let _ = s.open(&conn, &mirror, !first).await;
                }
                first = false;
                drop(s);
                conn.closed().await;
                drop(keepalive);
                if let Some(s) = server.upgrade() {
                    let _ = s.state.send(ConnState::Down("the file server connection closed".into()));
                    for mirror in s.mirrors.lock().values() {
                        let mut inner = mirror.inner.lock();
                        inner.wt = None;
                        mirror.emit(s.host.as_ref(), &mut inner, json!({ "kind": "down" }));
                    }
                }
                if started.elapsed() >= HEALTHY {
                    backoff = BACKOFF_START;
                }
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Open (or join) the mirror of `root` on `target`.
pub async fn open(ctx: &Arc<Ctx>, target: TargetRef, root: String) -> Result<Snapshot, String> {
    let store = ctx.store::<WorktreeStore>();
    let server = store.server(ctx, &target);
    let conn = server.wait_live(Duration::from_secs(30)).await?;
    let root = if root.len() > 1 { root.trim_end_matches('/').to_owned() } else { root };

    let (mirror, fresh) = {
        let mut mirrors = server.mirrors.lock();
        match mirrors.get(&root) {
            Some(m) => (Arc::clone(m), false),
            None => {
                let m = Arc::new(Mirror {
                    id: store.mirror_id(),
                    root: root.clone(),
                    inner: Mutex::new(MirrorInner::default()),
                    scanned: Notify::new(),
                });
                mirrors.insert(root.clone(), Arc::clone(&m));
                (m, true)
            }
        }
    };
    let (unopened, populated) = {
        let inner = mirror.inner.lock();
        (fresh || inner.wt.is_none(), !inner.entries.is_empty())
    };
    // A mirror that already holds a tree is rebuilt to the side and diffed,
    // exactly as on reconnect — never appended to, which would keep entries
    // that are gone.
    if unopened && let Err(e) = server.open(&conn, &mirror, populated).await {
        server.mirrors.lock().remove(&root);
        return Err(e);
    }
    // Wait briefly for the scan so a small project arrives as one snapshot
    // rather than a burst of patches. A large one keeps streaming.
    if !mirror.inner.lock().scanned {
        let wait = mirror.scanned.notified();
        let _ = tokio::time::timeout(Duration::from_millis(1500), wait).await;
    }
    Ok(mirror.snapshot())
}

// ── AgentFs ──────────────────────────────────────────────────────────────────

/// [`FileSystem`] over `fs-serve`, with the shell path behind it.
pub struct AgentFs {
    server: Arc<Server>,
    fallback: Arc<dyn FileSystem>,
    /// Whether a call has already waited for the connection to come up. Only
    /// the first one does: while a host is still being provisioned every later
    /// call goes straight to the shell path rather than paying the wait again.
    waited: std::sync::atomic::AtomicBool,
}

impl AgentFs {
    pub fn new(server: Arc<Server>, fallback: Arc<dyn FileSystem>) -> Self {
        Self { server, fallback, waited: std::sync::atomic::AtomicBool::new(false) }
    }

    /// The connection to use for this call, or `None` for the shell path.
    ///
    /// A connection still coming up gets a moment — but only a moment: the
    /// first listing on a fresh host must not sit behind an agent upload.
    async fn conn(&self) -> Option<Arc<Conn>> {
        if self.server.unsupported() {
            return None;
        }
        if let Some(conn) = self.server.live() {
            return Some(conn);
        }
        if self.waited.swap(true, Ordering::Relaxed) {
            return None;
        }
        self.server.wait_live(Duration::from_millis(1200)).await.ok()
    }
}

fn path_of(p: &str) -> WirePath {
    WirePath(wire::ui_unescape(p))
}

fn map_err(path: &str, e: WireError) -> anyhow::Error {
    match e.code {
        ErrorCode::Exists => anyhow::anyhow!("{path} already exists"),
        ErrorCode::Conflict => anyhow::anyhow!("{CONFLICT_PREFIX}{}", e.message),
        _ => anyhow::anyhow!(e.message),
    }
}

/// How a conflicting save is told apart from any other failure, across IPC.
pub const CONFLICT_PREFIX: &str = "conflict: ";

/// A dropped connection mid-call: a read can be retried on the shell path.
fn connection_lost(e: &WireError) -> bool {
    e.code == ErrorCode::Io && e.message.contains("file server")
}

fn entry_kind(kind: Kind) -> EntryKind {
    match kind {
        Kind::File => EntryKind::File,
        Kind::Dir => EntryKind::Directory,
        Kind::Symlink => EntryKind::Symlink,
    }
}

impl AgentFs {
    /// Read with the version token. Served from the cache when the watcher says
    /// the cached copy is current.
    pub async fn read_versioned(&self, path: &str) -> anyhow::Result<(FileContent, Option<String>)> {
        let Some(conn) = self.conn().await else {
            return Ok((self.fallback.read_file(path).await?, None));
        };
        let wire_path = path_of(path);
        if let Some(version) = self.server.version_of(wire_path.as_bytes())
            && let Some(text) = self.server.cache.lock().get(wire_path.as_bytes(), version)
        {
            return Ok((FileContent::Text { text: (*text).clone() }, Some(version.to_string())));
        }
        let result = conn
            .call(Request::Read { path: wire_path.clone(), max: Some(rmux_fs::MAX_READ_BYTES) }, Vec::new())
            .await;
        let (meta, bytes) = match result {
            Ok(ok) => ok,
            Err(e) if e.code == ErrorCode::TooLarge => {
                let bytes = e.current.and_then(|c| u64::try_from(c).ok()).unwrap_or(0);
                return Ok((FileContent::TooLarge { bytes }, None));
            }
            Err(e) if connection_lost(&e) => return Ok((self.fallback.read_file(path).await?, None)),
            Err(e) => return Err(map_err(path, e)),
        };
        let meta: ReadReply = serde_json::from_value(meta)?;
        // Length-framed, and checked: a short read is an error, never a file.
        anyhow::ensure!(
            bytes.len() as u64 == meta.size,
            "{path}: short read ({} of {} bytes)",
            bytes.len(),
            meta.size
        );
        let content = rmux_fs::classify_content(bytes);
        if let FileContent::Text { text } = &content {
            self.server.cache.lock().put(wire_path.0.clone(), meta.mtime, text.clone());
        }
        Ok((content, Some(meta.mtime.to_string())))
    }

    /// Save, refusing if the file changed since `expected` was read.
    pub async fn write_versioned(
        &self,
        path: &str,
        contents: &str,
        expected: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let Some(conn) = self.conn().await else {
            self.fallback.write_file(path, contents).await?;
            return Ok(None);
        };
        let expected_mtime = expected.and_then(|v| v.parse::<i64>().ok());
        let wire_path = path_of(path);
        self.server.cache.lock().forget(wire_path.as_bytes());
        let (stat, _) = conn
            .call(Request::Write { path: wire_path.clone(), expected_mtime }, contents.as_bytes().to_vec())
            .await
            .map_err(|e| map_err(path, e))?;
        let stat: Stat = serde_json::from_value(stat)?;
        self.server.cache.lock().put(wire_path.0, stat.mtime, contents.to_owned());
        Ok(Some(stat.mtime.to_string()))
    }

    async fn upload_stream<R: tokio::io::AsyncRead + Unpin>(
        &self,
        conn: &Conn,
        path: &str,
        mut source: R,
        sent: &rmux_fs::Transfer,
    ) -> anyhow::Result<()> {
        use tokio::io::AsyncReadExt as _;
        let (begin, _) = conn
            .call(Request::UploadBegin { path: path_of(path) }, Vec::new())
            .await
            .map_err(|e| map_err(path, e))?;
        let upload = begin["upload"].as_u64().ok_or_else(|| anyhow::anyhow!("malformed upload reply"))?;
        let mut total = 0u64;
        let result: anyhow::Result<()> = async {
            let mut buf = vec![0u8; UPLOAD_CHUNK];
            loop {
                let n = source.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                conn.call(Request::UploadChunk { upload }, buf[..n].to_vec()).await.map_err(|e| map_err(path, e))?;
                total += n as u64;
                sent.set_done(total);
            }
            conn.call(Request::UploadFinish { upload, size: total }, Vec::new()).await.map_err(|e| map_err(path, e))?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = conn.call(Request::UploadAbort { upload }, Vec::new()).await;
        }
        result
    }
}

/// A reader that fails once its transfer is cancelled.
struct Cancellable<'a, R> {
    inner: R,
    progress: &'a rmux_fs::Transfer,
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Cancellable<'_, R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.progress.is_cancelled() {
            return std::task::Poll::Ready(Err(std::io::Error::other(rmux_fs::CANCELLED)));
        }
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

#[async_trait]
impl FileSystem for AgentFs {
    async fn list_dir(&self, path: &str) -> anyhow::Result<Vec<DirEntry>> {
        let Some(conn) = self.conn().await else { return self.fallback.list_dir(path).await };
        match conn.call_as::<Vec<Listed>>(Request::ListDir { path: path_of(path) }).await {
            Ok(listed) => {
                let mut entries: Vec<DirEntry> = listed
                    .into_iter()
                    .map(|l| DirEntry { name: wire::ui_escape(l.name.as_bytes()), kind: entry_kind(l.kind) })
                    .collect();
                rmux_fs::protocol::sort_entries(&mut entries);
                Ok(entries)
            }
            Err(e) if connection_lost(&e) => self.fallback.list_dir(path).await,
            Err(e) => Err(map_err(path, e)),
        }
    }

    async fn read_file(&self, path: &str) -> anyhow::Result<FileContent> {
        Ok(self.read_versioned(path).await?.0)
    }

    async fn write_file(&self, path: &str, contents: &str) -> anyhow::Result<()> {
        self.write_versioned(path, contents, None).await.map(|_| ())
    }

    async fn create_dir(&self, path: &str) -> anyhow::Result<()> {
        let Some(conn) = self.conn().await else { return self.fallback.create_dir(path).await };
        conn.call(Request::CreateDir { path: path_of(path) }, Vec::new()).await.map_err(|e| map_err(path, e))?;
        Ok(())
    }

    async fn create_file(&self, path: &str) -> anyhow::Result<()> {
        let Some(conn) = self.conn().await else { return self.fallback.create_file(path).await };
        conn.call(Request::CreateFile { path: path_of(path) }, Vec::new()).await.map_err(|e| map_err(path, e))?;
        Ok(())
    }

    async fn upload(&self, path: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let Some(conn) = self.conn().await else { return self.fallback.upload(path, bytes).await };
        let sent = rmux_fs::Transfer::default();
        self.upload_stream(&conn, path, bytes, &sent).await
    }

    async fn upload_file(
        &self,
        path: &str,
        local: &std::path::Path,
        progress: &rmux_fs::Transfer,
    ) -> anyhow::Result<()> {
        let Some(conn) = self.conn().await else { return self.fallback.upload_file(path, local, progress).await };
        let file = tokio::fs::File::open(local).await?;
        progress.set_total(file.metadata().await?.len());
        // A cancel surfaces as a read error, so `upload_stream` takes its own
        // failure path and sends `UploadAbort` — the server removes the partial
        // file. Dropping the future instead would leave the upload open on the
        // server until the connection closed.
        let source = Cancellable { inner: file, progress };
        self.upload_stream(&conn, path, source, progress).await
    }

    async fn download(&self, path: &str, dest: &std::path::Path, progress: &rmux_fs::Transfer) -> anyhow::Result<u64> {
        // Streaming a download over the framed pipe is a later slice; the
        // shell path already chunks it.
        self.fallback.download(path, dest, progress).await
    }

    async fn rename(&self, from: &str, to: &str) -> anyhow::Result<()> {
        let Some(conn) = self.conn().await else { return self.fallback.rename(from, to).await };
        conn.call(Request::Rename { from: path_of(from), to: path_of(to) }, Vec::new())
            .await
            .map_err(|e| map_err(to, e))?;
        Ok(())
    }

    async fn delete(&self, path: &str) -> anyhow::Result<()> {
        let Some(conn) = self.conn().await else { return self.fallback.delete(path).await };
        conn.call(Request::Delete { path: path_of(path), recursive: true }, Vec::new())
            .await
            .map_err(|e| map_err(path, e))?;
        Ok(())
    }

    async fn read_preview(&self, path: &str) -> anyhow::Result<PreviewContent> {
        self.fallback.read_preview(path).await
    }

    async fn home_dir(&self) -> anyhow::Result<String> {
        if let Some(conn) = self.conn().await
            && let Ok(home) = conn.call_as::<WirePath>(Request::Home).await
        {
            return Ok(wire::ui_escape(home.as_bytes()));
        }
        self.fallback.home_dir().await
    }

    async fn search(&self, root: &str, query: &SearchQuery) -> anyhow::Result<Vec<SearchHit>> {
        let Some(conn) = self.conn().await else { return self.fallback.search(root, query).await };
        let request = Request::Search {
            root: path_of(root),
            query: wire::Search {
                text: query.text.clone(),
                case_sensitive: query.case_sensitive,
                regex: query.regex,
                globs: Vec::new(),
                limit: None,
            },
        };
        match conn.call_as::<Found>(request).await {
            Ok(found) => Ok(found
                .hits
                .into_iter()
                .map(|h| SearchHit { path: wire::ui_escape(h.path.as_bytes()), line: h.line, text: h.text })
                .collect()),
            Err(e) if connection_lost(&e) => self.fallback.search(root, query).await,
            Err(e) => Err(map_err(root, e)),
        }
    }

    async fn read_versioned(&self, path: &str) -> anyhow::Result<(FileContent, Option<String>)> {
        AgentFs::read_versioned(self, path).await
    }

    async fn write_versioned(&self, path: &str, contents: &str, expected: Option<&str>) -> anyhow::Result<Option<String>> {
        AgentFs::write_versioned(self, path, contents, expected).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_is_bounded_and_validated_by_version() {
        let mut cache = ContentCache::default();
        cache.put(b"/a".to_vec(), 1, "one".into());
        assert_eq!(cache.get(b"/a", 1).as_deref().map(String::as_str), Some("one"));
        assert!(cache.get(b"/a", 2).is_none(), "a newer version on disk is never served from cache");
        let big = "x".repeat(CACHE_BYTES / 5);
        for i in 0..10 {
            cache.put(format!("/f{i}").into_bytes(), 1, big.clone());
        }
        assert!(cache.bytes <= CACHE_BYTES);
        cache.forget(b"/f9");
        assert!(cache.get(b"/f9", 1).is_none());
    }

    #[test]
    fn subtree_moves_and_removals_are_exact() {
        let e = |p: &str| Entry {
            path: WirePath::text(p),
            kind: Kind::File,
            size: 0,
            mtime: 0,
            mode: 0,
            ignored: false,
            lazy: false,
        };
        let mut map = BTreeMap::new();
        for p in ["a", "a/b", "a/b/c", "a-b", "a0", "ab"] {
            map.insert(p.as_bytes().to_vec(), e(p));
        }
        move_subtree(&mut map, b"a", b"z");
        let keys: Vec<String> = map.keys().map(|k| String::from_utf8(k.clone()).unwrap()).collect();
        assert_eq!(keys, ["a-b", "a0", "ab", "z", "z/b", "z/b/c"]);
        assert_eq!(map[b"z/b/c".as_slice()].path, WirePath::text("z/b/c"));
        remove_subtree(&mut map, b"z");
        let keys: Vec<String> = map.keys().map(|k| String::from_utf8(k.clone()).unwrap()).collect();
        assert_eq!(keys, ["a-b", "a0", "ab"], "siblings sharing a prefix are untouched");
    }
}
