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

    // **Before creating anything, find out whether this session already exists
    // under a different build.** Our own daemon is asked first, because that is
    // the overwhelmingly common case and it costs one connect on a socket that
    // is already open. Only when it does not have the name is the install
    // directory searched — and if another build's daemon holds it, this process
    // hands over to that binary rather than starting a rival copy.
    //
    // The guard stops two builds bouncing a session between them forever.
    if start_daemon && std::env::var_os(HANDOFF_GUARD).is_none() {
        let ours = sessions_of(&endpoint).await;
        let known = ours.iter().any(|s| s.name == hello.session && s.pid.is_some());

        if !known
            && let Some(binary) = owner_of(&hello.session).await
        {
            return hand_off(&binary);
        }
    }

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
                // The daemon never sends these to an *attached* client —
                // `Sessions` is answered during the handshake of a `list`
                // connection, which never reaches this loop.
                Frame::Hello(_)
                | Frame::Resize { .. }
                | Frame::Kill { .. }
                | Frame::SetEnv(_)
                | Frame::List
                | Frame::Sessions(_)
                | Frame::Write { .. }
                | Frame::Ack { .. } => {}
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

    // The Windows counterpart of `setsid`, and it is not optional. Without it
    // the daemon inherits the handles of the shell that spawned it — which over
    // SSH means the *connection's* pipes. sshd then waits for the daemon before
    // closing the channel, so the client that started it never returns and every
    // attach appears to hang forever, on a daemon that is in fact running
    // perfectly. `DETACHED_PROCESS` also gives it no console to be killed with.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
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
/// The environment flag that stops a handoff from happening twice.
///
/// Without it two installed builds could each decide the other owns a session
/// and exec into one another forever, which on a host looks like the shell
/// simply never starting.
const HANDOFF_GUARD: &str = "RMUX_AGENT_HANDOFF";

/// Every other installed agent binary, newest first.
///
/// Reads the install directory rather than remembering anything: `provision`
/// names each build `rmux-agent-<version>-<fingerprint>`, so the directory *is*
/// the list of daemons that could exist.
fn sibling_binaries() -> Vec<std::path::PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let mine = std::env::current_exe().ok();

    let Ok(entries) = std::fs::read_dir(home.join(".rmux/bin")) else { return Vec::new() };
    let mut found: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rmux-agent-"))
                && Some(p) != mine.as_ref()
        })
        .collect();
    found.sort();
    found
}

/// Ask one specific daemon what it holds, without creating anything.
///
/// Never starts a daemon: an absent socket means that build has no daemon, and
/// spawning one to ask an empty question would resurrect a superseded binary.
async fn sessions_of(endpoint: &ipc::Endpoint) -> Vec<crate::protocol::SessionSummary> {
    let Ok(mut stream) = ipc::connect(endpoint).await else { return Vec::new() };
    if stream.write_all(&Frame::List.encode()).await.is_err() {
        return Vec::new();
    }
    let _ = stream.flush().await;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match Frame::decode(&buf) {
            Ok(Some((Frame::Sessions(sessions), _))) => return sessions,
            Ok(Some(_)) | Err(_) => return Vec::new(),
            Ok(None) => {}
        }
        match tokio::io::AsyncReadExt::read(&mut stream, &mut chunk).await {
            // A daemon too old to answer `List` closes instead. Nothing to do
            // but treat it as empty — it cannot be searched.
            Ok(0) | Err(_) => return Vec::new(),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Find the installed binary whose daemon already holds `session`.
///
/// **This is what stops an agent upgrade from duplicating live work.** The
/// daemon socket carries the binary's content fingerprint, so a rebuilt agent
/// starts its own daemon — deliberately, because upgrading must not kill work in
/// progress. The half that was missing is this one: without it the new client
/// cannot *see* the sessions the old daemon still holds, so it creates a second
/// Claude under the same name while the first keeps running, orphaned and
/// unreachable. Measured on a real host: one session name existed three times
/// across three daemons, the oldest 27 hours old and detached.
pub async fn owner_of(session: &str) -> Option<std::path::PathBuf> {
    for binary in sibling_binaries() {
        let Some(name) = binary.file_name().and_then(|n| n.to_str()) else { continue };
        let Ok(endpoint) = ipc::Endpoint::for_exe_name(crate::provision::VERSION, name) else {
            continue;
        };

        if sessions_of(&endpoint)
            .await
            .iter()
            // A dead session's name is free to reuse; only a live one owns it.
            .any(|s| s.name == session && s.pid.is_some())
        {
            return Some(binary);
        }
    }
    None
}

/// Replace this process with the build that owns the session.
///
/// `exec`, not spawn: this process *is* the far end of `ssh -tt`, so it owns the
/// pty, stdin and stdout. Spawning a child and proxying would put a second hop
/// on every keystroke and leave two processes to kill; replacing the image hands
/// the terminal over intact and keeps the pipeline exactly as long as before.
///
/// Falls through to normal behaviour on any failure — a session started twice is
/// bad, but a session that cannot start at all is worse.
fn hand_off(binary: &std::path::Path) -> anyhow::Result<ExitCode> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        let args: Vec<String> = std::env::args().skip(1).collect();
        let error = std::process::Command::new(binary)
            .args(&args)
            .env(HANDOFF_GUARD, "1")
            .exec();
        // `exec` only returns on failure.
        anyhow::bail!("could not hand off to {}: {error}", binary.display());
    }

    #[cfg(not(unix))]
    {
        // Windows has no `exec`, so the older build runs as a child and this
        // process becomes a pass-through for its lifetime. The pty is inherited
        // rather than proxied — the child gets this process's own stdin, stdout
        // and console — so the only cost against the Unix path is one extra
        // process sitting in the tree, not a second hop on every keystroke.
        let args: Vec<String> = std::env::args().skip(1).collect();
        let status = std::process::Command::new(binary)
            .args(&args)
            .env(HANDOFF_GUARD, "1")
            .status()?;

        Ok(match status.code() {
            Some(0) => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        })
    }
}

/// Ask the daemon what it is running.
///
/// Returns an empty list when there is no daemon at all, which is the ordinary
/// answer on a host rmux has never touched — not an error to report.
pub async fn list() -> anyhow::Result<Vec<crate::protocol::SessionSummary>> {
    let endpoint = daemon::endpoint()?;

    // **Every daemon, not just this build's.** Listing is what makes a leak
    // findable at all, and the leaks that matter most are exactly the ones an
    // upgrade left behind under a superseded binary — invisible to a listing
    // that only asks the current one. Reported oldest-first, because age is what
    // separates "left behind" from "rmux is merely closed".
    let mut all = sessions_of(&endpoint).await;
    for binary in sibling_binaries() {
        let Some(name) = binary.file_name().and_then(|n| n.to_str()) else { continue };
        let Ok(other) = ipc::Endpoint::for_exe_name(crate::provision::VERSION, name) else {
            continue;
        };
        all.extend(sessions_of(&other).await);
    }
    if !all.is_empty() {
        all.sort_by_key(|s| std::cmp::Reverse(s.age_seconds));
        return Ok(all);
    }

    let Ok(mut stream) = ipc::connect(&endpoint).await else {
        return Ok(Vec::new());
    };

    // `List` is sent *instead of* a Hello, not after one. Going through the
    // Hello path to ask what exists would spawn a shell named after the
    // question — the enumeration would create what it was counting.
    stream.write_all(&Frame::List.encode()).await?;
    stream.flush().await?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some((frame, _)) = Frame::decode(&buf)? {
            return match frame {
                Frame::Sessions(sessions) => Ok(sessions),
                other => anyhow::bail!("expected a session list, got {other:?}"),
            };
        }

        let n = tokio::io::AsyncReadExt::read(&mut stream, &mut chunk).await?;
        // A daemon from before this frame existed closes rather than answering.
        anyhow::ensure!(n > 0, "this host's agent is too old to list sessions");
        buf.extend_from_slice(&chunk[..n]);
    }
}

pub async fn kill(session: &str) -> anyhow::Result<()> {
    // **Kill it wherever it actually lives.** A session belongs to the daemon of
    // the build that created it, so after an agent upgrade the name the client
    // is closing is held by a *previous* daemon. Sending `Kill` only to our own
    // would report success and leave the shell running with nothing able to
    // reach it — a leak created by the very act of tidying up.
    let mut endpoint = daemon::endpoint()?;
    let ours = sessions_of(&endpoint).await;
    if !ours.iter().any(|s| s.name == session)
        && let Some(binary) = owner_of(session).await
        && let Some(name) = binary.file_name().and_then(|n| n.to_str())
        && let Ok(other) = ipc::Endpoint::for_exe_name(crate::provision::VERSION, name)
    {
        endpoint = other;
    }

    let Ok(mut stream) = ipc::connect(&endpoint).await else {
        return Ok(());
    };

    // `Kill` alone, with **no Hello in front of it**.
    //
    // It used to send a Hello first, because the daemon only handled `Kill`
    // once a client had fully attached. That was a real leak: this function
    // closes the connection immediately, so the daemon attached, tried to write
    // the scrollback replay to a socket that was already gone, errored, and
    // never read the `Kill`. Every closed tab kept its shell — the exact failure
    // closing exists to prevent. Verified against a real host.
    //
    // A Hello was also wrong on its own terms: it *creates* the session when the
    // name is unknown, so asking to kill something that had already gone would
    // spawn a shell in order to destroy it.
    stream
        .write_all(&Frame::Kill { session: session.to_owned() }.encode())
        .await?;
    stream.flush().await?;

    // Wait for the far end to close before dropping the stream. Without this the
    // socket can be torn down before the daemon has read the frame, which is the
    // same race in a smaller form.
    let mut sink = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut sink).await;
    Ok(())
}

/// Create a session and let go of it, without ever attaching a terminal.
///
/// The bridge's spawn path. Redstone asks for a Claude in a folder; there is no
/// operator, no window and no pty on this side of the request, so the ordinary
/// [`attach`] — which enters raw mode, negotiates a size from the tty and pumps
/// stdin — is entirely the wrong shape.
///
/// **Dropping the connection is what detaches**, and that is the daemon's own
/// design rather than a trick: a session outliving its client is the whole
/// reason the daemon exists. So this connects, says `Hello` (which creates), and
/// closes.
///
/// A daemon is started if none is running, because the *first* thing Redstone
/// ever asks a freshly enrolled host to do will find nothing there.
pub async fn spawn_detached(
    session: &str,
    cwd: &str,
    login_command: &str,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    // A name that already exists would be *reattached* rather than created, and
    // the caller would be handed someone else's conversation as though it were
    // the new one it asked for.
    let existing = list().await.unwrap_or_default();
    anyhow::ensure!(
        !existing.iter().any(|s| s.name == session),
        "a session called {session} already exists on this host",
    );

    let endpoint = daemon::endpoint()?;
    let mut stream = match ipc::connect(&endpoint).await {
        Ok(stream) => stream,
        Err(_) => {
            spawn_daemon()?;
            wait_for_daemon(&endpoint).await?
        }
    };

    let hello = Hello {
        session: session.to_owned(),
        cwd: Some(cwd.to_owned()),
        program: None,
        args: Vec::new(),
        // A **login** shell, like every other Claude launch: `claude` is
        // installed by a version manager whose PATH exists only there, so
        // spawning the binary directly gives "command not found" on a host where
        // it plainly works when typed.
        login_command: Some(login_command.to_owned()),
        env: Default::default(),
        cols,
        rows,
    };
    stream.write_all(&Frame::Hello(hello).encode()).await?;
    stream.flush().await?;

    // Read one chunk before letting go. The daemon replies with the scrollback
    // replay, and waiting for the first byte of it is how we know the session was
    // actually created rather than merely requested — closing immediately would
    // race the daemon's own accept, exactly as `kill` used to.
    let mut sink = [0u8; 1];
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::io::AsyncReadExt::read(&mut stream, &mut sink),
    )
    .await;

    Ok(())
}

/// Type bytes into a session that already exists, without attaching to it.
///
/// Returns `false` when no daemon on this host holds that name — which for a
/// remote caller is the ordinary outcome, not an exceptional one: it is working
/// from a list it fetched some seconds ago, and a session can end in between.
/// Reporting that honestly is the difference between Redstone saying "sent" and
/// saying "that conversation has finished".
///
/// **Resolved across every daemon, exactly like [`kill`].** A session belongs to
/// the build that created it, so after an upgrade the name being written to is
/// held by a *previous* daemon. Asking only our own would answer `false` for a
/// session that is plainly running — and the caller would report it gone.
pub async fn write(session: &str, data: &[u8]) -> anyhow::Result<bool> {
    let mut endpoint = daemon::endpoint()?;
    let ours = sessions_of(&endpoint).await;
    if !ours.iter().any(|s| s.name == session)
        && let Some(binary) = owner_of(session).await
        && let Some(name) = binary.file_name().and_then(|n| n.to_str())
        && let Ok(other) = ipc::Endpoint::for_exe_name(crate::provision::VERSION, name)
    {
        endpoint = other;
    }

    // No daemon at all means no session, which is a clean `false` rather than an
    // error. **And emphatically not a reason to start one**: a daemon spawned to
    // service a write has nothing to write to, so it would leave a fresh, empty
    // daemon behind on every send to a host whose sessions have all ended.
    let Ok(mut stream) = ipc::connect(&endpoint).await else {
        return Ok(false);
    };

    // `Write` alone, with no Hello — the same rule as `Kill`, for the same
    // reason. A Hello would *create* the session when the name is unknown, so a
    // stale name from a remote caller would silently spawn a shell instead of
    // reporting that the conversation had ended.
    stream
        .write_all(&Frame::Write { session: session.to_owned(), data: data.to_vec() }.encode())
        .await?;
    stream.flush().await?;

    // Unlike `kill`, this one has an answer worth waiting for.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64];
    loop {
        if let Some((frame, _)) = Frame::decode(&buf)? {
            return match frame {
                Frame::Ack { ok } => Ok(ok),
                other => anyhow::bail!("expected an ack, got {other:?}"),
            };
        }
        let n = tokio::io::AsyncReadExt::read(&mut stream, &mut chunk).await?;
        // The daemon closed without answering — an older build that does not
        // know this frame. Treated as "did not land", because the one thing
        // that must not happen is reporting a message as delivered when the
        // far side dropped it on the floor.
        if n == 0 {
            return Ok(false);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
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
