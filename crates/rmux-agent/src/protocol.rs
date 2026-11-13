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
const KIND_WRITE: u8 = 8;
const KIND_ACK: u8 = 9;
const KIND_READ: u8 = 10;
const KIND_SCROLLBACK: u8 = 11;
const KIND_SET_KIND_ENV: u8 = 12;
const KIND_SET_NAMED_ENV: u8 = 13;
const KIND_HELLO_BOUND: u8 = 14;
const KIND_REFUSED: u8 = 15;
const KIND_GET_BINDING: u8 = 16;
const KIND_BINDING: u8 = 17;
const KIND_SWITCH: u8 = 18;

/// Which credential set a session runs on — named per session, not per daemon.
///
/// The per-kind set ([`Frame::SetKindEnv`]) is one environment shared by every
/// session of that kind on the host, which cannot express two Claude sessions
/// on two accounts, a child inheriting its parent's provider, or one session
/// moving to another account without moving everyone. A binding names the set
/// (a profile id delivered with [`Frame::SetNamedEnv`], or one of the reserved
/// names in `rmux_claude_core::env_kind`) and, for a fallback chain, carries the
/// chain's state so the daemon can switch members itself while the app is
/// closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    /// `claude`, `deepseek` or `openai` — must match the session's name prefix.
    pub kind: String,
    /// The set this session spawns with. For a chain, the active member.
    pub set: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<rmux_claude_core::fallback::ChainState>,
}

/// What [`Frame::GetBinding`] answers: the binding plus what the daemon knows
/// about it. Names and labels only — never a value from a set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingReport {
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<Binding>,
    /// Display names for the set names that appear above.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub labels: std::collections::BTreeMap<String, String>,
    /// When the current set was bound (unix seconds).
    #[serde(default)]
    pub since: u64,
}

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
    /// Client → daemon: **replace** the credential environment for one agent
    /// kind (`claude`, `deepseek`, `openai` — see
    /// `rmux_claude_core::env_kind`). Applied only to sessions of that kind at
    /// spawn, so one provider's credentials never reach another agent.
    ///
    /// Answered with [`Frame::Ack`] during the handshake — `ok: false` for a
    /// kind the daemon does not know. The answer is the point: a daemon from
    /// before this frame existed fails to decode it and drops the connection,
    /// so a client that sees no `Ack` knows the credentials did **not** land
    /// and says so, instead of starting a session that silently falls back to
    /// whatever provider the host defaults to. `SetEnv` is fire-and-forget and
    /// could never have told it.
    SetKindEnv { kind: String, env: std::collections::BTreeMap<String, String> },
    /// Client → daemon: store one **named** credential set for `kind`, replacing
    /// any set of that name. A session bound to it ([`Frame::HelloBound`]) gets
    /// exactly this set at spawn — and nothing from the per-kind set.
    ///
    /// An empty value is a **removal**: the key is taken out of the environment
    /// the session would otherwise inherit (the shared account, the host's shell
    /// profile). That is how a subscription-token profile clears
    /// `ANTHROPIC_API_KEY`. Answered with [`Frame::Ack`]; an older daemon drops
    /// the connection instead, and the client reports that loudly.
    SetNamedEnv {
        kind: String,
        set: String,
        label: Option<String>,
        env: std::collections::BTreeMap<String, String>,
    },
    /// Client → daemon: a [`Hello`] that also binds the session to a set.
    ///
    /// **A separate frame kind, not a field on `Hello`, and that is the safety
    /// property.** `Hello` is parsed leniently so old daemons accept new
    /// clients; a binding field there would be *ignored* by an older daemon,
    /// which would then start the session on whatever provider the host
    /// defaults to while the app named another — the silent mis-routing this
    /// whole design exists to prevent. An older daemon cannot decode this frame
    /// and drops the connection, and the client refuses to start.
    ///
    /// Answered with `Ack { ok: true }` before the replay, or [`Frame::Refused`]
    /// (unknown set, wrong kind) and a close.
    HelloBound { hello: Hello, binding: Binding },
    /// Daemon → client: a [`Frame::HelloBound`] or [`Frame::Switch`] was refused,
    /// and why — said in the session's own terminal, where the operator looks.
    Refused { reason: String },
    /// Client → daemon: which set is this session on? Handshake-only.
    GetBinding { session: String },
    /// Daemon → client: the answer to [`Frame::GetBinding`].
    BindingIs(BindingReport),
    /// Client → daemon: move a chained session to `member` now and relaunch it —
    /// the operator's "switch back". Answered `Ack` or `Refused`.
    Switch { session: String, member: String },
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
    /// Client → daemon: type these bytes into a session **without attaching**.
    ///
    /// The bridge's whole reason for existing: Redstone asks a host to send a
    /// message to a Claude conversation, and the alternative — attach, write,
    /// detach — is wrong in three separate ways. Attaching replays the entire
    /// scrollback back at a client that wants to send four words; it flips
    /// `attached`, so a session being driven remotely stops looking abandoned in
    /// `list` when it may well be; and it resizes the terminal to whatever
    /// dimensions the caller invented, which reflows the conversation of anyone
    /// actually looking at it.
    ///
    /// Handled during the **handshake**, like [`Frame::Kill`] and [`Frame::List`],
    /// and for exactly the reason that cost a week there: a send-and-close client
    /// must be answered before any attach is attempted, or the daemon writes a
    /// replay to a socket that is already gone and never reads the frame.
    ///
    /// It deliberately **cannot create a session**. `Hello` creates; this does
    /// not. A typo'd name from a remote caller must be an error, not a new shell.
    Write { session: String, data: Vec<u8> },
    /// Daemon → client: whether the last one-shot request landed.
    ///
    /// `false` means the session did not exist. Distinguishing that from success
    /// is the difference between Redstone reporting "sent" and reporting "there
    /// is no such session any more" — and a remote caller working from a list it
    /// fetched a minute ago hits the second case routinely.
    Ack { ok: bool },
    /// Client → daemon: hand back a session's recent output **without attaching**.
    ///
    /// The read half of the same idea as [`Frame::Write`]: Redstone wants to
    /// show a terminal's history without flipping `attached`, resizing it, or
    /// joining its live stream. Answered during the handshake for the same
    /// reason `Write` is — the caller reads one reply and closes.
    ///
    /// It is the daemon's scrollback buffer verbatim: raw bytes, escape
    /// sequences and all. The daemon performs no emulation (that is its founding
    /// invariant), so it cannot hand back a rendered screen — only what the shell
    /// actually wrote.
    Read { session: String },
    /// Daemon → client: the scrollback, in answer to [`Frame::Read`].
    ///
    /// Empty when the session does not exist — told apart from a genuinely empty
    /// buffer by the [`Frame::Ack`] that is *not* sent, i.e. a caller that needs
    /// the distinction uses `Write`/`Ack`; a reader just takes what it gets.
    Scrollback(Vec<u8>),
}

/// The payload of [`Frame::SetNamedEnv`].
#[derive(Serialize, Deserialize)]
struct NamedEnv {
    kind: String,
    set: String,
    #[serde(default)]
    label: Option<String>,
    env: std::collections::BTreeMap<String, String>,
}

/// The payload of [`Frame::HelloBound`].
#[derive(Serialize, Deserialize)]
struct HelloBoundWire {
    hello: Hello,
    binding: Binding,
}

/// The payload of [`Frame::Switch`].
#[derive(Serialize, Deserialize)]
struct SwitchWire {
    session: String,
    member: String,
}

/// The payload of [`Frame::SetKindEnv`].
#[derive(Serialize, Deserialize)]
struct KindEnv {
    kind: String,
    env: std::collections::BTreeMap<String, String>,
}

/// One session, as the daemon sees it.
///
/// JSON rather than a packed layout: this is a once-in-a-while control message,
/// not the hot path, and a self-describing encoding means an older client shown
/// a newer daemon's answer degrades to missing fields rather than garbage.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// The name rmux reattaches by — `term-…` or `claude-…`.
    pub name: String,
    /// The shell's pid on the host, so the operator can find it in `ps`.
    pub pid: Option<u32>,
    /// Seconds since it was created. Age is what identifies a leak: a shell
    /// older than the app that started it had no tab for most of its life.
    pub age_seconds: u64,
    /// Whether a client is attached **right now**. An unattached session is not
    /// necessarily leaked — rmux may simply be closed — but every leaked one is
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
            Frame::SetKindEnv { kind, env } => (
                KIND_SET_KIND_ENV,
                serde_json::to_vec(&KindEnv { kind: kind.clone(), env: env.clone() })
                    .expect("a kind env is always serialisable"),
            ),
            // `[name_len: u16 LE][name][data]`, not JSON. The data is arbitrary
            // bytes — a pasted message runs to kilobytes — and a JSON array of
            // numbers inflates that by roughly four times for no gain, on a
            // protocol whose stated virtue is being tiny.
            Frame::Write { session, data } => {
                let name = session.as_bytes();
                let mut p = Vec::with_capacity(2 + name.len() + data.len());
                p.extend_from_slice(&(name.len() as u16).to_le_bytes());
                p.extend_from_slice(name);
                p.extend_from_slice(data);
                (KIND_WRITE, p)
            }
            Frame::Ack { ok } => (KIND_ACK, vec![u8::from(*ok)]),
            Frame::SetNamedEnv { kind, set, label, env } => (
                KIND_SET_NAMED_ENV,
                serde_json::to_vec(&NamedEnv {
                    kind: kind.clone(),
                    set: set.clone(),
                    label: label.clone(),
                    env: env.clone(),
                })
                .expect("a named env is always serialisable"),
            ),
            Frame::HelloBound { hello, binding } => (
                KIND_HELLO_BOUND,
                serde_json::to_vec(&HelloBoundWire { hello: hello.clone(), binding: binding.clone() })
                    .expect("a bound hello is always serialisable"),
            ),
            Frame::Refused { reason } => (KIND_REFUSED, reason.as_bytes().to_vec()),
            Frame::GetBinding { session } => (KIND_GET_BINDING, session.as_bytes().to_vec()),
            Frame::BindingIs(report) => (
                KIND_BINDING,
                serde_json::to_vec(report).expect("a binding report is always serialisable"),
            ),
            Frame::Switch { session, member } => (
                KIND_SWITCH,
                serde_json::to_vec(&SwitchWire { session: session.clone(), member: member.clone() })
                    .expect("a switch is always serialisable"),
            ),
            Frame::Read { session } => (KIND_READ, session.as_bytes().to_vec()),
            Frame::Scrollback(bytes) => (KIND_SCROLLBACK, bytes.clone()),
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
            KIND_SET_KIND_ENV => {
                let KindEnv { kind, env } = serde_json::from_slice(payload)?;
                Frame::SetKindEnv { kind, env }
            }
            KIND_LIST => Frame::List,
            KIND_SESSIONS => Frame::Sessions(serde_json::from_slice(payload)?),
            KIND_WRITE => {
                anyhow::ensure!(payload.len() >= 2, "malformed write frame");
                let name_len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                // Checked before slicing. A hostile length here would otherwise
                // panic the daemon on an out-of-range index, which is a remote
                // caller crashing the thing that holds every session on the host.
                anyhow::ensure!(
                    payload.len() >= 2 + name_len,
                    "write frame claims a {name_len}-byte name in {} bytes",
                    payload.len(),
                );
                Frame::Write {
                    session: String::from_utf8(payload[2..2 + name_len].to_vec())?,
                    data: payload[2 + name_len..].to_vec(),
                }
            }
            KIND_ACK => {
                anyhow::ensure!(payload.len() == 1, "malformed ack frame");
                Frame::Ack { ok: payload[0] != 0 }
            }
            KIND_READ => Frame::Read { session: String::from_utf8(payload.to_vec())? },
            KIND_SET_NAMED_ENV => {
                let NamedEnv { kind, set, label, env } = serde_json::from_slice(payload)?;
                Frame::SetNamedEnv { kind, set, label, env }
            }
            KIND_HELLO_BOUND => {
                let HelloBoundWire { hello, binding } = serde_json::from_slice(payload)?;
                Frame::HelloBound { hello, binding }
            }
            KIND_REFUSED => Frame::Refused { reason: String::from_utf8_lossy(payload).into_owned() },
            KIND_GET_BINDING => Frame::GetBinding { session: String::from_utf8(payload.to_vec())? },
            KIND_BINDING => Frame::BindingIs(serde_json::from_slice(payload)?),
            KIND_SWITCH => {
                let SwitchWire { session, member } = serde_json::from_slice(payload)?;
                Frame::Switch { session, member }
            }
            KIND_SCROLLBACK => Frame::Scrollback(payload.to_vec()),
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
            Frame::SetKindEnv {
                kind: "openai".into(),
                env: [("OPENAI_API_KEY".to_owned(), "k".to_owned())].into_iter().collect(),
            },
            Frame::SetNamedEnv {
                kind: "claude".into(),
                set: "7".into(),
                label: Some("Work Max".into()),
                env: [("CLAUDE_CODE_OAUTH_TOKEN".to_owned(), "t".to_owned()), ("ANTHROPIC_API_KEY".to_owned(), String::new())]
                    .into_iter()
                    .collect(),
            },
            Frame::HelloBound {
                hello: hello(),
                binding: Binding {
                    kind: "claude".into(),
                    set: "2".into(),
                    chain: Some(rmux_claude_core::fallback::ChainState {
                        id: "c1".into(),
                        members: vec!["2".into(), "3".into()],
                        active: "2".into(),
                        ..Default::default()
                    }),
                },
            },
            Frame::Refused { reason: "no set called 9".into() },
            Frame::GetBinding { session: "claude-x".into() },
            Frame::BindingIs(BindingReport { session: "claude-x".into(), since: 5, ..Default::default() }),
            Frame::Switch { session: "claude-x".into(), member: "2".into() },
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
    fn the_kind_env_frame_keeps_the_old_frames_wire_format() {
        // `SetEnv` is unchanged on the wire, so an older client still talks to
        // this daemon; the per-kind frame is a new kind number, which an older
        // daemon rejects outright (`an_unknown_kind_is_…` below) rather than
        // misreading as the global environment.
        let old = Frame::SetEnv([("A".to_owned(), "b".to_owned())].into_iter().collect()).encode();
        assert_eq!(old[0], KIND_SET_ENV);
        assert_eq!(&old[5..], br#"{"A":"b"}"#);
        let new = Frame::SetKindEnv { kind: "claude".into(), env: Default::default() }.encode();
        assert_eq!(new[0], KIND_SET_KIND_ENV);
        assert_ne!(new[0], KIND_SET_ENV);
    }

    #[test]
    fn a_bound_hello_is_a_new_kind_so_an_old_daemon_cannot_ignore_the_binding() {
        // The compatibility rule: an older daemon parses `Hello` leniently and
        // would *drop* an unknown field, starting the session on the host's
        // default provider. A new frame kind is rejected outright instead.
        let bound = Frame::HelloBound {
            hello: hello(),
            binding: Binding { kind: "claude".into(), set: "1".into(), chain: None },
        }
        .encode();
        assert_eq!(bound[0], KIND_HELLO_BOUND);
        assert_ne!(bound[0], KIND_HELLO);
        // And a plain Hello is byte-for-byte what it always was.
        let plain = Frame::Hello(hello()).encode();
        assert_eq!(plain[0], KIND_HELLO);
        assert!(!String::from_utf8_lossy(&plain[5..]).contains("binding"));
    }

    #[test]
    fn an_unknown_kind_is_an_error_not_a_silent_skip() {
        let mut unknown = vec![99u8];
        unknown.extend_from_slice(&0u32.to_le_bytes());
        assert!(Frame::decode(&unknown).is_err());
    }
}
