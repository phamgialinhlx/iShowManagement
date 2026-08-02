//! The wire contract between rmux and everything built on it.
//!
//! This is the piece that outlives every client, so it is defined once, here,
//! rather than growing out of whatever the first consumer happened to need.
//!
//! **Newline-delimited JSON**, not the length-framed binary the agent uses.
//! The agent's framing exists because it carries terminal bytes at volume; this
//! carries occasional control messages, and being readable from `nc` is worth
//! more here than the framing overhead is worth saving. A Node client is
//! `socket.on("data")` and `JSON.parse` — no codec to port for every language
//! that wants to talk to rmux.
//!
//! Three message kinds share the socket:
//!
//! - a **request** carries an `id`, and gets exactly one response with that `id`
//! - an **event** has no `id` and is pushed whenever rmux's state changes
//! - a **response** carries the `id` of the request it answers

use serde::{Deserialize, Serialize};

/// The protocol version, sent in the greeting.
///
/// Clients check it. rmux and rbrowse ship separately and will drift in
/// version, so "the app you have is older than the backend you connected to"
/// is a normal Tuesday rather than an exceptional case — and it must surface as
/// a clear message rather than a field that silently deserialises to its
/// default.
pub const VERSION: u32 = 1;

/// What a client asks for.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum Request {
    /// Identify yourself. Must be first; nothing else is answered before it.
    Hello {
        /// The per-run token, proving this client can read rmux's own state
        /// directory. See [`crate::server`] for why that is the check.
        token: String,
        /// For the log, so an operator can tell which app is connected.
        client: String,
    },
    /// Every session rmux currently has.
    ListSessions,
    /// The one the operator is looking at, if any.
    ActiveSession,
    /// Bring a session to the front. Lets a client drive rmux, not just follow.
    Activate { id: String },
    /// What the target is listening on.
    DiscoverPorts { session: String },
    /// Open `ssh -L port:localhost:port` for a session's target.
    ///
    /// Here rather than in each client on purpose: rmux already owns the ssh
    /// connections, and two apps independently forwarding the same port would
    /// collide on the bind — with the loser reporting a failure it cannot
    /// explain.
    ForwardPort { session: String, port: u16 },
    /// Open a SOCKS proxy onto the session's target (`ssh -D`).
    ///
    /// **This is the one that actually removes port forwarding.** A `-L` tunnel
    /// carries one port that the operator had to know; a `-D` proxy carries the
    /// whole network — every port, and with `socks5h` the far side's DNS too,
    /// so internal hostnames resolve as they do on the server.
    ///
    /// Only useful to a client that can scope a proxy to part of itself.
    /// Chromium can (per session/partition); rmux's own webview cannot, because
    /// there is only one and proxying it would proxy the app's own UI.
    OpenProxy { session: String },
    /// Hand rmux something the browser observed.
    ///
    /// **This is the direction that makes a separate browser worth having.**
    /// rbrowse can see things rmux never can — the rendered page, what the
    /// operator pointed at, what the console said — and a report turns those
    /// into something the session's Claude can be told about.
    Report {
        /// Which session this belongs to. rbrowse keeps its tabs per session,
        /// so it always knows; rmux files the report against that session and
        /// nowhere else.
        session: String,
        #[serde(flatten)]
        report: Report,
    },
}

/// What a browser can tell rmux about.
///
/// One enum rather than a request each, because they share a lifecycle: every
/// one is filed against a session, shown to the operator, and offered to that
/// session's Claude. A new kind should arrive here rather than as another
/// top-level method.
///
/// **Nothing here is trusted content.** A page chooses its own console output
/// and can put anything in the DOM, so every field below is data the operator
/// is shown — never an instruction rmux acts on, and never something typed into
/// a Claude session without the operator sending it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "report", rename_all = "camelCase")]
pub enum Report {
    /// The operator picked an element and said something about it.
    ///
    /// The one that closes the loop: point at the broken button, type what is
    /// wrong, and the session's Claude gets the selector and the note together.
    Selection {
        /// Where it happened. Always sent — a note about "this button" is
        /// useless without the page it was on.
        url: String,
        /// A CSS selector for the element, as the browser computed it.
        selector: String,
        /// The element's own text, trimmed by the client. Handy when the
        /// selector is a generated class name that means nothing to a reader.
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// What the operator typed. This is the actual message.
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// PNG of the element or the viewport, base64, no data-URI prefix.
        ///
        /// Base64 because the transport is one JSON line per message; binary
        /// would need a second channel and a correlation id for no benefit at
        /// screenshot volumes.
        #[serde(skip_serializing_if = "Option::is_none")]
        screenshot: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        viewport: Option<Viewport>,
    },
    /// A screenshot on its own, with no element behind it.
    Screenshot { url: String, png: String, #[serde(skip_serializing_if = "Option::is_none")] viewport: Option<Viewport> },
    /// Console output the page produced.
    Console { url: String, entries: Vec<ConsoleEntry> },
    /// Network activity, as a HAR document.
    ///
    /// Carried as a **string**, not as parsed JSON: HAR is a large, evolving
    /// schema owned by someone else, and binding to it would make every spec
    /// revision a deserialisation failure here. rmux stores and forwards it.
    Har { url: String, har: String },
    /// The window changed size, or was reported on connection.
    ///
    /// Worth its own kind because layout bugs are size-dependent, and "it looks
    /// wrong" without the viewport is a bug report nobody can reproduce.
    Viewport { url: String, viewport: Viewport },
}

/// CSS pixels, plus the device ratio needed to make sense of a screenshot.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    /// 2.0 on a Retina display. Without it a 2560px-wide screenshot of a
    /// 1280px viewport reads as a desktop layout when it is a laptop one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_pixel_ratio: Option<f32>,
}

/// One console line.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleEntry {
    /// `log`, `warn`, `error`, … Left as a free string on purpose: browsers add
    /// levels, and an unknown one must not fail the whole batch.
    pub level: String,
    pub text: String,
    /// Milliseconds since the epoch, from the client's clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

/// What rmux says back.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum Response {
    Hello { version: u32, app: String },
    Sessions { sessions: Vec<SessionInfo> },
    Session { session: Option<SessionInfo> },
    Ports { ports: Vec<u16> },
    Forwarded { port: u16, ok: bool, #[serde(skip_serializing_if = "Option::is_none")] error: Option<String> },
    /// Point a Chromium partition at `socks5h://127.0.0.1:{port}`.
    ///
    /// `socks5h` and not `socks5`: the `h` sends hostnames to the proxy to
    /// resolve rather than resolving them locally. Without it an internal name
    /// is looked up on *this* machine, fails, and looks like the proxy is
    /// broken when it is working perfectly.
    Proxy { port: u16 },
    Ok,
    Error { message: String },
}

/// Pushed when rmux's state changes, with no request behind it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event {
    /// The operator switched sessions.
    ///
    /// **The whole session is sent, not just its id.** A client that receives
    /// only an id has to turn round and ask for the rest, and in that gap it
    /// either renders nothing or renders the previous session's tabs under the
    /// new session's name.
    SessionActivated { session: SessionInfo },
    SessionCreated { session: SessionInfo },
    SessionRenamed { session: SessionInfo },
    SessionClosed { id: String },
    /// Open this in the browser, for this session.
    ///
    /// The other half of the proxy story. rmux knows a session's target is
    /// listening on 3000; it cannot show that page itself, because proxying its
    /// single webview would proxy its own UI. So it asks the browser to open
    /// the URL *in that session's partition*, where the SOCKS proxy from
    /// `OpenProxy` already applies and `localhost:3000` means the server's
    /// localhost — no forwarded port, no rewritten address.
    OpenUrl {
        session: String,
        url: String,
        /// Bring the browser to the front. Off for a background prefetch.
        #[serde(default)]
        focus: bool,
    },
    /// rmux is going away. Clients should stop rather than reconnect-loop.
    ShuttingDown,
}

/// A session, as a client needs to see it.
///
/// Deliberately not rmux's internal `Session`: that carries UI state — which
/// file is open, which terminal is focused — that no other app has any business
/// depending on, and every field here is one a client would have to be told
/// about again if it changed.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// Stable across restarts. This is the key a client stores its own
    /// per-session state under — rbrowse's tab sets, for instance.
    pub id: String,
    pub name: String,
    /// The ssh alias, or `None` for this machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The project root.
    pub folder: String,
}

/// One line on the wire, in either direction.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Message {
    Request { id: u64, #[serde(flatten)] request: Request },
    Response { id: u64, #[serde(flatten)] response: Response },
    Event(Event),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionInfo {
        SessionInfo {
            id: "s-1".into(),
            name: "redstone-agent".into(),
            host: Some("build-box".into()),
            folder: "/home/dev.user/redstone-agent".into(),
        }
    }

    #[test]
    fn a_request_round_trips_as_one_json_line() {
        // Newline-delimited framing only works if nothing serialises with an
        // embedded newline.
        let line = serde_json::to_string(&Message::Request {
            id: 7,
            request: Request::Activate { id: "s-1".into() },
        })
        .unwrap();

        assert!(!line.contains('\n'), "{line}");
        assert!(line.contains(r#""method":"activate""#), "{line}");
        assert!(line.contains(r#""id":7"#), "{line}");
    }

    #[test]
    fn an_event_is_distinguishable_from_a_response() {
        // The client demultiplexes on the presence of `id`: a response answers
        // something it asked for, an event arrives unprompted. If an event
        // could parse as a response the client would resolve a random pending
        // request with unrelated data.
        let event = serde_json::to_string(&Message::Event(Event::SessionActivated {
            session: session(),
        }))
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert!(parsed.get("id").is_none(), "{event}");
        assert_eq!(parsed["event"], "sessionActivated");
    }

    #[test]
    fn an_activation_event_carries_the_whole_session() {
        // Sending only an id makes every client immediately ask for the rest,
        // and render the wrong thing in the gap.
        let event = Event::SessionActivated { session: session() };
        let json = serde_json::to_string(&event).unwrap();

        assert!(json.contains("redstone-agent"), "{json}");
        assert!(json.contains("build-box"), "{json}");
        assert!(json.contains("/home/dev.user/redstone-agent"), "{json}");
    }

    #[test]
    fn a_local_session_omits_the_host_rather_than_inventing_one() {
        // `"host": "localhost"` would be a lie a client might try to ssh to.
        let local = SessionInfo { host: None, ..session() };
        let json = serde_json::to_string(&local).unwrap();
        assert!(!json.contains("host"), "{json}");
    }

    #[test]
    fn responses_are_matched_to_requests_by_id() {
        let line = serde_json::to_string(&Message::Response {
            id: 42,
            response: Response::Sessions { sessions: vec![session()] },
        })
        .unwrap();

        match serde_json::from_str::<Message>(&line).unwrap() {
            Message::Response { id, response } => {
                assert_eq!(id, 42);
                assert!(matches!(response, Response::Sessions { .. }));
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn an_unknown_method_is_an_error_rather_than_a_panic() {
        // A newer client against an older rmux. This must be a clean rejection,
        // because the alternative is the backend dying on the first message a
        // future version sends.
        let parsed = serde_json::from_str::<Message>(r#"{"id":1,"method":"teleport"}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn the_version_is_sent_so_a_mismatch_is_visible() {
        // rmux and its clients ship separately and *will* drift.
        let json = serde_json::to_string(&Response::Hello {
            version: VERSION,
            app: "rmux".into(),
        })
        .unwrap();
        assert!(json.contains(r#""version":1"#), "{json}");
    }

    #[test]
    fn a_selection_report_carries_the_note_and_where_it_came_from() {
        // The whole point of the feature: a selector on its own is unusable
        // without the page it was on and what the operator said about it.
        let json = serde_json::to_string(&Message::Request {
            id: 3,
            request: Request::Report {
                session: "s-1".into(),
                report: Report::Selection {
                    url: "http://localhost:3000/settings".into(),
                    selector: "form > button.save".into(),
                    text: Some("Save".into()),
                    note: Some("this stays disabled after a valid edit".into()),
                    screenshot: None,
                    viewport: Some(Viewport {
                        width: 1280,
                        height: 800,
                        device_pixel_ratio: Some(2.0),
                    }),
                },
            },
        })
        .unwrap();

        assert!(!json.contains('\n'), "{json}");
        assert!(json.contains(r#""method":"report""#), "{json}");
        assert!(json.contains(r#""report":"selection""#), "{json}");
        assert!(json.contains("stays disabled"), "{json}");
        assert!(json.contains("localhost:3000/settings"), "{json}");
        // Absent optionals must not appear at all — a client that sees
        // `"screenshot": null` has to special-case it.
        assert!(!json.contains("screenshot"), "{json}");
    }

    #[test]
    fn a_screenshot_survives_the_newline_framing() {
        // Base64 has no newline in it, but the framing depends on that being
        // true for the *serialised* message, which is what this asserts.
        let png = "iVBORw0KGgo=".repeat(400);
        let line = serde_json::to_string(&Message::Request {
            id: 9,
            request: Request::Report {
                session: "s-1".into(),
                report: Report::Screenshot {
                    url: "http://localhost:3000/".into(),
                    png: png.clone(),
                    viewport: None,
                },
            },
        })
        .unwrap();

        assert!(!line.contains('\n'), "a screenshot broke the framing");
        assert!(line.len() > png.len());
    }

    #[test]
    fn an_unknown_console_level_does_not_fail_the_batch() {
        // Browsers add levels. Binding to a fixed set would mean one unfamiliar
        // entry discards every other line in the same report.
        let line = r#"{"id":1,"method":"report","session":"s-1","report":"console",
            "url":"http://x/","entries":[{"level":"trace","text":"a"},{"level":"assert","text":"b"}]}"#
            .replace('\n', "");

        match serde_json::from_str::<Message>(&line).unwrap() {
            Message::Request { request: Request::Report { report: Report::Console { entries, .. }, .. }, .. } => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[1].level, "assert");
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn har_stays_a_string_rather_than_a_bound_schema() {
        // HAR is someone else's evolving spec. Parsing it here would turn every
        // revision into "the network report is empty".
        let line = r#"{"id":1,"method":"report","session":"s","report":"har",
            "url":"http://x/","har":"{\"log\":{\"version\":\"1.2\",\"unknownFutureField\":true}}"}"#
            .replace('\n', "");

        match serde_json::from_str::<Message>(&line).unwrap() {
            Message::Request { request: Request::Report { report: Report::Har { har, .. }, .. }, .. } => {
                assert!(har.contains("unknownFutureField"));
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn open_url_names_the_session_it_is_for() {
        // rbrowse keeps a partition per session — that partition is where the
        // SOCKS proxy applies, so a URL without a session cannot be opened in
        // the one place `localhost:3000` means the server's localhost.
        let json = serde_json::to_string(&Message::Event(Event::OpenUrl {
            session: "s-1".into(),
            url: "http://localhost:3000/".into(),
            focus: true,
        }))
        .unwrap();

        assert!(json.contains(r#""event":"openUrl""#), "{json}");
        assert!(json.contains(r#""session":"s-1""#), "{json}");
    }

    #[test]
    fn a_forward_failure_explains_itself() {
        // "ok: false" alone leaves a client with nothing to show. The usual
        // cause is that the local port is already taken, which the operator can
        // act on only if told.
        let json = serde_json::to_string(&Response::Forwarded {
            port: 3000,
            ok: false,
            error: Some("bind: Address already in use".into()),
        })
        .unwrap();
        assert!(json.contains("Address already in use"), "{json}");

        // …and a success carries no empty error field.
        let ok = serde_json::to_string(&Response::Forwarded { port: 3000, ok: true, error: None })
            .unwrap();
        assert!(!ok.contains("error"), "{ok}");
    }
}
