//! The wire contract between a host's rmux bridge and Redstone.
//!
//! This is the piece that outlives both products, so — like
//! [`rmux_control::protocol`] — it is defined once, here, rather than growing
//! out of whatever the first caller happened to need. The two protocols are
//! deliberately the same *shape*: a request carries an `id` and gets exactly one
//! response with that `id`, an event has no `id` and is pushed when state
//! changes. Anyone who has read one has read both.
//!
//! ## The direction is inverted, and that is the whole design
//!
//! `rmux-control` is rmux *serving* a local client over a unix socket. This is
//! the opposite: the bridge runs on a **server**, and it **dials out** to
//! Redstone. Redstone is the caller; the bridge answers.
//!
//! Outbound-only, because the alternative does not work. A listening port on a
//! dev box needs a firewall hole, a public address, and a certificate — three
//! things that are somebody's job on every machine, forever. A host that can
//! reach `https://redstone/` can reach it from behind NAT, from a laptop on
//! hotel wifi, and from a VPC with no ingress at all, and nothing has to be
//! opened.
//!
//! ## One WebSocket message per JSON object
//!
//! The WebSocket already frames, so there is no newline delimiter to agree on —
//! but the *objects* are byte-identical to what `rmux-control` puts on its
//! socket, so a client can be ported between the two by changing the transport
//! and nothing else.
//!
//! ## What this protocol deliberately cannot do
//!
//! **There is no `exec`, and there must never be one.** This is the single most
//! important sentence in the file.
//!
//! The bridge is a credential sitting on a development server, reachable by
//! whoever holds the token. Its verbs are a closed set — list what is running,
//! start a Claude in a folder, type into one, read a transcript — and every one
//! of them is something the operator could have done from rmux's own UI. Add a
//! method that runs a command and the token stops being "drive my Claude
//! sessions" and becomes "shell on every machine I have ever ssh'd into", which
//! is a different security decision that nobody made.
//!
//! The pressure to add one will be real: an agent that can run `ls` is more
//! capable than an agent that cannot. The answer is that Claude *already runs
//! commands* — inside a session, under its own permission prompts, with the
//! operator able to see and interrupt it. Routing shell access through a Claude
//! session keeps the existing safety machinery in the path. A bare `exec`
//! removes it.
//!
//! ## Transcripts, never the screen
//!
//! Redstone reads a conversation from Claude's own `.jsonl`, which is structured
//! and authoritative. It is never shown the terminal buffer. rmux's standing
//! rule is that it *may observe* the TUI and must not *reimplement* it, and a
//! remote client scraping rendered pixels would be the largest possible version
//! of that mistake — a second interface over Claude's own, always one release
//! behind it.

use serde::{Deserialize, Serialize};

/// The protocol version, sent in the greeting.
///
/// Redstone and rmux ship separately and *will* drift — a host may be running an
/// agent from six weeks ago because nothing has restarted its daemon. A mismatch
/// must surface as a clear message rather than a field that silently
/// deserialises to its default.
pub const VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Bridge → Redstone: the greeting
// ---------------------------------------------------------------------------

/// The bridge's first message after the socket opens.
///
/// The **token is not in here** — it goes in the `Authorization` header at
/// connect time, so an unauthenticated peer never gets a socket at all. Putting
/// it in the first frame would mean accepting, allocating for and parsing input
/// from anyone who can reach the URL.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    /// This file's [`VERSION`].
    pub protocol: u32,
    /// The agent binary's version, so Redstone can explain a missing feature by
    /// naming the version that has it rather than reporting "unsupported".
    pub agent_version: String,
    pub host: HostInfo,
}

/// What Redstone says back. Sent once, before any request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub protocol: u32,
    /// Redstone's own build, for the same reason the agent sends its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    /// The host id Redstone filed this connection under. Echoed so an operator
    /// looking at two machines with the same hostname can tell them apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
}

/// The machine the bridge is running on.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    /// `uname -n`. Not unique — every fresh cloud image is `localhost` — which
    /// is exactly why Redstone keys on its own host id and treats this as a
    /// label.
    pub hostname: String,
    /// The account the bridge runs as. **The ceiling on everything this
    /// connection can do**: the bridge has no privilege beyond this user's, so
    /// showing it is showing the blast radius.
    pub user: String,
    /// `linux`, `macos`, … Free text; an unknown value must not fail a parse.
    pub os: String,
    /// The absolute path of `$HOME`, so Redstone can render `~`-relative folders
    /// without guessing at the layout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

// ---------------------------------------------------------------------------
// Redstone → bridge: requests
// ---------------------------------------------------------------------------

/// What Redstone asks a host to do.
///
/// Every variant is something the operator could have done from rmux's own UI on
/// that host. Nothing here reaches past what the session model already allows —
/// see the module note on why there is no `exec`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum Request {
    /// Everything the daemons on this host are running right now.
    ///
    /// Unions every daemon, not just the newest: a rebuilt agent runs beside its
    /// predecessor until that one's sessions end, and the sessions worth finding
    /// are frequently the ones an upgrade left behind.
    ListSessions,

    /// The machine's own details, refreshed. Sent in `Hello` too — this exists
    /// for a long-lived connection whose host was renamed under it.
    HostInfo,

    /// Start a new Claude conversation in a folder.
    ///
    /// The folder must already exist. Creating it would make a typo in an agent's
    /// tool call silently produce a directory tree on someone's server.
    Spawn {
        /// Absolute, or `~`-relative. Never interpolated into a shell line
        /// unquoted — see `rmux_transport::shell_quote`.
        folder: String,
        /// Typed into the composer and **submitted**. This is the one place
        /// Redstone genuinely drives Claude, and it is why enrolment is an
        /// explicit act by the operator rather than a default.
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        /// A display name for rmux's rail. Defaults to the folder's basename.
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Which stored model profile to run against, by name.
        ///
        /// A profile decides **where the operator's credential is sent**, so an
        /// unknown name is refused rather than falling back to Anthropic. A
        /// silent change of provider is the failure worth being loud about.
        #[serde(skip_serializing_if = "Option::is_none")]
        model_profile: Option<String>,
    },

    /// Type a message into a running Claude and submit it.
    Send {
        /// The session name from [`Request::ListSessions`].
        session: String,
        message: String,
    },

    /// Interrupt whatever a session is doing. Escape, not a kill.
    Interrupt { session: String },

    /// End a session for good, on the host.
    ///
    /// Separate from *detaching*, which the bridge cannot do and does not want
    /// to: dropping a view is a local, per-machine act with no meaning here.
    Close { session: String },

    /// Every Claude conversation on this host, running or not.
    ///
    /// **This is the one Redstone cannot get any other way.** rmux's rail shows
    /// the sessions *this operator opened from this machine*; the host holds
    /// every conversation anyone ever had on it, including the ones started from
    /// a laptop that is now closed.
    ListConversations {
        /// Newest first, bounded. A host that has been in use for a year holds
        /// thousands, and an unbounded listing is a response nobody can render.
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        /// Restrict to conversations whose recorded `cwd` is under this folder.
        #[serde(skip_serializing_if = "Option::is_none")]
        folder: Option<String>,
    },

    /// Read one conversation back.
    ReadConversation {
        /// Claude's own session id, as [`Conversation::id`] reports it.
        ///
        /// **Named `conversation`, not `id`, and that is load-bearing.** The
        /// envelope flattens a request into the same object as its `id`, so a
        /// field called `id` here produces `{"id":3,…,"id":"conv-abc"}` — two
        /// keys of the same name, which does not round-trip *at all*: the frame
        /// fails to parse rather than losing one of them quietly. It was written
        /// that way first and caught only by a round-trip test.
        conversation: String,
        /// How much of the tail to read. The transcript of a working server has
        /// been measured at **228 MB**, so this is bounded by default and the
        /// answer says whether it truncated.
        #[serde(skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u64>,
    },
}

// ---------------------------------------------------------------------------
// Bridge → Redstone: responses
// ---------------------------------------------------------------------------

/// What the bridge says back.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum Response {
    Sessions { sessions: Vec<Session> },
    Host { host: HostInfo },
    Spawned { session: Session },
    Conversations { conversations: Vec<Conversation> },
    Conversation {
        messages: Vec<Message>,
        /// Whether the tail limit cut the front off. A client that renders a
        /// truncated transcript as a complete one shows a conversation that
        /// appears to begin mid-sentence.
        truncated: bool,
    },
    Ok,
    /// Typed, because a client matching on error *text* breaks the first time
    /// anyone rewords a message.
    Error {
        code: ErrorCode,
        message: String,
    },
}

/// Why a request failed, in a form a caller can branch on.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    /// No session or conversation by that name. The usual cause is a stale list.
    NotFound,
    /// The request was understood and refused — a folder outside the allowed
    /// roots, a model profile that does not exist.
    Refused,
    /// This agent build has no such method. Redstone should say which version
    /// added it rather than reporting a generic failure.
    Unsupported,
    /// Something on the host went wrong: the daemon is not answering, a file
    /// could not be read.
    Failed,
}

// ---------------------------------------------------------------------------
// Bridge → Redstone: events
// ---------------------------------------------------------------------------

/// Pushed when the host's state changes, with no request behind it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event {
    /// A session changed status.
    ///
    /// Sourced from Claude's own `~/.claude/sessions/<pid>.json`, which the
    /// agent already watches — a fact Claude reports about itself, not one
    /// inferred from its pixels. **Only changes are sent.** A client that
    /// receives one of these does not need to poll, which is the entire reason
    /// the watcher exists.
    SessionStatus {
        session: String,
        status: Status,
        /// Milliseconds since the epoch, on the **host's** clock. Named as such
        /// because comparing it to Redstone's own clock is a bug waiting to be
        /// written; use it to order a host's own events, not to age them.
        at: u64,
    },
    /// A session appeared. Includes ones rmux did not start — the operator may
    /// have opened a terminal by hand.
    SessionOpened { session: Session },
    /// It ended. `name` rather than the whole session: it is gone, and there is
    /// nothing left to describe.
    SessionClosed { session: String },
    /// The bridge is stopping. Clients should stand down rather than
    /// reconnect-loop against a host that is deliberately going away.
    GoingAway {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// What a session is doing.
///
/// Claude's own vocabulary, carried through unchanged rather than remapped —
/// a translation layer here would be a second definition of "busy" to keep in
/// step with a program that ships weekly.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Working.
    Busy,
    /// Waiting for the operator to answer something. **The one that deserves
    /// attention**: a session in this state has stopped and will not proceed.
    Waiting,
    /// Idle, ready for input.
    Idle,
    /// At a shell rather than in Claude.
    Shell,
    /// Nothing has reported yet. Distinct from `Idle`, which is a measurement —
    /// reporting "idle" for a session never observed is inventing one.
    #[default]
    Unknown,
}

// ---------------------------------------------------------------------------
// The nouns
// ---------------------------------------------------------------------------

/// A live session on the host.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// The agent's own key — what [`Request::Send`] and [`Request::Close`] take.
    pub name: String,
    /// A shell or a conversation, without anyone parsing the name for a prefix.
    pub kind: Kind,
    /// The working directory, when the daemon was told one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The operator's name for it, from `~/.rmux/aliases.json`. Host-side on
    /// purpose, so every client — rmux on two laptops, and Redstone — sees the
    /// same name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// **Claude's own session id**, which is what links a running session to its
    /// transcript on disk.
    ///
    /// Without it there is no way to read the conversation of a session you can
    /// see running, which is most of what Redstone wants to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub status: Status,
    /// The shell's pid on the host, so an operator can find it in `ps`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Seconds since it was created. **Age is what identifies a leak**: a
    /// session older than the app that started it had no view for most of its
    /// life.
    pub age_seconds: u64,
    /// Whether a client is attached right now. An unattached session is not
    /// necessarily abandoned — rmux may simply be closed — but every abandoned
    /// one is unattached.
    pub attached: bool,
}

/// Shell or conversation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Claude,
    #[default]
    Shell,
}

/// One of Claude's conversations on disk, running or not.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// Claude's session id. Matches [`Session::conversation_id`] when it is
    /// still running.
    pub id: String,
    /// The folder it was held in, as the transcript itself records it.
    ///
    /// Read from the record rather than derived from the directory name:
    /// Claude's project-directory scheme **changed between versions**, so a
    /// path reconstructed from the slug is right on one host and wrong on the
    /// next.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// First line of the first human message, for a list nobody has to open to
    /// read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Milliseconds since the epoch, host clock. Newest first is the only
    /// ordering that makes a long list usable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
    /// Bytes on disk. Redstone needs this to decide whether to ask for the
    /// whole thing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// True when a daemon on this host is running it now.
    #[serde(default)]
    pub running: bool,
}

/// One turn of a conversation.
///
/// **Never bound to Claude's schema.** Fields are picked out one at a time and
/// unknown record types are skipped, exactly as `rmux_claude::transcript` does —
/// a strict deserialise turns every Claude Code release into "the transcript is
/// empty".
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// `user`, `assistant`, or `system`.
    ///
    /// Slash-command plumbing, caveat banners and system reminders are recorded
    /// by Claude as *user* messages and dominate the tail of a long session.
    /// They are demoted to `system` here so a reader can drop them without
    /// having to know the shape of each one.
    pub role: Role,
    pub text: String,
    /// **ISO-8601, verbatim from Claude's record** — the one timestamp in this
    /// protocol that is not a millisecond epoch.
    ///
    /// That inconsistency is deliberate. Everything else here is a number
    /// because its source is a number; this one is a string in the file, and
    /// converting it would mean an offset parser in the agent that can be
    /// subtly wrong about a timezone — on a value nobody sorts by, since the
    /// records already arrive in order. Passing it through unchanged cannot be
    /// wrong, and any client can parse ISO-8601.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// The tool's name, when this entry is a tool call or its result.
    ///
    /// Kept rather than dropped: "Claude ran the tests" is frequently the most
    /// informative line in a transcript, and a reader that sees only prose
    /// cannot tell a session that did work from one that talked about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    #[default]
    User,
    Assistant,
    /// A tool Claude ran, and what it returned.
    Tool,
    /// Not the operator talking. See [`Message::role`].
    System,
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// One WebSocket message, in either direction.
///
/// Untagged, and demultiplexed on the presence of `id`, exactly like
/// `rmux_control::Message`. The ordering matters: `Request` and `Response` both
/// carry an `id`, so they are tried before `Event`, and each is distinguished by
/// its own tag field (`method` / `result`).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Frame {
    Request {
        id: u64,
        #[serde(flatten)]
        request: Request,
    },
    Response {
        id: u64,
        #[serde(flatten)]
        response: Response,
    },
    Event(Event),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            name: "claude-a1b2".into(),
            kind: Kind::Claude,
            folder: Some("/home/dev.user/api".into()),
            title: Some("billing rewrite".into()),
            conversation_id: Some("2f9c…".into()),
            status: Status::Busy,
            pid: Some(4021),
            age_seconds: 900,
            attached: false,
        }
    }

    #[test]
    fn a_request_round_trips_and_names_its_method() {
        let line = serde_json::to_string(&Frame::Request {
            id: 7,
            request: Request::Send {
                session: "claude-a1b2".into(),
                message: "run the tests".into(),
            },
        })
        .unwrap();

        assert!(line.contains(r#""method":"send""#), "{line}");
        assert!(line.contains(r#""id":7"#), "{line}");

        match serde_json::from_str::<Frame>(&line).unwrap() {
            Frame::Request { id, request: Request::Send { session, message } } => {
                assert_eq!(id, 7);
                assert_eq!(session, "claude-a1b2");
                assert_eq!(message, "run the tests");
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn an_event_is_distinguishable_from_a_response() {
        // The client demultiplexes on `id`. If an event could parse as a
        // response it would resolve a random pending request with unrelated
        // data — the failure is silent and looks like corruption.
        let event = serde_json::to_string(&Frame::Event(Event::SessionStatus {
            session: "claude-a1b2".into(),
            status: Status::Waiting,
            at: 1_782_903_431_000,
        }))
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert!(parsed.get("id").is_none(), "{event}");
        assert_eq!(parsed["event"], "sessionStatus");

        assert!(matches!(
            serde_json::from_str::<Frame>(&event).unwrap(),
            Frame::Event(Event::SessionStatus { .. })
        ));
    }

    #[test]
    fn a_response_is_matched_to_its_request_by_id() {
        let line = serde_json::to_string(&Frame::Response {
            id: 42,
            response: Response::Sessions { sessions: vec![session()] },
        })
        .unwrap();

        match serde_json::from_str::<Frame>(&line).unwrap() {
            Frame::Response { id, response: Response::Sessions { sessions } } => {
                assert_eq!(id, 42);
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].name, "claude-a1b2");
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn there_is_no_exec_method() {
        // The invariant in the module note, pinned. If someone adds a way to run
        // a command, this fails and they have to come and read why it is here.
        //
        // Checked against the *serialised* form of every variant rather than a
        // list of names, so a variant added later is covered without anyone
        // remembering to extend the test.
        let requests = [
            Request::ListSessions,
            Request::HostInfo,
            Request::Spawn {
                folder: "/tmp".into(),
                prompt: None,
                name: None,
                model_profile: None,
            },
            Request::Send { session: "s".into(), message: "m".into() },
            Request::Interrupt { session: "s".into() },
            Request::Close { session: "s".into() },
            Request::ListConversations { limit: None, folder: None },
            Request::ReadConversation { conversation: "c".into(), max_bytes: None },
        ];

        for request in &requests {
            let json = serde_json::to_string(request).unwrap();
            let method = serde_json::from_str::<serde_json::Value>(&json).unwrap()["method"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(
                !method.contains("exec")
                    && !method.contains("shell")
                    && !method.contains("command")
                    && !method.contains("run"),
                "a method that runs commands was added: {method}. \
                 Read the module note before removing this assertion."
            );
        }
    }

    #[test]
    fn every_request_round_trips_inside_the_envelope() {
        // **The envelope is flattened**, so a request's own field named `id` or
        // `method` collides with the envelope's and the frame stops parsing
        // entirely — not "loses a field", *fails*. `readConversation` shipped
        // that way for an afternoon with an `id: String` field, producing
        // `{"id":3,"method":"readConversation","id":"conv-abc"}`, and nothing
        // caught it until a round trip was actually attempted.
        //
        // Checked over every variant rather than the one that broke, so a field
        // added later is covered without anyone remembering this happened.
        let requests = [
            Request::ListSessions,
            Request::HostInfo,
            Request::Spawn {
                folder: "/tmp".into(),
                prompt: Some("go".into()),
                name: Some("n".into()),
                model_profile: Some("p".into()),
            },
            Request::Send { session: "s".into(), message: "m".into() },
            Request::Interrupt { session: "s".into() },
            Request::Close { session: "s".into() },
            Request::ListConversations { limit: Some(10), folder: Some("/tmp".into()) },
            Request::ReadConversation { conversation: "c".into(), max_bytes: Some(1) },
        ];

        for request in requests {
            let line = serde_json::to_string(&Frame::Request { id: 3, request: request.clone() })
                .unwrap();

            // Every key appears exactly once. A duplicate is invalid JSON in
            // practice even where a parser tolerates it.
            let keys: Vec<&str> = line
                .trim_matches(|c| c == '{' || c == '}')
                .split(",\"")
                .filter_map(|part| part.split(':').next())
                .map(|k| k.trim_matches('"'))
                .collect();
            let mut seen = std::collections::HashSet::new();
            for key in &keys {
                assert!(
                    seen.insert(*key),
                    "the request shadows the envelope's {key:?}: {line}",
                );
            }

            match serde_json::from_str::<Frame>(&line) {
                Ok(Frame::Request { id, request: parsed }) => {
                    assert_eq!(id, 3, "the envelope id was lost: {line}");
                    assert_eq!(parsed, request, "the request did not survive: {line}");
                }
                other => panic!("{line} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn an_unknown_method_is_a_clean_rejection_rather_than_a_panic() {
        // A newer Redstone against an older agent. This is the normal case, not
        // the exceptional one — a host may run an agent from weeks ago because
        // nothing has restarted its daemon.
        let parsed = serde_json::from_str::<Frame>(r#"{"id":1,"method":"exec","cmd":"rm -rf /"}"#);
        assert!(parsed.is_err(), "an unknown method parsed as something");
    }

    #[test]
    fn an_error_carries_a_code_a_caller_can_branch_on() {
        // Matching on message text breaks the first time anyone rewords it.
        let json = serde_json::to_string(&Response::Error {
            code: ErrorCode::Unsupported,
            message: "listConversations needs agent 0.2.20 or newer".into(),
        })
        .unwrap();

        assert!(json.contains(r#""code":"unsupported""#), "{json}");

        match serde_json::from_str::<Response>(&json).unwrap() {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::Unsupported),
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn a_session_links_to_its_transcript() {
        // Without `conversationId` there is no way to read the conversation of a
        // session you can see running, which is most of the point.
        let json = serde_json::to_string(&session()).unwrap();
        assert!(json.contains(r#""conversationId":"2f9c…""#), "{json}");
        assert!(json.contains(r#""kind":"claude""#), "{json}");
    }

    #[test]
    fn absent_optionals_do_not_appear_at_all() {
        // A client that sees `"folder": null` has to special-case it, and the
        // one that forgets renders the string "null" in a path.
        let bare = Session { name: "term-9".into(), age_seconds: 3, ..Default::default() };
        let json = serde_json::to_string(&bare).unwrap();

        assert!(!json.contains("folder"), "{json}");
        assert!(!json.contains("title"), "{json}");
        assert!(!json.contains("conversationId"), "{json}");
        assert!(!json.contains("pid"), "{json}");
        // …but the non-optional ones are always present, including the defaults.
        assert!(json.contains(r#""kind":"shell""#), "{json}");
        assert!(json.contains(r#""status":"unknown""#), "{json}");
        assert!(json.contains(r#""attached":false"#), "{json}");
    }

    #[test]
    fn an_unreported_status_is_unknown_rather_than_idle() {
        // Reporting "idle" for a session nothing has ever observed is inventing
        // a measurement — the rule the whole status watcher exists to keep.
        assert_eq!(Status::default(), Status::Unknown);

        // And a status this build has never heard of must not fail the frame.
        let parsed = serde_json::from_str::<Status>(r#""compacting""#);
        assert!(parsed.is_err(), "unknown statuses need handling at the call site");
    }

    #[test]
    fn a_truncated_transcript_says_so() {
        // A client that renders a truncated transcript as a complete one shows a
        // conversation that appears to begin mid-sentence.
        let json = serde_json::to_string(&Response::Conversation {
            messages: vec![Message {
                role: Role::Assistant,
                text: "…and that is why the socket name carries the build.".into(),
                at: Some("2026-08-16T09:58:11.000Z".into()),
                tool: None,
            }],
            truncated: true,
        })
        .unwrap();

        assert!(json.contains(r#""truncated":true"#), "{json}");
        assert!(json.contains(r#""role":"assistant""#), "{json}");
    }

    #[test]
    fn the_greeting_carries_both_versions_so_a_mismatch_is_visible() {
        // rmux and Redstone ship separately and will drift.
        let hello = Hello {
            protocol: VERSION,
            agent_version: "0.2.20".into(),
            host: HostInfo {
                hostname: "build-box".into(),
                user: "dev.user".into(),
                os: "linux".into(),
                home: Some("/home/dev.user".into()),
            },
        };
        let json = serde_json::to_string(&hello).unwrap();

        assert!(json.contains(r#""protocol":1"#), "{json}");
        assert!(json.contains(r#""agentVersion":"0.2.20""#), "{json}");
        // The account is the ceiling on what the connection can do, so it is
        // always sent rather than being optional.
        assert!(json.contains(r#""user":"dev.user""#), "{json}");
    }

    #[test]
    fn the_token_is_never_part_of_a_frame() {
        // It travels in the `Authorization` header at connect time. A token in
        // the first frame means parsing input from anyone who can reach the URL,
        // and it lands in every log that records a message body.
        let hello = serde_json::to_string(&Hello {
            protocol: VERSION,
            agent_version: "0.2.20".into(),
            host: HostInfo::default(),
        })
        .unwrap();

        assert!(!hello.contains("token"), "{hello}");
    }

    #[test]
    fn a_spawn_cannot_ask_for_skipped_permissions() {
        // `--dangerously-skip-permissions` is a judgement about one piece of
        // work on one machine, made at the moment of starting it. There is
        // deliberately no field for it here, so a remote caller cannot launch a
        // session with the permission checks off.
        let json = serde_json::to_string(&Request::Spawn {
            folder: "/home/dev.user/api".into(),
            prompt: Some("fix the failing test".into()),
            name: None,
            model_profile: None,
        })
        .unwrap();

        assert!(!json.contains("dangerous"), "{json}");
        assert!(!json.contains("skipPermissions"), "{json}");
    }
}
