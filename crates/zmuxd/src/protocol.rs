//! The wire format between the agent daemon and an attached client.
//!
//! Deliberately tiny, and deliberately **not** a terminal protocol. The daemon
//! moves opaque bytes; it never interprets an escape sequence, never maintains a
//! grid, never has an opinion about scrolling or the cursor.
//!
//! That restraint is the entire point. A multiplexer like tmux is a second
//! terminal emulator sitting between the shell and the real one, so scrollback,
//! selection and cursor movement are all *its* implementation rather than the
//! terminal you are actually using — which is why scrolling in tmux feels like a
//! different application, because it is one. Here the shell's bytes reach
//! xterm.js unaltered, so native scrolling, GPU rendering and cursor semantics
//! are simply the terminal's own.
//!
//! Frames are `[kind: u8][len: u32 LE][payload]`.

use serde::{Deserialize, Serialize};

/// Maximum payload accepted in one frame.
///
/// A bound is required: without one, a corrupt or hostile length prefix makes
/// the reader allocate whatever it was told to.
pub const MAX_PAYLOAD: usize = 4 * 1024 * 1024;

const KIND_HELLO: u8 = 0;
const KIND_DATA: u8 = 1;
const KIND_RESIZE: u8 = 2;
const KIND_EXITED: u8 = 3;
const KIND_KILL: u8 = 4;
const KIND_SET_ENV: u8 = 5;
const KIND_LIST: u8 = 6;
const KIND_SESSIONS: u8 = 7;

/// What an attaching client asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Stable name for the session — the same name reattaches to the same shell.
    pub session: String,
    /// Working directory, used only when the session is created.
    pub cwd: Option<String>,
    /// Program to run. `None` means the user's login shell.
    pub program: Option<String>,
    pub args: Vec<String>,
    /// A command line for the user's **login** shell to run.
    ///
    /// Claude needs this rather than `program`: it is normally installed by a
    /// version manager (nvm, asdf, mise) whose PATH exists only in a login
    /// shell, so spawning the binary directly gives "claude: command not found"
    /// on a host where it is plainly installed.
    ///
    /// `#[serde(default)]` so a daemon left running by an older build still
    /// accepts a Hello from this one instead of failing to parse it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_command: Option<String>,
    /// Environment for the spawned process.
    ///
    /// **This is the only safe channel for a secret.** `spec_to_shell_line`
    /// renders environment into a command line, and any user on the host can read
    /// another user's command line out of `ps` — so a token passed that way is
    /// disclosed to the whole machine. A `Hello` travels over the agent's `0600`
    /// socket and never appears in an argument list.
    ///
    /// `#[serde(default)]` so a daemon left running by an older build still
    /// accepts a Hello from this one.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

/// One message on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Client → daemon, always first.
    Hello(Hello),
    /// Raw terminal bytes, in either direction.
    Data(Vec<u8>),
    /// Client → daemon: the window changed.
    Resize { cols: u16, rows: u16 },
    /// Daemon → client: the shell exited.
    Exited { code: i32 },
    /// Client → daemon: environment to apply to sessions created from now on.
    ///
    /// Exists so a credential can reach the far side **without ever being an
    /// argument**. Any user on a host can read another user's command line out
    /// of `ps`, so a token passed as a flag is disclosed machine-wide. This
    /// travels over the agent's `0600` socket, and the daemon keeps it in memory
    /// only — nothing is written to disk.
    SetEnv(std::collections::BTreeMap<String, String>),
    /// Client → daemon: end this session for good.
    ///
    /// Closing a tab has to say so explicitly. The whole design is that dropping
    /// a connection leaves the shell running, so without this every closed tab
    /// would leak a shell that nothing can ever reach again.
    Kill { session: String },
    /// Client → daemon: what are you running?
    ///
    /// **This is what makes a leak findable.** Sessions deliberately outlive
    /// both the client and the network, so a session whose tab is gone keeps
    /// running with nothing able to reach it. Without an enumeration the only
    /// way to discover one is `ps` on the host and correlating by hand — which
    /// means in practice nobody does, and shells accumulate for months.
    List,
    /// Daemon → client: everything it is running.
    Sessions(Vec<SessionSummary>),
}

/// One session, as the daemon sees it.
///
/// JSON rather than a packed layout: this is a once-in-a-while control message,
/// not the hot path, and a self-describing encoding means an older client shown
/// a newer daemon's answer degrades to missing fields rather than garbage.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// The name zmux reattaches by — `term-…` or `claude-…`.
    pub name: String,
    /// The shell's pid on the host, so the operator can find it in `ps`.
    pub pid: Option<u32>,
    /// Seconds since it was created. Age is what identifies a leak: a shell
    /// older than the app that started it had no tab for most of its life.
    pub age_seconds: u64,
    /// Whether a client is attached **right now**. An unattached session is not
    /// necessarily leaked — zmux may simply be closed — but every leaked one is
    /// unattached, so this is the first thing to sort by.
    pub attached: bool,
    /// What it is running, when the daemon was told. Distinguishes a shell from
    /// a Claude conversation without parsing the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Append a data frame directly to `out`.
///
/// The hot path — every byte a shell produces goes through it — so it copies the
/// payload exactly once, into a buffer the caller reuses. Building a `Frame` and
/// calling `encode` instead costs three copies per chunk (into the frame, out of
/// it, then into the output), which on a `cat` of a large file is real work for
/// no benefit.
pub fn encode_data_into(out: &mut Vec<u8>, payload: &[u8]) {
    out.clear();
    out.reserve(5 + payload.len());
    out.push(KIND_DATA);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let (kind, payload) = match self {
            Frame::Hello(hello) => (
                KIND_HELLO,
                serde_json::to_vec(hello).expect("Hello is always serialisable"),
            ),
            Frame::Data(bytes) => (KIND_DATA, bytes.clone()),
            Frame::Resize { cols, rows } => {
                let mut p = Vec::with_capacity(4);
                p.extend_from_slice(&cols.to_le_bytes());
                p.extend_from_slice(&rows.to_le_bytes());
                (KIND_RESIZE, p)
            }
            Frame::Exited { code } => (KIND_EXITED, code.to_le_bytes().to_vec()),
            Frame::Kill { session } => (KIND_KILL, session.as_bytes().to_vec()),
            Frame::List => (KIND_LIST, Vec::new()),
            Frame::Sessions(sessions) => (
                KIND_SESSIONS,
                serde_json::to_vec(sessions).expect("summaries are always serialisable"),
            ),
            Frame::SetEnv(env) => (
                KIND_SET_ENV,
                serde_json::to_vec(env).expect("an env map is always serialisable"),
            ),
        };

        let mut out = Vec::with_capacity(5 + payload.len());
        out.push(kind);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    /// Decode one frame from `buf`.
    ///
    /// Returns the frame and how many bytes it consumed, or `Ok(None)` when more
    /// input is needed. A stream protocol has to distinguish "not yet" from
    /// "broken", or a frame split across two reads looks like corruption.
    pub fn decode(buf: &[u8]) -> anyhow::Result<Option<(Frame, usize)>> {
        if buf.len() < 5 {
            return Ok(None);
        }

        let kind = buf[0];
        let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        anyhow::ensure!(len <= MAX_PAYLOAD, "frame claims {len} bytes, over the limit");

        let end = 5 + len;
        if buf.len() < end {
            return Ok(None);
        }
        let payload = &buf[5..end];

        let frame = match kind {
            KIND_HELLO => Frame::Hello(serde_json::from_slice(payload)?),
            KIND_DATA => Frame::Data(payload.to_vec()),
            KIND_RESIZE => {
                anyhow::ensure!(payload.len() == 4, "malformed resize frame");
                Frame::Resize {
                    cols: u16::from_le_bytes([payload[0], payload[1]]),
                    rows: u16::from_le_bytes([payload[2], payload[3]]),
                }
            }
            KIND_EXITED => {
                anyhow::ensure!(payload.len() == 4, "malformed exit frame");
                Frame::Exited {
                    code: i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]),
                }
            }
            KIND_KILL => Frame::Kill { session: String::from_utf8(payload.to_vec())? },
            KIND_SET_ENV => Frame::SetEnv(serde_json::from_slice(payload)?),
            KIND_LIST => Frame::List,
            KIND_SESSIONS => Frame::Sessions(serde_json::from_slice(payload)?),
            other => anyhow::bail!("unknown frame kind {other}"),
        };

        Ok(Some((frame, end)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Hello {
        Hello {
            session: "api".into(),
            cwd: Some("/srv/api".into()),
            program: None,
            args: vec![],
            login_command: None,
            env: Default::default(),
            cols: 120,
            rows: 40,
        }
    }

    #[test]
    fn every_frame_round_trips() {
        for frame in [
            Frame::Hello(hello()),
            Frame::Data(b"hello world".to_vec()),
            Frame::Resize { cols: 200, rows: 60 },
            Frame::Exited { code: 3 },
            Frame::Kill { session: "api".into() },
            Frame::SetEnv([("A".to_owned(), "b".to_owned())].into_iter().collect()),
        ] {
            let encoded = frame.encode();
            let (decoded, used) = Frame::decode(&encoded).unwrap().unwrap();
            assert_eq!(decoded, frame);
            assert_eq!(used, encoded.len(), "decode must consume exactly one frame");
        }
    }

    #[test]
    fn a_frame_split_across_reads_is_not_an_error() {
        // The distinction that makes a stream protocol work: "incomplete" is not
        // "corrupt". Treating a partial read as a failure would drop terminal
        // output on any chunk boundary.
        let encoded = Frame::Data(b"abcdefghij".to_vec()).encode();

        for cut in 0..encoded.len() {
            assert!(
                Frame::decode(&encoded[..cut]).unwrap().is_none(),
                "a partial frame ({cut} bytes) should ask for more, not fail"
            );
        }
        assert!(Frame::decode(&encoded).unwrap().is_some());
    }

    #[test]
    fn several_frames_in_one_buffer_decode_in_order() {
        // Terminal output arrives coalesced; the reader must drain everything it
        // was handed rather than one frame per read.
        let mut buf = Frame::Data(b"one".to_vec()).encode();
        buf.extend(Frame::Data(b"two".to_vec()).encode());
        buf.extend(Frame::Resize { cols: 80, rows: 24 }.encode());

        let mut cursor = 0;
        let mut frames = Vec::new();
        while let Some((frame, used)) = Frame::decode(&buf[cursor..]).unwrap() {
            cursor += used;
            frames.push(frame);
        }

        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], Frame::Data(b"one".to_vec()));
        assert_eq!(frames[2], Frame::Resize { cols: 80, rows: 24 });
        assert_eq!(cursor, buf.len());
    }

    #[test]
    fn an_oversized_length_is_rejected_rather_than_allocated() {
        // Without this a corrupt prefix asks the reader to allocate 4GB.
        let mut evil = vec![KIND_DATA];
        evil.extend_from_slice(&(u32::MAX).to_le_bytes());

        assert!(Frame::decode(&evil).is_err());
    }

    #[test]
    fn the_fast_path_encodes_identically_to_the_general_one() {
        // `encode_data_into` exists purely to avoid copies. If it ever diverged
        // from `encode`, output would corrupt only under load — the worst kind of
        // bug to find.
        for payload in [b"".as_slice(), b"x".as_slice(), &[0u8, 255, 27, 91][..]] {
            let mut fast = Vec::new();
            encode_data_into(&mut fast, payload);
            assert_eq!(fast, Frame::Data(payload.to_vec()).encode());
        }
    }

    #[test]
    fn the_fast_path_reuses_its_buffer() {
        // The point is to stop allocating per chunk, so a second call must not
        // append to the first.
        let mut buf = Vec::new();
        encode_data_into(&mut buf, b"first");
        let after_first = buf.len();
        encode_data_into(&mut buf, b"second-and-longer");

        assert_ne!(buf.len(), after_first + 5 + 17, "the buffer was appended to, not reused");
        let (decoded, used) = Frame::decode(&buf).unwrap().unwrap();
        assert_eq!(decoded, Frame::Data(b"second-and-longer".to_vec()));
        assert_eq!(used, buf.len());
    }

    #[test]
    fn binary_payloads_survive_unchanged() {
        // The daemon carries raw terminal bytes, including NULs and invalid
        // UTF-8. Anything that assumed text here would corrupt output.
        let nasty = vec![0u8, 27, 91, 65, 255, 254, 0, 10, 13];
        let encoded = Frame::Data(nasty.clone()).encode();
        let (decoded, _) = Frame::decode(&encoded).unwrap().unwrap();

        assert_eq!(decoded, Frame::Data(nasty));
    }

    #[test]
    fn an_empty_data_frame_is_valid() {
        let encoded = Frame::Data(Vec::new()).encode();
        let (decoded, used) = Frame::decode(&encoded).unwrap().unwrap();
        assert_eq!(decoded, Frame::Data(Vec::new()));
        assert_eq!(used, 5);
    }

    #[test]
    fn an_unknown_kind_is_an_error_not_a_silent_skip() {
        let mut unknown = vec![99u8];
        unknown.extend_from_slice(&0u32.to_le_bytes());
        assert!(Frame::decode(&unknown).is_err());
    }
}
