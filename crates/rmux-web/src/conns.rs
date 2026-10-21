//! Live WebSockets, and the channels bound to them.
//!
//! One socket per page carries every stream (`docs/rmux-web.md` › "Wire
//! protocol"). A command that streams resolves the `"__CHANNEL__:<n>"` it was
//! given against the socket the page named in `X-Rmux-Conn`, and from then on
//! its output becomes frames on that socket.
//!
//! **Every queue is bounded, and overflowing it drops the connection.** A phone
//! on a bad link reads slower than a build log scrolls; an unbounded queue turns
//! that into the server's memory growing until something kills it. Dropping the
//! socket instead is cheap to recover from — the page reconnects and its panes
//! reattach, exactly as after a network blip — and a stream task writing to a
//! dropped connection gets an error and stops, rather than buffering for a
//! reader that is never coming back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use axum::body::Bytes;
use axum::extract::ws::Message;
use parking_lot::Mutex;
use rmux_app::{Sink, SinkResolver};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio_util::sync::CancellationToken;

/// The prefix Tauri's `Channel.toJSON()` produces, which `bridge.ts` copies.
pub const CHANNEL_PREFIX: &str = "__CHANNEL__:";

/// Frames a connection may have queued before it is judged too slow and
/// dropped. A terminal chunk is at most tens of KiB, so this bounds a stalled
/// socket at a few MiB rather than at whatever the shell produces.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// Every live connection, by id.
pub struct Conns {
    map: Mutex<HashMap<String, Arc<Conn>>>,
    capacity: usize,
}

impl Conns {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "a zero-length queue drops every connection");
        Self { map: Mutex::new(HashMap::new()), capacity }
    }

    /// Register a new connection. The receiver is the socket's outbound queue.
    pub fn open(&self) -> (Arc<Conn>, mpsc::Receiver<Message>) {
        let (tx, rx) = mpsc::channel(self.capacity);
        let conn = Arc::new(Conn {
            id: random_id(),
            tx,
            closed: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            channels: Mutex::new(HashMap::new()),
        });
        self.map.lock().insert(conn.id.clone(), conn.clone());
        (conn, rx)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Conn>> {
        self.map.lock().get(id).cloned()
    }

    pub fn remove(&self, id: &str) {
        if let Some(conn) = self.map.lock().remove(id) {
            conn.shut();
        }
    }

    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Send one text frame to every connection (`Host::emit`). A connection that
    /// cannot take it is dropped, for the same reason a slow stream is.
    pub fn broadcast(&self, text: &str) {
        let conns: Vec<Arc<Conn>> = self.map.lock().values().cloned().collect();
        for conn in conns {
            let _ = conn.push(Message::Text(text.into()));
        }
    }
}

/// One page's WebSocket, seen from the producer side.
pub struct Conn {
    id: String,
    tx: mpsc::Sender<Message>,
    closed: AtomicBool,
    cancel: CancellationToken,
    /// Live sinks by channel id. Weak, so a sink's lifetime is its users', and
    /// resolving one id twice while the first is alive yields the same sink —
    /// otherwise the first to drop would end a channel the second still uses.
    channels: Mutex<HashMap<u32, Weak<ChannelSink>>>,
}

impl Conn {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Fires when the connection is dropped, by either side.
    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.cancel.cancelled()
    }

    /// Mark the connection dead and tell its socket task to close it.
    pub fn shut(&self) {
        self.closed.store(true, Ordering::Release);
        self.cancel.cancel();
    }

    /// Queue a frame. A full queue drops the connection: see the module docs.
    pub fn push(&self, message: Message) -> anyhow::Result<()> {
        if self.is_closed() {
            anyhow::bail!("the page's connection {} is gone", self.id);
        }
        match self.tx.try_send(message) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                tracing::warn!(conn = %self.id, "client is not keeping up; dropping the connection");
                self.shut();
                anyhow::bail!("the page's connection {} fell behind and was dropped", self.id)
            }
            Err(TrySendError::Closed(_)) => {
                self.shut();
                anyhow::bail!("the page's connection {} is gone", self.id)
            }
        }
    }

    /// The sink for `channel` on this connection.
    pub fn channel(self: &Arc<Self>, channel: u32) -> Arc<ChannelSink> {
        let mut channels = self.channels.lock();
        if let Some(live) = channels.get(&channel).and_then(Weak::upgrade) {
            return live;
        }
        let sink = Arc::new(ChannelSink { conn: self.clone(), channel });
        channels.insert(channel, Arc::downgrade(&sink));
        sink
    }
}

/// A stream to one channel on one page.
pub struct ChannelSink {
    conn: Arc<Conn>,
    channel: u32,
}

impl Sink for ChannelSink {
    /// Binary frame: 4-byte big-endian channel id, then the bytes, unencoded.
    fn send_bytes(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        let mut frame = Vec::with_capacity(4 + bytes.len());
        frame.extend_from_slice(&self.channel.to_be_bytes());
        frame.extend_from_slice(&bytes);
        self.conn.push(Message::Binary(Bytes::from(frame)))
    }

    /// Text frame `{"channel": n, "data": value}`.
    fn send_json(&self, value: serde_json::Value) -> anyhow::Result<()> {
        let text = serde_json::json!({ "channel": self.channel, "data": value }).to_string();
        self.conn.push(Message::Text(text.into()))
    }
}

impl Drop for ChannelSink {
    /// Tell the page the channel is finished (`{"channel": n, "end": true}`), so
    /// it can forget the handler. Best effort: a full or gone connection is not
    /// worth dropping over a goodbye.
    fn drop(&mut self) {
        {
            let mut channels = self.conn.channels.lock();
            if channels.get(&self.channel).is_some_and(|w| w.strong_count() == 0) {
                channels.remove(&self.channel);
            }
        }
        if !self.conn.is_closed() {
            let text = serde_json::json!({ "channel": self.channel, "end": true }).to_string();
            let _ = self.conn.tx.try_send(Message::Text(text.into()));
        }
    }
}

/// Resolves channel ids for one invoke, against the conn its `X-Rmux-Conn`
/// header named.
pub struct ConnResolver {
    pub conns: Arc<Conns>,
    pub conn: Option<String>,
}

/// `"__CHANNEL__:<n>"` → `n`.
pub fn parse_channel_id(channel_id: &str) -> anyhow::Result<u32> {
    channel_id
        .strip_prefix(CHANNEL_PREFIX)
        .and_then(|n| n.parse::<u32>().ok())
        .ok_or_else(|| anyhow::anyhow!("`{channel_id}` is not a channel id"))
}

impl SinkResolver for ConnResolver {
    fn resolve(&self, channel_id: &str) -> anyhow::Result<Arc<dyn Sink>> {
        let channel = parse_channel_id(channel_id)?;
        let Some(id) = self.conn.as_deref() else {
            anyhow::bail!("this call streams to a channel, but named no connection (X-Rmux-Conn)");
        };
        let conn = self
            .conns
            .get(id)
            .filter(|c| !c.is_closed())
            .ok_or_else(|| anyhow::anyhow!("the page's connection {id} is gone"))?;
        Ok(conn.channel(channel))
    }
}

/// A connection id: 128 random bits, hex. Not a credential — every request is
/// authenticated on its own — but unguessable so one page cannot address
/// another's streams by counting.
fn random_id() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_ids_parse_only_in_the_tauri_shape() {
        assert_eq!(parse_channel_id("__CHANNEL__:7").unwrap(), 7);
        assert_eq!(parse_channel_id("__CHANNEL__:4294967295").unwrap(), u32::MAX);
        for bad in ["7", "__CHANNEL__:", "__CHANNEL__:-1", "__CHANNEL__:4294967296", "__channel__:7"] {
            assert!(parse_channel_id(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_slow_reader_is_dropped_not_buffered() {
        let conns = Conns::new(2);
        let (conn, _rx) = conns.open(); // never drained: the phone that stopped reading
        let sink = conn.channel(1);
        sink.send_bytes(b"a".to_vec()).unwrap();
        sink.send_bytes(b"b".to_vec()).unwrap();
        let err = sink.send_bytes(b"c".to_vec()).unwrap_err();
        assert!(err.to_string().contains("fell behind"), "{err}");
        assert!(conn.is_closed());
        // And it stays dropped: the stream task's next write fails too.
        assert!(sink.send_json(serde_json::json!(1)).is_err());
    }

    #[tokio::test]
    async fn a_gone_connection_fails_sends() {
        let conns = Arc::new(Conns::new(8));
        let (conn, rx) = conns.open();
        let resolver = ConnResolver { conns: conns.clone(), conn: Some(conn.id().to_owned()) };
        let sink = resolver.resolve("__CHANNEL__:3").unwrap();
        drop(rx);
        assert!(sink.send_bytes(vec![1]).is_err());
        conns.remove(conn.id());
        assert!(resolver.resolve("__CHANNEL__:3").is_err());
    }

    #[tokio::test]
    async fn resolving_a_channel_without_a_conn_is_an_error() {
        let conns = Arc::new(Conns::new(8));
        let resolver = ConnResolver { conns, conn: None };
        let err = resolver.resolve("__CHANNEL__:1").err().unwrap();
        assert!(err.to_string().contains("X-Rmux-Conn"), "{err}");
    }

    #[tokio::test]
    async fn one_channel_resolved_twice_is_one_sink() {
        let conns = Conns::new(8);
        let (conn, mut rx) = conns.open();
        let a = conn.channel(5);
        let b = conn.channel(5);
        assert!(Arc::ptr_eq(&a, &b));
        drop(a);
        assert!(rx.try_recv().is_err(), "ended while still in use");
        drop(b);
        match rx.try_recv().unwrap() {
            Message::Text(t) => assert_eq!(t.as_str(), r#"{"channel":5,"end":true}"#),
            other => panic!("{other:?}"),
        }
    }
}
