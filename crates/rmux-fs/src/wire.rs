//! The `fs-serve` wire format — what crosses the one long-lived pipe between
//! rmux and the filesystem server on a host.
//!
//! ## Framing
//!
//! ```text
//! u32 BE header length | u32 BE payload length | header (JSON) | payload (raw bytes)
//! ```
//!
//! The header is JSON because it is small, self-describing and survives a field
//! being added on one side only. The payload is **raw bytes**, never base64 in
//! the JSON: a file is the hot path, and base64 inflates it by a third and costs
//! an encode and a decode on each side for nothing.
//!
//! **A short read is an error, never a short file.** Both lengths are read
//! before anything else, and [`read_frame`] uses `read_exact` for the body, so a
//! connection that drops mid-transfer surfaces as `UnexpectedEof` rather than as
//! a frame holding the first half of a file — which the editor would then show,
//! and save back, as if it were the whole thing. A clean EOF is only accepted
//! *between* frames.
//!
//! ## Paths are bytes
//!
//! A Unix filename is any bytes but `/` and NUL, and need not be UTF-8. A path in
//! a header is a [`WirePath`]: a JSON string when it is valid UTF-8 (the
//! overwhelmingly common case, and readable in a log), otherwise
//! `{"b64": "…"}`. Either way it round-trips **exactly** — the same reason
//! `rmux-fs` uses NUL records: a format that can only carry most filenames
//! corrupts the rest, silently.
//!
//! The webview cannot hold raw bytes in a path string, so [`ui_escape`] and
//! [`ui_unescape`] give it a lossless string form for the rare non-UTF-8 name.

use std::io;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The protocol revision `fs-serve` announces in its hello. Bumped on any
/// change a peer could misread; additive fields do not need it.
pub const PROTOCOL: u32 = 1;

/// Largest header accepted. Generous — a snapshot chunk of a few thousand
/// entries is well under a megabyte — but bounded, so a corrupted length cannot
/// make the reader allocate gigabytes.
pub const MAX_HEADER: u32 = 32 * 1024 * 1024;

/// Largest payload accepted in one frame. Reads are capped far below this by
/// policy ([`crate::MAX_READ_BYTES`]); uploads are chunked. This is only the
/// corruption guard.
pub const MAX_PAYLOAD: u32 = 512 * 1024 * 1024;

/// One frame: a JSON header and an optional raw payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub header: serde_json::Value,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(header: serde_json::Value) -> Self {
        Self { header, payload: Vec::new() }
    }

    pub fn with_payload(header: serde_json::Value, payload: Vec<u8>) -> Self {
        Self { header, payload }
    }
}

/// Encode one frame into bytes.
pub fn encode(frame: &Frame) -> io::Result<Vec<u8>> {
    let header = serde_json::to_vec(&frame.header).map_err(io::Error::other)?;
    let header_len = u32::try_from(header.len()).map_err(|_| io::Error::other("header too large"))?;
    let payload_len =
        u32::try_from(frame.payload.len()).map_err(|_| io::Error::other("payload too large"))?;
    if header_len > MAX_HEADER || payload_len > MAX_PAYLOAD {
        return Err(io::Error::other("frame exceeds the protocol limits"));
    }
    let mut out = Vec::with_capacity(8 + header.len() + frame.payload.len());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&frame.payload);
    Ok(out)
}

/// Write one frame and flush it.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> io::Result<()> {
    w.write_all(&encode(frame)?).await?;
    w.flush().await
}

/// Read one frame.
///
/// `Ok(None)` is a clean end of stream *between* frames. EOF anywhere inside a
/// frame is `UnexpectedEof` — see the module note on short reads.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut lens = [0u8; 8];
    // The first byte decides between "clean end" and "torn frame".
    let first = r.read(&mut lens[..1]).await?;
    if first == 0 {
        return Ok(None);
    }
    r.read_exact(&mut lens[1..]).await?;
    let header_len = u32::from_be_bytes(lens[..4].try_into().expect("4 bytes"));
    let payload_len = u32::from_be_bytes(lens[4..].try_into().expect("4 bytes"));
    if header_len > MAX_HEADER || payload_len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame lengths out of range ({header_len}, {payload_len})"),
        ));
    }
    let mut header = vec![0u8; header_len as usize];
    r.read_exact(&mut header).await?;
    let mut payload = vec![0u8; payload_len as usize];
    r.read_exact(&mut payload).await?;
    let header = serde_json::from_slice(&header)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(Frame { header, payload }))
}

/// A path as raw bytes, carried losslessly in JSON.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct WirePath(pub Vec<u8>);

impl WirePath {
    pub fn text(s: &str) -> Self {
        Self(s.as_bytes().to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// For logs and error messages only — never for a path that is used again.
    pub fn display(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }

    #[cfg(unix)]
    pub fn to_path_buf(&self) -> std::path::PathBuf {
        use std::os::unix::ffi::OsStrExt as _;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&self.0))
    }

    /// Off Unix a path is UTF-16 underneath and every name the OS hands out is
    /// representable as a string, so the lossy form is never actually lossy for
    /// a name that came from the disk.
    #[cfg(not(unix))]
    pub fn to_path_buf(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(String::from_utf8_lossy(&self.0).into_owned())
    }

    #[cfg(unix)]
    pub fn from_path(path: &std::path::Path) -> Self {
        use std::os::unix::ffi::OsStrExt as _;
        Self(path.as_os_str().as_bytes().to_vec())
    }

    #[cfg(not(unix))]
    pub fn from_path(path: &std::path::Path) -> Self {
        Self(path.to_string_lossy().into_owned().into_bytes())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum WirePathRepr {
    Text(String),
    Bytes { b64: String },
}

impl Serialize for WirePath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        match std::str::from_utf8(&self.0) {
            Ok(text) => WirePathRepr::Text(text.to_owned()).serialize(s),
            Err(_) => WirePathRepr::Bytes {
                b64: base64::engine::general_purpose::STANDARD.encode(&self.0),
            }
            .serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for WirePath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        match WirePathRepr::deserialize(d)? {
            WirePathRepr::Text(text) => Ok(Self(text.into_bytes())),
            WirePathRepr::Bytes { b64 } => base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .map(Self)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// First code point of the escape range.
///
/// A byte that is not part of valid UTF-8 is always `0x80..=0xFF`, so 128 code
/// points cover every one. They are taken from the Private Use Area, where no
/// real filename has any business being — and the encoding stays lossless even
/// when one does, because a *literal* character from this range is itself
/// escaped as its three UTF-8 bytes (see [`ui_escape`]).
const ESCAPE_BASE: u32 = 0xF780;

fn is_escape_char(c: char) -> bool {
    (ESCAPE_BASE..ESCAPE_BASE + 0x80).contains(&(c as u32))
}

/// Raw path bytes → a string the webview can hold, losslessly.
///
/// Valid UTF-8 passes through unchanged (so every ordinary path looks like
/// itself). An invalid byte `b` becomes `U+F780 + (b - 0x80)`. A literal
/// character from that same range is written as its own three bytes, escaped —
/// which is what makes [`ui_unescape`] an exact inverse.
pub fn ui_escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    let push_byte = |out: &mut String, b: u8| {
        out.push(char::from_u32(ESCAPE_BASE + u32::from(b - 0x80)).expect("valid PUA code point"));
    };
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                push_valid(&mut out, valid, push_byte);
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                push_valid(&mut out, std::str::from_utf8(good).expect("validated"), push_byte);
                let skip = e.error_len().unwrap_or(bad.len());
                for &b in &bad[..skip] {
                    push_byte(&mut out, b);
                }
                rest = &bad[skip..];
            }
        }
    }
    out
}

fn push_valid(out: &mut String, valid: &str, push_byte: impl Fn(&mut String, u8)) {
    for c in valid.chars() {
        if is_escape_char(c) {
            let mut buf = [0u8; 4];
            for &b in c.encode_utf8(&mut buf).as_bytes() {
                push_byte(out, b);
            }
        } else {
            out.push(c);
        }
    }
}

/// The inverse of [`ui_escape`].
pub fn ui_unescape(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        if is_escape_char(c) {
            out.push((c as u32 - ESCAPE_BASE + 0x80) as u8);
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}

/// What a worktree entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

/// One entry of a worktree, relative to its root (`/`-separated).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: WirePath,
    pub kind: Kind,
    pub size: u64,
    /// Nanoseconds since the epoch. The version token for conflict checks: an
    /// `i64` is exact in JSON between two Rust peers, and it crosses to the
    /// webview as a *string* because a JavaScript number is not.
    pub mtime: i64,
    pub mode: u32,
    /// Matched by `.gitignore`, `.ignore` or the global excludes.
    pub ignored: bool,
    /// A directory whose children are **not** in the snapshot and must be listed
    /// on demand: ignored directories (`node_modules`, `target`) and anything
    /// past the entry cap.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lazy: bool,
}

/// One incremental change to a worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Change {
    /// Created or modified — the entry as it is now.
    Upsert { entry: Entry },
    /// Gone, with everything under it.
    Remove { path: WirePath },
    /// Moved, with everything under it. Followed by an `Upsert` of `to`.
    Rename { from: WirePath, to: WirePath },
}

/// Why a request failed, in a form the client can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    /// Create, rename or upload onto a name that is taken.
    Exists,
    /// `write` with an `expected_mtime` that no longer matches the disk.
    Conflict,
    TooLarge,
    IsDir,
    Permission,
    Invalid,
    Unsupported,
    Io,
}

impl ErrorCode {
    pub fn from_io(e: &io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::AlreadyExists => Self::Exists,
            io::ErrorKind::PermissionDenied => Self::Permission,
            io::ErrorKind::IsADirectory => Self::IsDir,
            io::ErrorKind::InvalidInput => Self::Invalid,
            _ => Self::Io,
        }
    }
}

/// A failed request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
    /// For `conflict`: the mtime now on disk. For `too_large`: the size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<i64>,
}

impl WireError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), current: None }
    }
}

/// What to search for, server side.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Search {
    pub text: String,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub regex: bool,
    /// Glob filters (`*.rs`, `!*.lock`). Empty means everything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// A request. **No field may be called `id`** — the envelope is flattened
/// beside the request id, and a duplicate key does not round-trip at all (the
/// bridge learnt that one; `a_request_never_shadows_the_envelope_id` pins it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    OpenWorktree {
        root: WirePath,
        #[serde(default)]
        limit: Option<usize>,
    },
    CloseWorktree {
        wt: u64,
    },
    ListDir {
        path: WirePath,
    },
    Stat {
        path: WirePath,
    },
    /// The whole file as the payload of the reply.
    Read {
        path: WirePath,
        #[serde(default)]
        max: Option<u64>,
    },
    /// The payload is the new contents. Copied over the original — never a
    /// rename onto it.
    Write {
        path: WirePath,
        #[serde(default)]
        expected_mtime: Option<i64>,
    },
    CreateFile {
        path: WirePath,
    },
    CreateDir {
        path: WirePath,
    },
    Rename {
        from: WirePath,
        to: WirePath,
    },
    Delete {
        path: WirePath,
        #[serde(default)]
        recursive: bool,
    },
    UploadBegin {
        path: WirePath,
    },
    /// The payload is the next chunk.
    UploadChunk {
        upload: u64,
    },
    UploadFinish {
        upload: u64,
        size: u64,
    },
    UploadAbort {
        upload: u64,
    },
    Search {
        root: WirePath,
        query: Search,
    },
    Home,
}

/// A request with its id, as it goes on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// Everything the server sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Sent once, first, unprompted — it is how a client tells a live server
    /// from an older agent that printed a usage error and exited.
    Hello { protocol: u32, version: String },
    /// A successful reply. `result` is typed by the request it answers; a
    /// payload, when there is one, travels in the frame body.
    Ok { id: u64, result: serde_json::Value },
    Err { id: u64, error: WireError },
    /// Part of a worktree's initial snapshot, in breadth-first order so the top
    /// of the tree arrives first.
    Entries { wt: u64, entries: Vec<Entry> },
    /// The initial scan finished. `unscanned` names directories the cap left
    /// out; they are lazy from here on.
    Scanned {
        wt: u64,
        count: u64,
        truncated: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unscanned: Vec<WirePath>,
        /// Some directories could not be watched (inotify's limit), so live
        /// updates below them will not arrive.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        watch_limited: bool,
    },
    /// Incremental changes from the watcher, debounced and coalesced.
    Changes { wt: u64, changes: Vec<Change> },
    /// Something under `.git` moved (HEAD, the index, a ref) — a cheap hint
    /// that git status is worth refreshing. `.git` itself is never in the tree.
    GitChanged { wt: u64 },
}

/// Reply to `OpenWorktree`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    pub wt: u64,
    /// The root as the server resolved it (a leading `~` expanded).
    pub root: WirePath,
}

/// Reply to `Read`; the bytes are the frame payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadReply {
    pub size: u64,
    pub mtime: i64,
    pub binary: bool,
}

/// Reply to `Write`, `Stat`, `CreateFile`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
}

/// One entry of a `ListDir` reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub name: WirePath,
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
}

/// One search hit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub path: WirePath,
    pub line: u32,
    pub text: String,
}

/// Reply to `Search`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub hits: Vec<Hit>,
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_with_binary_payloads() {
        let frame = Frame::with_payload(
            serde_json::json!({"t": "ok", "id": 7, "result": {"size": 4}}),
            vec![0, 0xFF, b'\n', 0],
        );
        let bytes = encode(&frame).unwrap();
        let mut reader = &bytes[..];
        assert_eq!(read_frame(&mut reader).await.unwrap(), Some(frame));
        assert_eq!(read_frame(&mut reader).await.unwrap(), None, "clean EOF between frames");
    }

    /// The property the whole format exists for: a connection that drops
    /// mid-frame is an error, never a shorter file.
    #[tokio::test]
    async fn a_torn_frame_is_an_error_at_every_cut() {
        let frame = Frame::with_payload(serde_json::json!({"t": "ok", "id": 1, "result": null}), vec![7; 300]);
        let bytes = encode(&frame).unwrap();
        for cut in 1..bytes.len() {
            let mut reader = &bytes[..cut];
            let err = read_frame(&mut reader).await.expect_err(&format!("cut at {cut} must fail"));
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "cut at {cut}");
        }
    }

    #[tokio::test]
    async fn absurd_lengths_are_refused_before_allocating() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let mut reader = &bytes[..];
        assert_eq!(read_frame(&mut reader).await.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    /// Fuzz-ish: many frames of pseudo-random shapes, concatenated, read back in
    /// order through a reader that hands out arbitrary small slices.
    #[tokio::test]
    async fn a_stream_of_random_frames_reads_back_exactly() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut frames = Vec::new();
        let mut stream = Vec::new();
        for i in 0..200 {
            let len = (next() % 5000) as usize;
            let payload: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let name: Vec<u8> = (0..(next() % 20)).map(|_| (next() as u8) | 1).collect();
            let frame = Frame::with_payload(
                serde_json::json!({"t": "entries", "wt": i, "entries": [{"path": WirePath(name), "kind": "file", "size": len, "mtime": -1, "mode": 0o644, "ignored": false}]}),
                payload,
            );
            stream.extend_from_slice(&encode(&frame).unwrap());
            frames.push(frame);
        }

        // A reader that returns at most a few bytes per call exercises every
        // partial-read path in `read_frame`.
        struct Dribble<'a> {
            data: &'a [u8],
            step: usize,
        }
        impl AsyncRead for Dribble<'_> {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                self.step = self.step % 7 + 1;
                let n = self.step.min(self.data.len()).min(buf.remaining());
                let (head, tail) = self.data.split_at(n);
                buf.put_slice(head);
                self.data = tail;
                std::task::Poll::Ready(Ok(()))
            }
        }
        let mut reader = Dribble { data: &stream, step: 0 };
        for expected in &frames {
            let got = read_frame(&mut reader).await.unwrap().expect("a frame");
            assert_eq!(&got, expected);
        }
        assert!(read_frame(&mut reader).await.unwrap().is_none());
    }

    #[test]
    fn a_non_utf8_path_round_trips_through_json() {
        let raw = WirePath(vec![b'a', 0xFF, b'/', 0xC3, b'\n', b'z']);
        let json = serde_json::to_string(&raw).unwrap();
        assert!(json.contains("b64"), "{json}");
        assert_eq!(serde_json::from_str::<WirePath>(&json).unwrap(), raw);

        let plain = WirePath::text("src/main.rs");
        assert_eq!(serde_json::to_string(&plain).unwrap(), "\"src/main.rs\"");
    }

    #[test]
    fn the_ui_escape_is_lossless_for_every_byte_pattern() {
        let cases: Vec<Vec<u8>> = vec![
            b"plain/path.txt".to_vec(),
            vec![0xFF, 0xFE, b'x'],
            vec![b'a', 0xC3],                 // truncated sequence at the end
            "caf\u{e9}".as_bytes().to_vec(),  // valid multibyte
            "\u{F780}\u{F7FF}lit".as_bytes().to_vec(), // literal escape-range chars
            vec![0xEF, 0x9E, 0x80],           // the bytes of U+F780 itself
            (0x80..=0xFF).collect(),
        ];
        for bytes in cases {
            let text = ui_escape(&bytes);
            assert_eq!(ui_unescape(&text), bytes, "{text:?}");
        }
        assert_eq!(ui_escape(b"src/lib.rs"), "src/lib.rs", "ordinary paths look like themselves");
    }

    #[test]
    fn a_request_never_shadows_the_envelope_id() {
        let all = vec![
            Request::OpenWorktree { root: WirePath::text("/r"), limit: Some(3) },
            Request::CloseWorktree { wt: 1 },
            Request::ListDir { path: WirePath::text("/r") },
            Request::Stat { path: WirePath::text("/r") },
            Request::Read { path: WirePath::text("/r"), max: None },
            Request::Write { path: WirePath::text("/r"), expected_mtime: Some(5) },
            Request::CreateFile { path: WirePath::text("/r") },
            Request::CreateDir { path: WirePath::text("/r") },
            Request::Rename { from: WirePath::text("/a"), to: WirePath::text("/b") },
            Request::Delete { path: WirePath::text("/r"), recursive: true },
            Request::UploadBegin { path: WirePath::text("/r") },
            Request::UploadChunk { upload: 1 },
            Request::UploadFinish { upload: 1, size: 3 },
            Request::UploadAbort { upload: 1 },
            Request::Search { root: WirePath::text("/r"), query: Search::default() },
            Request::Home,
        ];
        for request in all {
            let envelope = Envelope { id: 42, request };
            let text = serde_json::to_string(&envelope).unwrap();
            assert_eq!(text.matches("\"id\"").count(), 1, "{text}");
            assert_eq!(serde_json::from_str::<Envelope>(&text).unwrap(), envelope);
        }
    }
}
