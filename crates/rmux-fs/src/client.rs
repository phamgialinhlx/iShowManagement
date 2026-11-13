//! The client half of [`crate::wire`]: one connection to an `fs-serve`.
//!
//! Requests are multiplexed by id over the single pipe, so a slow search and a
//! file read share the connection without queueing behind each other. Anything
//! the server sends unprompted (snapshot chunks, watcher changes) goes to the
//! `on_event` callback, in arrival order.
//!
//! When the pipe ends — the ssh connection dropped, the laptop slept — every
//! request still in flight fails with an error rather than hanging, and
//! [`Conn::closed`] resolves. Reconnecting is the owner's job, not this one's:
//! what to reopen afterwards is state only the owner has.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};

use crate::wire::{self, Envelope, ErrorCode, Frame, Request, ServerMsg, WireError};

type Pending = oneshot::Sender<Result<(serde_json::Value, Vec<u8>), WireError>>;

/// A live connection to one `fs-serve`.
pub struct Conn {
    out: mpsc::UnboundedSender<Frame>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    next: AtomicU64,
    closed: Arc<AtomicBool>,
    closed_rx: watch::Receiver<bool>,
    /// The server's build, from its hello.
    pub version: String,
}

/// Why a connection could not be established.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The pipe closed before the server said hello — most often an older
    /// agent that does not know `fs-serve` and printed a usage error instead.
    #[error("the server ended before it said hello")]
    NoHello,
    #[error("the server did not say hello within {0:?}")]
    Timeout(Duration),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl Conn {
    /// Handshake over an established pipe and start reading.
    pub async fn start<R, W, F>(
        mut reader: R,
        mut writer: W,
        hello_timeout: Duration,
        on_event: F,
    ) -> Result<Arc<Self>, StartError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
        F: Fn(ServerMsg) + Send + Sync + 'static,
    {
        let hello = tokio::time::timeout(hello_timeout, wire::read_frame(&mut reader))
            .await
            .map_err(|_| StartError::Timeout(hello_timeout))??
            .ok_or(StartError::NoHello)?;
        let version = match serde_json::from_value::<ServerMsg>(hello.header) {
            Ok(ServerMsg::Hello { version, .. }) => version,
            _ => return Err(StartError::NoHello),
        };

        let (out, mut rx) = mpsc::unbounded_channel::<Frame>();
        let closed = Arc::new(AtomicBool::new(false));
        let (closed_tx, closed_rx) = watch::channel(false);
        let pending: Arc<Mutex<HashMap<u64, Pending>>> = Arc::default();

        let writer_closed = Arc::clone(&closed);
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if wire::write_frame(&mut writer, &frame).await.is_err() {
                    writer_closed.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });

        let reader_pending = Arc::clone(&pending);
        let reader_closed = Arc::clone(&closed);
        tokio::spawn(async move {
            loop {
                let frame = match wire::read_frame(&mut reader).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => break,
                };
                let Ok(msg) = serde_json::from_value::<ServerMsg>(frame.header) else { continue };
                match msg {
                    ServerMsg::Ok { id, result } => {
                        if let Some(tx) = reader_pending.lock().remove(&id) {
                            let _ = tx.send(Ok((result, frame.payload)));
                        }
                    }
                    ServerMsg::Err { id, error } => {
                        if let Some(tx) = reader_pending.lock().remove(&id) {
                            let _ = tx.send(Err(error));
                        }
                    }
                    ServerMsg::Hello { .. } => {}
                    event => on_event(event),
                }
            }
            reader_closed.store(true, Ordering::Relaxed);
            // Nothing in flight may hang on a connection that is gone.
            for (_, tx) in reader_pending.lock().drain() {
                let _ = tx.send(Err(WireError::new(ErrorCode::Io, "the connection to the file server closed")));
            }
            let _ = closed_tx.send(true);
        });

        Ok(Arc::new(Self { out, pending, next: AtomicU64::new(1), closed, closed_rx, version }))
    }

    /// A server running inside this process, over an in-memory pipe.
    ///
    /// The local machine's equivalent of `ssh host -- rmux-agent fs-serve`:
    /// the same server code and the same protocol, so local and remote cannot
    /// drift apart — only the pipe differs.
    pub async fn in_process<F>(on_event: F) -> Result<Arc<Self>, StartError>
    where
        F: Fn(ServerMsg) + Send + Sync + 'static,
    {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (server_read, server_write) = tokio::io::split(server);
        tokio::spawn(async move {
            let _ = crate::serve::serve(server_read, server_write).await;
        });
        let (client_read, client_write) = tokio::io::split(client);
        Self::start(client_read, client_write, Duration::from_secs(5), on_event).await
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Resolves once the connection has ended.
    pub async fn closed(&self) {
        let mut rx = self.closed_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Send a request and wait for its reply: the JSON result and the payload.
    pub async fn call(
        &self,
        request: Request,
        payload: Vec<u8>,
    ) -> Result<(serde_json::Value, Vec<u8>), WireError> {
        if self.is_closed() {
            return Err(WireError::new(ErrorCode::Io, "the connection to the file server is closed"));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        let header = serde_json::to_value(Envelope { id, request })
            .map_err(|e| WireError::new(ErrorCode::Invalid, e.to_string()))?;
        if self.out.send(Frame::with_payload(header, payload)).is_err() {
            self.pending.lock().remove(&id);
            return Err(WireError::new(ErrorCode::Io, "the connection to the file server is closed"));
        }
        rx.await.unwrap_or_else(|_| Err(WireError::new(ErrorCode::Io, "the file server went away")))
    }

    /// [`Conn::call`], with the result deserialised.
    pub async fn call_as<T: DeserializeOwned>(&self, request: Request) -> Result<T, WireError> {
        let (value, _) = self.call(request, Vec::new()).await?;
        serde_json::from_value(value).map_err(|e| WireError::new(ErrorCode::Invalid, e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Kind, Opened, ReadReply, Stat, WirePath};
    use std::sync::mpsc as std_mpsc;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rmux-fsclient-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wp(p: &std::path::Path) -> WirePath {
        WirePath::from_path(p)
    }

    #[tokio::test]
    async fn read_write_and_conflict_through_the_protocol() {
        let dir = temp("rw");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"one").unwrap();
        let conn = Conn::in_process(|_| {}).await.unwrap();

        let (meta, bytes) = conn.call(Request::Read { path: wp(&file), max: None }, Vec::new()).await.unwrap();
        let meta: ReadReply = serde_json::from_value(meta).unwrap();
        assert_eq!(bytes, b"one");
        assert_eq!(meta.size, 3);

        let (stat, _) = conn
            .call(Request::Write { path: wp(&file), expected_mtime: Some(meta.mtime) }, b"two".to_vec())
            .await
            .unwrap();
        let stat: Stat = serde_json::from_value(stat).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");

        // Someone else writes behind our back; the old token must be refused.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&file, b"theirs").unwrap();
        let err = conn
            .call(Request::Write { path: wp(&file), expected_mtime: Some(stat.mtime) }, b"mine".to_vec())
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert_eq!(std::fs::read(&file).unwrap(), b"theirs", "a conflict never writes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Multi-threaded: the test blocks on a std channel, which on a
    // current-thread runtime would starve the reader task it is waiting on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_worktree_snapshot_streams_and_then_watches() {
        let dir = temp("wt");
        std::fs::write(dir.join(".gitignore"), "ignored_dir/\n*.log\n").unwrap();
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(dir.join("src/deep/x.rs"), "fn main() {}").unwrap();
        std::fs::create_dir_all(dir.join("ignored_dir/inner")).unwrap();
        std::fs::write(dir.join("ignored_dir/inner/big.js"), "x").unwrap();
        std::fs::write(dir.join("noise.log"), "x").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        let (tx, rx) = std_mpsc::channel::<ServerMsg>();
        let tx = Mutex::new(tx);
        let conn = Conn::in_process(move |m| {
            let _ = tx.lock().send(m);
        })
        .await
        .unwrap();
        let opened: Opened = conn.call_as(Request::OpenWorktree { root: wp(&dir), limit: None }).await.unwrap();

        let mut entries = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("snapshot") {
                ServerMsg::Entries { wt, entries: chunk } if wt == opened.wt => entries.extend(chunk),
                ServerMsg::Scanned { truncated, .. } => {
                    assert!(!truncated);
                    break;
                }
                _ => {}
            }
        }
        let find = |p: &str| entries.iter().find(|e| e.path.as_bytes() == p.as_bytes()).cloned();
        assert!(find("src/deep/x.rs").is_some(), "{entries:?}");
        assert!(find(".git").is_none(), ".git is never in the tree");
        let ignored = find("ignored_dir").expect("an ignored dir is still listed");
        assert!(ignored.ignored && ignored.lazy);
        assert!(find("ignored_dir/inner").is_none(), "an ignored dir is never scanned");
        assert!(find("noise.log").unwrap().ignored);
        assert_eq!(find("src").unwrap().kind, Kind::Dir);

        // Created / modified / renamed / deleted, each observed from the watcher.
        let wait_for = |pred: &dyn Fn(&crate::wire::Change) -> bool, what: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match rx.recv_timeout(left) {
                    Ok(ServerMsg::Changes { changes, .. }) if changes.iter().any(pred) => return,
                    Ok(_) => {}
                    Err(_) => panic!("never saw: {what}"),
                }
            }
        };
        use crate::wire::Change;
        std::fs::write(dir.join("src/new.rs"), "a").unwrap();
        wait_for(&|c| matches!(c, Change::Upsert { entry } if entry.path.as_bytes() == b"src/new.rs"), "create");

        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("src/new.rs"), "a longer body").unwrap();
        wait_for(
            &|c| matches!(c, Change::Upsert { entry } if entry.path.as_bytes() == b"src/new.rs" && entry.size == 13),
            "modify",
        );

        std::fs::rename(dir.join("src/new.rs"), dir.join("src/renamed.rs")).unwrap();
        wait_for(
            &|c| match c {
                Change::Rename { to, .. } => to.as_bytes() == b"src/renamed.rs",
                Change::Upsert { entry } => entry.path.as_bytes() == b"src/renamed.rs",
                _ => false,
            },
            "rename",
        );

        std::fs::remove_file(dir.join("src/renamed.rs")).unwrap();
        wait_for(&|c| matches!(c, Change::Remove { path } if path.as_bytes() == b"src/renamed.rs"), "delete");

        // A new directory with content appears whole.
        std::fs::create_dir_all(dir.join("fresh/sub")).unwrap();
        std::fs::write(dir.join("fresh/sub/f.txt"), "x").unwrap();
        wait_for(&|c| matches!(c, Change::Upsert { entry } if entry.path.as_bytes() == b"fresh/sub/f.txt"), "new subtree");

        // Inside an ignored directory: nothing is reported.
        std::fs::write(dir.join("ignored_dir/inner/more.js"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(400));
        while let Ok(m) = rx.try_recv() {
            if let ServerMsg::Changes { changes, .. } = m {
                assert!(
                    !changes.iter().any(|c| matches!(c, Change::Upsert { entry } if entry.path.as_bytes().starts_with(b"ignored_dir/inner"))),
                    "{changes:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_snapshot_respects_its_cap_and_says_so() {
        let dir = temp("cap");
        for d in 0..5 {
            std::fs::create_dir_all(dir.join(format!("d{d}"))).unwrap();
            for f in 0..5 {
                std::fs::write(dir.join(format!("d{d}/f{f}")), "x").unwrap();
            }
        }
        let (tx, rx) = std_mpsc::channel::<ServerMsg>();
        let tx = Mutex::new(tx);
        let conn = Conn::in_process(move |m| {
            let _ = tx.lock().send(m);
        })
        .await
        .unwrap();
        let _: Opened = conn.call_as(Request::OpenWorktree { root: wp(&dir), limit: Some(8) }).await.unwrap();
        loop {
            if let ServerMsg::Scanned { truncated, unscanned, .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                assert!(truncated);
                assert!(!unscanned.is_empty(), "the directories it skipped are named, so they can be listed lazily");
                break;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
