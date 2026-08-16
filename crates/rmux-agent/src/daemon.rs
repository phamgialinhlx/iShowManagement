//! The session host.
//!
//! Owns a PTY per named session and keeps it running after every client has gone
//! away. Attaching replays recent output and then streams live; detaching does
//! nothing to the shell.
//!
//! **It performs no terminal emulation.** Bytes from the shell reach the client
//! exactly as the shell wrote them. That is what makes scrolling, selection and
//! the cursor behave like a normal terminal instead of like a multiplexer's
//! reimplementation of one.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_term::{TermSize, Terminal, TerminalEvent};
use rmux_transport::{CommandSpec, LocalTarget, Target, Tty, local::terminal_env};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::ipc::{self, Endpoint};
use crate::protocol::{Frame, Hello, encode_data_into};

/// Where the daemon listens, on whichever platform this is.
pub fn endpoint() -> anyhow::Result<Endpoint> {
    Endpoint::current(env!("CARGO_PKG_VERSION"))
}

/// A session, plus what the daemon knows about it that the terminal does not.
///
/// The extra fields exist to make a *leak* identifiable, not merely to list
/// things. A name and a pid say what is running; age and attachment say whether
/// anyone still wants it — which is the actual question when a shell has been
/// alive for six weeks and its tab is long gone.
struct Session {
    terminal: Arc<Terminal>,
    started: std::time::Instant,
    /// What it was launched to run, when the client said. Distinguishes a plain
    /// shell from a Claude conversation without parsing the session name, which
    /// is rmux's convention and not the agent's business.
    command: Option<String>,
    /// How many clients are attached **right now**.
    ///
    /// A count rather than a flag: reattaching before the old connection has
    /// finished tearing down is normal, and a flag would be cleared by the
    /// departing client after the arriving one set it — leaving a live session
    /// reported as abandoned.
    attached: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Default)]
struct Sessions {
    by_name: HashMap<String, Session>,
    /// Environment applied to sessions created from now on.
    ///
    /// **In memory only, deliberately.** It holds credentials, and the whole
    /// point of delivering them over the socket is that they are never written
    /// anywhere a later reader could find them. They are re-sent on the next
    /// connection, which costs one round trip and no persistence.
    env: std::collections::BTreeMap<String, String>,
}

/// Merge a `SetEnv` frame, treating an empty value as a **removal**.
///
/// Merging alone is right for adding an account and wrong for switching model
/// profiles: moving from a profile that set `ANTHROPIC_BASE_URL` back to
/// Anthropic proper would leave the old URL in place, and the session would
/// keep talking to the previous provider while the client said otherwise. There
/// is no legitimate empty value among the variables sent this way — an empty
/// base URL or token is indistinguishable from an unset one to Claude Code — so
/// empty is free to mean "unset", and it keeps the wire format unchanged.
fn merge_env(
    into: &mut std::collections::BTreeMap<String, String>,
    from: std::collections::BTreeMap<String, String>,
) {
    for (key, value) in from {
        if value.is_empty() {
            into.remove(&key);
        } else {
            into.insert(key, value);
        }
    }
}

/// Run the daemon until the socket is closed.
pub async fn serve() -> anyhow::Result<()> {
    let endpoint = endpoint()?;
    let mut listener = ipc::Listener::bind(&endpoint).await?;

    let sessions = Arc::new(Mutex::new(Sessions::default()));
    tracing::info!(address = %endpoint.address, "rmux agent listening");

    loop {
        let stream = listener.accept().await?;
        let sessions = Arc::clone(&sessions);
        tokio::spawn(async move {
            if let Err(e) = handle(stream, sessions).await {
                tracing::debug!(error = %e, "client disconnected");
            }
        });
    }
}

/// Holds the attached count up for as long as a client is connected.
struct AttachGuard(Arc<std::sync::atomic::AtomicUsize>);

impl AttachGuard {
    fn hold(count: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(count)
    }
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Serve one attached client.
///
/// Generic over the stream so the same logic serves a Unix socket and a Windows
/// named pipe, which are different types with the same behaviour.
async fn handle<S>(stream: S, sessions: Arc<Mutex<Sessions>>) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);

    // --- handshake ----------------------------------------------------------
    let mut buf = Vec::new();
    let hello = loop {
        let mut chunk = [0u8; 4096];
        let n = reader.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "client closed before saying hello");
        buf.extend_from_slice(&chunk[..n]);

        if let Some((frame, used)) = Frame::decode(&buf)? {
            buf.drain(..used);
            match frame {
                Frame::Hello(hello) => break hello,
                // A client that only delivers environment never opens a session.
                Frame::SetEnv(env) => {
                    merge_env(&mut sessions.lock().env, env);
                    return Ok(());
                }
                // Answered here, during the handshake, because listing must not
                // *create* anything. Going through the Hello path to ask what
                // exists would spawn a shell named after the question.
                Frame::List => {
                    let reply = Frame::Sessions(summarise(&sessions)).encode();
                    writer.write_all(&reply).await?;
                    writer.flush().await?;
                    return Ok(());
                }
                // **Also during the handshake, and this one was a real bug.**
                //
                // `Kill` used to be handled only in the streaming loop, which
                // meant a client had to complete a full attach first. The kill
                // client sends its frames and closes immediately, so the daemon
                // attached, tried to write the scrollback replay to a socket
                // that was already gone, errored — and never read the `Kill` at
                // all. Verified on a real host: the session survived, stayed in
                // the map, and kept its shell running. Every "closed" tab was
                // still leaking the exact way closing was supposed to prevent.
                //
                // Attaching to kill was always wrong anyway: it made the
                // teardown path *create* a session when the name did not exist,
                // just to destroy it.
                Frame::Kill { session } => {
                    if let Some(dead) = sessions.lock().by_name.remove(&session) {
                        let _ = dead.terminal.kill();
                    }
                    return Ok(());
                }
                // **Here for the same reason `Kill` is**: the caller sends one
                // frame and closes, so anything that attaches first writes a
                // scrollback replay into a socket that has already gone away and
                // never gets round to reading this.
                //
                // Note it looks the session up rather than going through
                // `open_or_attach`: a one-shot write must never *create*. The
                // caller is remote and working from a list it fetched some time
                // ago, so a name that no longer exists is the ordinary case, and
                // answering it by spawning a shell nobody asked for would leak
                // one on every stale send.
                Frame::Write { session, data } => {
                    let target = sessions.lock().by_name.get(&session).map(|s| s.terminal.clone());
                    let ok = match target {
                        Some(terminal) => terminal.write(&data).is_ok(),
                        None => false,
                    };
                    writer.write_all(&Frame::Ack { ok }.encode()).await?;
                    writer.flush().await?;
                    return Ok(());
                }
                other => anyhow::bail!("expected Hello, got {other:?}"),
            }
        }
    };

    let size = TermSize { cols: hello.cols, rows: hello.rows };
    let (terminal, created) = open_or_attach(&sessions, &hello, size)?;

    // Counted for exactly as long as this client is here. `AttachGuard` releases
    // it on every exit path — including the error ones, which is the whole
    // reason it is a guard: a client killed mid-stream would otherwise leave the
    // session looking permanently attached, and permanently attached is exactly
    // what a leak does not look like.
    let _attached = {
        let guard = sessions.lock();
        guard.by_name.get(&hello.session).map(|s| AttachGuard::hold(Arc::clone(&s.attached)))
    };

    // The terminal is resized to *this* client's window. With one client that is
    // simply correct; with several the last to attach wins, which is the same
    // rule every multiplexer uses.
    let _ = terminal.resize(size);

    // Replay then stream, atomically — see `Terminal::attach`. Split apart, a
    // reattaching client would miss or duplicate whatever arrived in between.
    let (backlog, mut events) = terminal.attach();
    if !backlog.is_empty() {
        let mut frame = Vec::new();
        encode_data_into(&mut frame, &backlog);
        writer.write_all(&frame).await?;
    }

    // --- shell output → client ---------------------------------------------
    let out = tokio::spawn(async move {
        // Reused across every chunk: this is the path every byte of shell output
        // takes, so allocating per chunk would be the app's busiest allocation.
        let mut frame = Vec::with_capacity(16 * 1024);

        loop {
            match events.recv().await {
                Ok(TerminalEvent::Output(chunk)) => {
                    encode_data_into(&mut frame, &chunk);
                    if writer.write_all(&frame).await.is_err() {
                        break;
                    }
                }
                Ok(TerminalEvent::Exited { code }) => {
                    let _ = writer.write_all(&Frame::Exited { code }.encode()).await;
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(chunks = n, "client fell behind; output dropped");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // --- client input → shell ----------------------------------------------
    let input = Arc::clone(&terminal);
    let result: anyhow::Result<()> = async {
        loop {
            // Drain everything buffered before reading again: terminal input
            // arrives coalesced, and one frame per read would add latency to
            // every keystroke burst.
            while let Some((frame, used)) = Frame::decode(&buf)? {
                buf.drain(..used);
                match frame {
                    Frame::Data(bytes) => input.write(&bytes)?,
                    Frame::Resize { cols, rows } => {
                        let _ = input.resize(TermSize { cols, rows });
                    }
                    Frame::Kill { session } => {
                        // Explicit end-of-life: the tab was closed, so the shell
                        // should die rather than linger unreachable.
                        if let Some(dead) = sessions.lock().by_name.remove(&session) {
                            let _ = dead.terminal.kill();
                        }
                        return Ok(());
                    }
                    Frame::SetEnv(env) => {
                        merge_env(&mut sessions.lock().env, env);
                    }
                    // A client has no business announcing these.
                    Frame::Hello(_)
                    | Frame::Exited { .. }
                    | Frame::Sessions(_)
                    | Frame::Ack { .. } => {}
                    // Answered only during the handshake, where they cannot be
                    // confused with traffic for an open session. `Write` in
                    // particular: an attached client already has a `Data` path to
                    // its own session, so honouring a one-shot write here would
                    // be a second way to do the same thing — pointed at a session
                    // that need not be the one this connection holds.
                    Frame::List | Frame::Write { .. } => {}
                }
            }

            let mut chunk = [0u8; 8192];
            let n = reader.read(&mut chunk).await?;
            if n == 0 {
                // The client detached. The shell keeps running — that is the
                // whole reason this daemon exists.
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }
    .await;

    out.abort();

    if created {
        tracing::info!(session = %hello.session, "session created");
    }
    result
}

/// Find the named session, or start it.
fn open_or_attach(
    sessions: &Arc<Mutex<Sessions>>,
    hello: &Hello,
    size: TermSize,
) -> anyhow::Result<(Arc<Terminal>, bool)> {
    let mut guard = sessions.lock();

    if let Some(existing) = guard.by_name.get(&hello.session) {
        // A shell that has exited should not be reattached to — start a new one
        // under the same name rather than handing back a dead terminal.
        if existing.terminal.exit_code().is_none() {
            return Ok((Arc::clone(&existing.terminal), false));
        }
        guard.by_name.remove(&hello.session);
    }

    let mut spec = match (&hello.program, &hello.login_command) {
        (Some(program), _) => CommandSpec::new(program).args(hello.args.clone()),
        // `-l -c` and not plain `-c`: the point is the login shell's PATH.
        (None, Some(line)) => CommandSpec::login_shell().arg("-c").arg(line.clone()),
        (None, None) => CommandSpec::login_shell(),
    }
    .tty(Tty::Allocate);

    for (key, value) in terminal_env() {
        spec = spec.env(key, value);
    }
    // Then anything delivered over the socket, then anything this Hello carries.
    // The daemon spawns locally, so these become real `envp` entries and never
    // appear in an argument list.
    for (key, value) in &guard.env {
        spec = spec.env(key.clone(), value.clone());
    }
    for (key, value) in &hello.env {
        spec = spec.env(key.clone(), value.clone());
    }

    let command = LocalTarget::new().build_command(&spec)?;
    let cwd = hello.cwd.as_deref().map(camino::Utf8Path::new);
    let terminal = Arc::new(Terminal::spawn(&command, cwd, size)?);

    guard.by_name.insert(
        hello.session.clone(),
        Session {
            terminal: Arc::clone(&terminal),
            started: std::time::Instant::now(),
            // The login command is the informative one — it is what carries
            // `claude --resume …`. A bare program name says only "a shell".
            command: hello
                .login_command
                .clone()
                .or_else(|| hello.program.clone()),
            attached: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        },
    );
    Ok((terminal, true))
}

/// Everything the daemon is running, newest last.
///
/// Sorted oldest-first on purpose: age is what identifies a leak, so the ones
/// worth looking at are the ones at the top.
fn summarise(sessions: &Arc<Mutex<Sessions>>) -> Vec<crate::protocol::SessionSummary> {
    use std::sync::atomic::Ordering;

    let guard = sessions.lock();
    let mut out: Vec<_> = guard
        .by_name
        .iter()
        // A session whose shell has exited is not running, whatever the map
        // still says — reporting it would send the operator to kill a pid that
        // the OS may have reused.
        .filter(|(_, session)| session.terminal.exit_code().is_none())
        .map(|(name, session)| crate::protocol::SessionSummary {
            name: name.clone(),
            pid: session.terminal.pid(),
            age_seconds: session.started.elapsed().as_secs(),
            attached: session.attached.load(Ordering::Relaxed) > 0,
            command: session.command.clone(),
        })
        .collect();

    out.sort_by_key(|s| std::cmp::Reverse(s.age_seconds));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_value_unsets_rather_than_setting_an_empty_string() {
        // The failure this prevents, which is silent and expensive: switch from
        // a custom provider back to Anthropic, and keep talking to the custom
        // provider because a merge cannot express a removal. The session would
        // be billed to, and its prompts sent to, whoever the last profile named.
        let mut env: std::collections::BTreeMap<String, String> =
            [("ANTHROPIC_BASE_URL", "https://vendor.test"), ("ANTHROPIC_AUTH_TOKEN", "tok")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();

        merge_env(
            &mut env,
            [("ANTHROPIC_BASE_URL", ""), ("ANTHROPIC_AUTH_TOKEN", ""), ("ANTHROPIC_MODEL", "opus")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        );

        // Removed, not set to "" — an empty `ANTHROPIC_BASE_URL` in the real
        // environment is not reliably the same as an absent one.
        assert!(!env.contains_key("ANTHROPIC_BASE_URL"), "{env:?}");
        assert!(!env.contains_key("ANTHROPIC_AUTH_TOKEN"), "{env:?}");
        assert_eq!(env.get("ANTHROPIC_MODEL").map(String::as_str), Some("opus"));
    }

    #[test]
    fn setting_env_still_merges_with_what_is_already_there() {
        // The account token and a model profile are delivered by two separate
        // calls; if the second replaced the map, whichever arrived first would
        // be lost and the session would start signed out.
        let mut env: std::collections::BTreeMap<String, String> =
            [("CLAUDE_CODE_OAUTH_TOKEN".to_owned(), "oat".to_owned())].into_iter().collect();

        merge_env(
            &mut env,
            [("ANTHROPIC_MODEL".to_owned(), "opus".to_owned())].into_iter().collect(),
        );

        assert_eq!(env.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str), Some("oat"));
        assert_eq!(env.get("ANTHROPIC_MODEL").map(String::as_str), Some("opus"));
    }

    #[test]
    fn the_endpoint_is_version_stamped() {
        // A newer client talking to an older daemon would surface as corrupt
        // terminal output rather than an honest error, so they never share one.
        assert!(endpoint().unwrap().address.contains(env!("CARGO_PKG_VERSION")));
    }

    /// Killing must work without attaching first.
    ///
    /// The bug this pins was real and shipped: `Kill` was handled only in the
    /// streaming loop, so a client had to complete a full attach before it
    /// counted. The kill client closes its connection immediately, so the
    /// daemon attached, tried to write the scrollback replay to a socket that
    /// was already gone, errored — and never read the `Kill`. Every closed tab
    /// kept its shell, which is the precise failure closing exists to prevent,
    /// and it looked fixed because the *client* reported success.
    ///
    /// Driven through `handle` rather than by calling a helper, because the
    /// handshake is where the bug lived: a test that reaches past it proves
    /// nothing.
    #[tokio::test]
    async fn a_kill_needs_no_hello_and_no_attach() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let size = TermSize { cols: 80, rows: 24 };

        let hello = Hello {
            session: "doomed".into(),
            cwd: None,
            program: Some("sh".into()),
            args: vec!["-c".into(), "sleep 30".into()],
            login_command: None,
            env: Default::default(),
            cols: 80,
            rows: 24,
        };
        let (terminal, _) = open_or_attach(&sessions, &hello, size).unwrap();
        assert!(terminal.exit_code().is_none(), "the shell should be running");

        // A client that says only "kill this" and hangs up, which is exactly
        // what `attach::kill` does.
        let (client, server) = tokio::io::duplex(1024);
        let (mut client_read, mut client_write) = tokio::io::split(client);
        client_write
            .write_all(&Frame::Kill { session: "doomed".into() }.encode())
            .await
            .unwrap();
        drop(client_write);

        handle(server, Arc::clone(&sessions)).await.unwrap();

        assert!(
            !sessions.lock().by_name.contains_key("doomed"),
            "the session must be gone from the daemon's map"
        );

        // And the process itself, not merely the bookkeeping. `SIGHUP` is what
        // portable-pty sends; asserting only on the map would have passed for
        // the broken version too.
        for _ in 0..50 {
            if terminal.exit_code().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(terminal.exit_code().is_some(), "the shell must actually have died");

        // Nothing is sent back; the client learns it worked from the close.
        let mut sink = [0u8; 1];
        let _ = client_read.read(&mut sink).await;
    }

    /// A one-shot write reaches a live session, and creates nothing when it misses.
    ///
    /// The bridge's whole send path. Both halves matter and the second one more:
    /// a remote caller works from a session list it fetched some time ago, so
    /// writing to a name that has since ended is *routine*. If that spawned a
    /// shell — which going through `Hello` would — every stale send would leak
    /// one, unreachable, on a machine nobody is looking at.
    #[tokio::test]
    async fn a_write_reaches_a_live_session_and_creates_nothing_when_it_misses() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let size = TermSize { cols: 80, rows: 24 };

        // `cat` echoes what it is given, so the write is observable rather than
        // merely reported as having happened.
        let hello = Hello {
            session: "live".into(),
            cwd: None,
            program: Some("cat".into()),
            args: vec![],
            login_command: None,
            env: Default::default(),
            cols: 80,
            rows: 24,
        };
        let (terminal, _) = open_or_attach(&sessions, &hello, size).unwrap();
        let (_backlog, mut events) = terminal.attach();

        // Exactly what `attach::write` sends: one frame, then the socket closes.
        async fn write_once(
            sessions: &Arc<Mutex<Sessions>>,
            session: &str,
            data: &[u8],
        ) -> bool {
            let (client, server) = tokio::io::duplex(1024);
            let (mut client_read, mut client_write) = tokio::io::split(client);
            client_write
                .write_all(
                    &Frame::Write { session: session.into(), data: data.to_vec() }.encode(),
                )
                .await
                .unwrap();

            handle(server, Arc::clone(sessions)).await.unwrap();

            let mut buf = Vec::new();
            let mut chunk = [0u8; 64];
            loop {
                if let Some((Frame::Ack { ok }, _)) = Frame::decode(&buf).unwrap() {
                    return ok;
                }
                let n = client_read.read(&mut chunk).await.unwrap();
                assert!(n > 0, "the daemon closed without acknowledging");
                buf.extend_from_slice(&chunk[..n]);
            }
        }

        assert!(write_once(&sessions, "live", b"ping\n").await, "a live session must ack true");

        // The bytes actually reached the child, not just the daemon's map.
        let mut saw = String::new();
        for _ in 0..50 {
            match tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await {
                Ok(Ok(crate::daemon::TerminalEvent::Output(bytes))) => {
                    saw.push_str(&String::from_utf8_lossy(&bytes));
                    if saw.contains("ping") {
                        break;
                    }
                }
                Ok(Ok(_)) => {}
                _ => {}
            }
        }
        assert!(saw.contains("ping"), "the child never saw the write; got {saw:?}");

        // …and a name nobody holds is a clean `false` that leaves the map alone.
        assert!(!write_once(&sessions, "ghost", b"hello\n").await, "a miss must ack false");
        assert!(
            !sessions.lock().by_name.contains_key("ghost"),
            "a one-shot write must never create a session"
        );
        assert_eq!(sessions.lock().by_name.len(), 1, "only the original session should exist");
    }

    /// Age ordering, and the exclusion of the dead.
    ///
    /// Oldest first because age is what identifies a leak — a shell older than
    /// the app that started it had no tab for most of its life. And a session
    /// whose shell has exited must not appear at all: its pid may already have
    /// been reused by the OS, so reporting it sends the operator to kill
    /// something else entirely.
    #[test]
    fn the_summary_leads_with_the_oldest_and_omits_the_dead() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let size = TermSize { cols: 80, rows: 24 };

        for name in ["young", "old"] {
            let hello = Hello {
                session: name.into(),
                cwd: None,
                // Long-lived, so it is still running when the summary is taken.
                program: Some("sh".into()),
                args: vec!["-c".into(), "sleep 30".into()],
                login_command: None,
                env: Default::default(),
                cols: 80,
                rows: 24,
            };
            open_or_attach(&sessions, &hello, size).unwrap();
        }
        // Backdate one so the ordering is decided by age rather than by the
        // map's iteration order, which is not stable and would make this pass
        // or fail at random.
        sessions.lock().by_name.get_mut("old").unwrap().started =
            std::time::Instant::now() - std::time::Duration::from_secs(600);

        let summary = summarise(&sessions);
        assert_eq!(summary.len(), 2, "{summary:?}");
        assert_eq!(summary[0].name, "old", "the oldest must lead: {summary:?}");
        assert!(summary[0].age_seconds >= 600);

        // Nothing is attached: these were opened, not connected to. That is
        // exactly the shape of a leak, and the field has to say so.
        assert!(summary.iter().all(|s| !s.attached), "{summary:?}");

        // A session that exits leaves the listing.
        sessions.lock().by_name.get("young").unwrap().terminal.kill().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let after = summarise(&sessions);
        assert!(
            after.iter().all(|s| s.name != "young"),
            "a dead session must not be listed: {after:?}"
        );
    }

    #[test]
    fn a_dead_session_is_replaced_rather_than_reattached() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let hello = Hello {
            session: "test".into(),
            cwd: None,
            program: Some("sh".into()),
            args: vec!["-c".into(), "exit 0".into()],
            login_command: None,
            env: Default::default(),
            cols: 80,
            rows: 24,
        };
        let size = TermSize { cols: 80, rows: 24 };

        let (first, created) = open_or_attach(&sessions, &hello, size).unwrap();
        assert!(created);

        // Let it exit.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while first.exit_code().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(first.exit_code().is_some(), "the shell should have exited");

        // Attaching again must start a fresh shell, not hand back the corpse.
        let (second, created_again) = open_or_attach(&sessions, &hello, size).unwrap();
        assert!(created_again, "a dead session should be replaced");
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn a_live_session_is_reattached_not_restarted() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let hello = Hello {
            session: "live".into(),
            cwd: None,
            program: Some("sh".into()),
            args: vec!["-c".into(), "sleep 5".into()],
            login_command: None,
            env: Default::default(),
            cols: 80,
            rows: 24,
        };
        let size = TermSize { cols: 80, rows: 24 };

        let (first, created) = open_or_attach(&sessions, &hello, size).unwrap();
        assert!(created);

        // This is the property the whole daemon exists for: the same name comes
        // back to the same running shell.
        let (second, created_again) = open_or_attach(&sessions, &hello, size).unwrap();
        assert!(!created_again);
        assert_eq!(first.id(), second.id());

        let _ = first.kill();
    }

    #[test]
    fn distinct_names_get_distinct_shells() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let size = TermSize { cols: 80, rows: 24 };
        let make = |name: &str| Hello {
            session: name.into(),
            cwd: None,
            program: Some("sh".into()),
            args: vec!["-c".into(), "sleep 5".into()],
            login_command: None,
            env: Default::default(),
            cols: 80,
            rows: 24,
        };

        let (a, _) = open_or_attach(&sessions, &make("one"), size).unwrap();
        let (b, _) = open_or_attach(&sessions, &make("two"), size).unwrap();

        assert_ne!(a.id(), b.id());
        let _ = a.kill();
        let _ = b.kill();
    }
}
