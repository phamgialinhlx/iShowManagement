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
    Endpoint::for_version(env!("CARGO_PKG_VERSION"))
}

#[derive(Default)]
struct Sessions {
    by_name: HashMap<String, Arc<Terminal>>,
    /// Environment applied to sessions created from now on.
    ///
    /// **In memory only, deliberately.** It holds credentials, and the whole
    /// point of delivering them over the socket is that they are never written
    /// anywhere a later reader could find them. They are re-sent on the next
    /// connection, which costs one round trip and no persistence.
    env: std::collections::BTreeMap<String, String>,
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
                    sessions.lock().env.extend(env);
                    return Ok(());
                }
                other => anyhow::bail!("expected Hello, got {other:?}"),
            }
        }
    };

    let size = TermSize { cols: hello.cols, rows: hello.rows };
    let (terminal, created) = open_or_attach(&sessions, &hello, size)?;

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
                            let _ = dead.kill();
                        }
                        return Ok(());
                    }
                    Frame::SetEnv(env) => {
                        sessions.lock().env.extend(env);
                    }
                    // A client has no business announcing these.
                    Frame::Hello(_) | Frame::Exited { .. } => {}
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
        if existing.exit_code().is_none() {
            return Ok((Arc::clone(existing), false));
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

    guard.by_name.insert(hello.session.clone(), Arc::clone(&terminal));
    Ok((terminal, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_version_stamped() {
        // A newer client talking to an older daemon would surface as corrupt
        // terminal output rather than an honest error, so they never share one.
        assert!(endpoint().unwrap().address.contains(env!("CARGO_PKG_VERSION")));
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
