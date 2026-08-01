//! The thin client that `ssh` actually executes.
//!
//! Connects to the daemon, then shovels bytes: stdin → daemon, daemon → stdout.
//! It keeps no state, so killing it — by closing the SSH connection, losing the
//! network, quitting rmux — leaves the shell untouched on the far side.
//!
//! This split is what makes sessions survive. The process holding the PTY is the
//! daemon, which nothing about the connection can reach; the process the
//! connection *can* kill holds nothing worth keeping.

use std::process::ExitCode;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::daemon;
use crate::ipc;
use crate::protocol::{Frame, Hello};

/// Attach to `session`, creating it if needed.
pub async fn attach(mut hello: Hello, start_daemon: bool) -> anyhow::Result<ExitCode> {
    let endpoint = daemon::endpoint()?;

    // Held for the whole attachment. Dropping it puts the terminal back, which
    // matters on every exit path — a terminal left raw has no echo and no line
    // editing, and the user is left with an apparently broken shell.
    let _raw = crate::tty::RawMode::enter();

    // The real terminal size beats whatever the caller guessed on the command
    // line: `ssh -tt` has already negotiated one, and it is authoritative.
    if let Some((cols, rows)) = crate::tty::window_size() {
        hello.cols = cols;
        hello.rows = rows;
    }

    let stream = match ipc::connect(&endpoint).await {
        Ok(stream) => stream,
        Err(_) if start_daemon => {
            // No daemon yet. Start one and wait — the ordinary first-connection
            // path, not an error worth reporting.
            spawn_daemon()?;
            wait_for_daemon(&endpoint).await?
        }
        Err(e) => return Err(e),
    };

    let (mut reader, mut writer) = tokio::io::split(stream);
    writer.write_all(&Frame::Hello(hello).encode()).await?;
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // Window changes have to be forwarded explicitly.
    //
    // Resizing rmux resizes the local PTY, and `ssh` passes that on to *this*
    // process's terminal — but the shell lives in the daemon, on the far side of
    // a socket that knows nothing about signals. Without this, a full-screen
    // program keeps drawing at the size it started with and the display is
    // wrong until it is restarted.
    let (resize_tx, mut resize_rx) = tokio::sync::mpsc::channel::<(u16, u16)>(4);
    #[cfg(unix)]
    tokio::spawn(async move {
        let Ok(mut winch) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        else {
            return;
        };
        while winch.recv().await.is_some() {
            if let Some(size) = crate::tty::window_size()
                && resize_tx.send(size).await.is_err()
            {
                break;
            }
        }
    });
    #[cfg(not(unix))]
    drop(resize_tx);

    // stdin → daemon, plus window changes.
    //
    // Both go down the same socket, so they are driven by one task — two tasks
    // would need to share the writer behind a lock, and that lock would sit on
    // the keystroke path.
    let to_daemon = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        // Reused: keystrokes are small but frequent, and this is the path whose
        // latency the user feels directly.
        let mut frame = Vec::with_capacity(8 * 1024 + 5);

        loop {
            tokio::select! {
                // Biased towards input: a burst of resize events during a window
                // drag must never starve typing.
                biased;

                read = stdin.read(&mut chunk) => {
                    let Ok(n) = read else { break };
                    if n == 0 {
                        break;
                    }
                    crate::protocol::encode_data_into(&mut frame, &chunk[..n]);
                    if writer.write_all(&frame).await.is_err() {
                        break;
                    }
                }

                Some((cols, rows)) = resize_rx.recv() => {
                    if writer.write_all(&Frame::Resize { cols, rows }.encode()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // daemon → stdout.
    let mut buf = Vec::new();
    let mut code = 0;
    'outer: loop {
        let mut chunk = [0u8; 8192];
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);

        while let Some((frame, used)) = Frame::decode(&buf)? {
            buf.drain(..used);
            match frame {
                Frame::Data(bytes) => {
                    stdout.write_all(&bytes).await?;
                    // Flushed per frame: buffering here would add latency to the
                    // one path that must feel instant.
                    stdout.flush().await?;
                }
                Frame::Exited { code: exit } => {
                    code = exit;
                    break 'outer;
                }
                // The daemon never sends these to a client.
                Frame::Hello(_)
                | Frame::Resize { .. }
                | Frame::Kill { .. }
                | Frame::SetEnv(_) => {}
            }
        }
    }

    to_daemon.abort();
    Ok(if code == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Start a detached daemon.
///
/// `setsid` puts it in its own session so it has no controlling terminal —
/// without that it would receive SIGHUP when the SSH connection closes and die
/// with exactly the shells it was supposed to protect.
fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid() is async-signal-safe and is the standard way to
        // detach a daemon from its controlling terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    cmd.spawn()?;
    Ok(())
}

/// Wait for the daemon to start listening.
async fn wait_for_daemon(endpoint: &ipc::Endpoint) -> anyhow::Result<ipc::Stream> {
    // Polled rather than slept: the daemon is usually ready in a few
    // milliseconds, and a fixed delay would add that latency to every attach.
    for _ in 0..200 {
        if let Ok(stream) = ipc::connect(endpoint).await {
            return Ok(stream);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    anyhow::bail!("the rmux agent did not start listening at {}", endpoint.address)
}

/// End a session for good.
///
/// Connects only if a daemon is already running — if none is, there is nothing
/// to kill, and starting one just to tell it to do nothing would be absurd.
pub async fn kill(session: &str) -> anyhow::Result<()> {
    let endpoint = daemon::endpoint()?;
    let Ok(mut stream) = ipc::connect(&endpoint).await else {
        return Ok(());
    };

    // The daemon expects a Hello first; this one only carries the name, and the
    // Kill that follows it is what actually does the work.
    let hello = Hello {
        session: session.to_owned(),
        cwd: None,
        program: None,
        args: Vec::new(),
        login_command: None,
        env: Default::default(),
        cols: 80,
        rows: 24,
    };
    stream.write_all(&Frame::Hello(hello).encode()).await?;
    stream
        .write_all(&Frame::Kill { session: session.to_owned() }.encode())
        .await?;
    stream.flush().await?;
    Ok(())
}

/// Hand the daemon environment for future sessions, read from **stdin**.
///
/// Stdin and not arguments: `ps` exposes another user's command line to the
/// whole machine, so a token passed as a flag is disclosed to every account on
/// the host. Reading it from a pipe keeps it between the two processes.
pub async fn set_env(pairs: std::collections::BTreeMap<String, String>) -> anyhow::Result<()> {
    let endpoint = daemon::endpoint()?;

    let mut stream = match ipc::connect(&endpoint).await {
        Ok(stream) => stream,
        Err(_) => {
            // Nothing running yet — start one, so the environment is in place
            // before the first session is created rather than after it.
            spawn_daemon()?;
            wait_for_daemon(&endpoint).await?
        }
    };

    stream.write_all(&Frame::SetEnv(pairs).encode()).await?;
    stream.flush().await?;
    Ok(())
}
