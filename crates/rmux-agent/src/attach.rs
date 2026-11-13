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
use crate::protocol::{Binding, BindingReport, Frame, Hello};

/// How a client attach behaves beyond the bare protocol.
#[derive(Clone, Debug, Default)]
pub struct AttachOptions {
    /// The command-line detach key (`C-b d`). **`None` for every attach the
    /// desktop app makes** — it uses `Ctrl-B` as its own prefix, so a filter
    /// here would turn its forwarded `\x02` plus a typed `d` into a dropped
    /// terminal. Only the CLI sets this, via `--detach-key`.
    pub detach: Option<crate::detach::DetachKey>,
    /// What `[detached (from session …)]` calls it — the alias when it has one.
    pub label: Option<String>,
    /// The credential set to bind a **new** session to (`--env-set`). Sent as
    /// [`Frame::HelloBound`]; a daemon that does not acknowledge it is refused,
    /// never attached to.
    pub binding: Option<Binding>,
}

/// The attach flags that carry a binding. Stripped on a handoff: the owning
/// daemon already holds the session (that is why it owns it), a live session
/// keeps the binding it was created with, and an older build would refuse the
/// flags as unknown options.
pub const BINDING_FLAGS: &[&str] = &["--env-set", "--env-chain", "--env-members", "--env-overloaded"];

/// Attach to `session`, creating it if needed.
pub async fn attach(hello: Hello, start_daemon: bool) -> anyhow::Result<ExitCode> {
    attach_with(hello, start_daemon, AttachOptions::default()).await
}

/// [`attach`], with a detach key and a label for the detach message.
pub async fn attach_with(
    mut hello: Hello,
    start_daemon: bool,
    options: AttachOptions,
) -> anyhow::Result<ExitCode> {
    let mut endpoint = daemon::endpoint()?;
    // Whether `endpoint` is this build's own daemon, which is the only one this
    // process may start.
    let mut own = true;

    // **A command-line attach connects to the owning daemon instead of handing
    // off.** `hand_off` re-execs the older build with *these* arguments, and an
    // older build refuses `--detach-key` as an unknown option — so on exactly
    // the hosts that matter (a session held by a superseded daemon) a CLI attach
    // would fail outright. The attach protocol (`Hello`, `Data`, `Resize`) is the
    // one every daemon has always spoken, which is what `write_via_attach`
    // already relies on, so speaking it to the old daemon from here keeps the
    // detach key in this process where it works.
    if options.detach.is_some() {
        let found = owning_endpoint(&hello.session).await?;
        own = found.address == endpoint.address;
        endpoint = found;
    }

    // **Before creating anything, find out whether this session already exists
    // under a different build.** Our own daemon is asked first, because that is
    // the overwhelmingly common case and it costs one connect on a socket that
    // is already open. Only when it does not have the name is the install
    // directory searched — and if another build's daemon holds it, this process
    // hands over to that binary rather than starting a rival copy.
    //
    // The guard stops two builds bouncing a session between them forever.
    if options.detach.is_none() && start_daemon && std::env::var_os(HANDOFF_GUARD).is_none() {
        let ours = sessions_of(&endpoint).await;
        let known = ours.iter().any(|s| s.name == hello.session && s.pid.is_some());

        if !known
            && let Some(binary) = owner_of(&hello.session).await
        {
            return hand_off(&binary);
        }
    }
    let binding = options.binding.clone();

    // Held for the whole attachment. Dropping it puts the terminal back, which
    // matters on every exit path — a terminal left raw has no echo and no line
    // editing, and the user is left with an apparently broken shell.
    let raw = crate::tty::RawMode::enter();

    // The real terminal size beats whatever the caller guessed on the command
    // line: `ssh -tt` has already negotiated one, and it is authoritative.
    if let Some((cols, rows)) = crate::tty::window_size() {
        hello.cols = cols;
        hello.rows = rows;
    }

    let stream = match ipc::connect(&endpoint).await {
        Ok(stream) => stream,
        Err(_) if start_daemon && own => {
            // No daemon yet. Start one and wait — the ordinary first-connection
            // path, not an error worth reporting.
            spawn_daemon()?;
            wait_for_daemon(&endpoint).await?
        }
        Err(e) => return Err(e),
    };

    let (mut reader, mut writer) = tokio::io::split(stream);
    let session = hello.session.clone();
    let mut buf = Vec::new();
    match binding {
        None => writer.write_all(&Frame::Hello(hello).encode()).await?,
        Some(binding) => {
            writer.write_all(&Frame::HelloBound { hello, binding }.encode()).await?;
            writer.flush().await?;
            // The answer comes before any replay. No answer is the old-daemon
            // case and is a refusal: starting the session anyway would run it on
            // whatever provider that daemon defaults to.
            if let Err(e) = bound_answer(&mut reader, &mut buf).await {
                drop(raw);
                return Err(e);
            }
        }
    }
    // "Most recently attached" is what a bare `rmux attach` reaches for.
    crate::cli::record_recent(&session);
    // Registered so `rmux detach`, typed *inside* the session, can find this
    // client. Only a CLI attach registers, so the app's views are never detached.
    let _client = options.detach.map(|_| crate::cli::ClientRecord::register(&session));
    let (detach_tx, mut detach_rx) = tokio::sync::mpsc::channel::<()>(1);
    #[cfg(unix)]
    if options.detach.is_some() {
        let tx = detach_tx.clone();
        tokio::spawn(async move {
            let Ok(mut usr1) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
            else {
                return;
            };
            if usr1.recv().await.is_some() {
                let _ = tx.send(()).await;
            }
        });
    }
    let mut filter = options.detach.map(crate::detach::Filter::new);
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
                    let (bytes, detach) = match filter.as_mut() {
                        Some(f) => {
                            let out = f.feed(&chunk[..n]);
                            (std::borrow::Cow::Owned(out.forward), out.detach)
                        }
                        None => (std::borrow::Cow::Borrowed(&chunk[..n]), false),
                    };
                    if !bytes.is_empty() {
                        crate::protocol::encode_data_into(&mut frame, &bytes);
                        if writer.write_all(&frame).await.is_err() {
                            break;
                        }
                    }
                    if detach {
                        let _ = writer.flush().await;
                        let _ = detach_tx.send(()).await;
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
    let mut code = 0;
    let mut detached = false;
    'outer: loop {
        let mut chunk = [0u8; 8192];
        let n = tokio::select! {
            read = reader.read(&mut chunk) => read?,
            // Disabled once every sender is gone (stdin closed, no signal task):
            // a `None` does not match, so the output keeps draining as before.
            Some(()) = detach_rx.recv() => {
                detached = true;
                break 'outer;
            }
        };
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
                | Frame::SetKindEnv { .. }
                | Frame::List
                | Frame::Sessions(_)
                | Frame::Write { .. }
                | Frame::Ack { .. }
                | Frame::Read { .. }
                | Frame::Scrollback(_)
                | Frame::SetNamedEnv { .. }
                | Frame::HelloBound { .. }
                | Frame::Refused { .. }
                | Frame::GetBinding { .. }
                | Frame::BindingIs(_)
                | Frame::Switch { .. } => {}
            }
        }
    }

    to_daemon.abort();
    if detached {
        // Undo the modes a TUI may have left the *local* terminal in — the
        // session still has them; this terminal is going back to a shell.
        // Alternate screen, hidden cursor, mouse reporting, bracketed paste.
        stdout
            .write_all(b"\x1b[?1049l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l")
            .await?;
        stdout.flush().await?;
        drop(raw);
        let label = options.label.unwrap_or(session);
        println!("[detached (from session {label})]");
        return Ok(ExitCode::SUCCESS);
    }
    drop(raw);
    Ok(if code == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Read the daemon's answer to a [`Frame::HelloBound`], leaving anything after
/// it (the replay) in `buf`.
async fn bound_answer<R: tokio::io::AsyncRead + Unpin>(reader: &mut R, buf: &mut Vec<u8>) -> anyhow::Result<()> {
    loop {
        if let Some((frame, used)) = Frame::decode(buf)? {
            buf.drain(..used);
            return match frame {
                Frame::Ack { ok: true } => Ok(()),
                Frame::Refused { reason } => anyhow::bail!("{reason}"),
                other => anyhow::bail!("the agent daemon answered a bound attach with {other:?}"),
            };
        }
        let mut chunk = [0u8; 4096];
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            anyhow::bail!(
                "the rmux agent daemon on this host is too old to bind a session to a credential \
                 set, so the session was not started (it would have run on another provider) — \
                 end that daemon's sessions or reinstall the agent"
            );
        }
        buf.extend_from_slice(&chunk[..n]);
    }
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

        let args = handoff_args(std::env::args().skip(1).collect());
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
        let args = handoff_args(std::env::args().skip(1).collect());
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

/// The binding flags of `attach`, before they are checked against the session.
#[derive(Clone, Debug, Default)]
pub struct BindingFlags {
    pub set: Option<String>,
    pub chain: Option<String>,
    /// Comma-separated member set names, in order.
    pub members: Option<String>,
    pub overloaded: bool,
}

impl BindingFlags {
    /// The binding these flags ask for, for `session` — whose name prefix
    /// decides the kind, exactly as it decides which per-kind set applies.
    pub fn resolve(self, session: &str) -> anyhow::Result<Option<Binding>> {
        let Some(set) = self.set else {
            anyhow::ensure!(self.chain.is_none() && self.members.is_none(), "--env-chain needs --env-set");
            return Ok(None);
        };
        let kind = rmux_claude_core::env_kind::for_session(session).ok_or_else(|| {
            anyhow::anyhow!("--env-set applies to claude-, codex- and dsh- sessions, not {session}")
        })?;
        anyhow::ensure!(rmux_claude_core::env_kind::valid_set_name(&set), "{set:?} is not a valid set name");
        let chain = match (self.chain, self.members) {
            (None, None) => None,
            (Some(id), Some(members)) => {
                let members: Vec<String> =
                    members.split(',').map(str::trim).filter(|m| !m.is_empty()).map(str::to_owned).collect();
                anyhow::ensure!(members.contains(&set), "--env-set {set} is not one of --env-members");
                Some(rmux_claude_core::fallback::ChainState {
                    id,
                    members,
                    active: set.clone(),
                    switch_on_overloaded: self.overloaded,
                    ..Default::default()
                })
            }
            _ => anyhow::bail!("--env-chain and --env-members go together"),
        };
        Ok(Some(Binding { kind: kind.to_owned(), set, chain }))
    }
}

/// Where `setenv` delivers: everywhere, one kind, or one named set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetenvTarget {
    Shared,
    Kind(String),
    Named { kind: String, set: String, label: Option<String> },
}

impl SetenvTarget {
    /// `[--kind K [--set NAME [--label TEXT]]]`, in any order.
    pub fn parse(args: &[String]) -> anyhow::Result<Self> {
        let (mut kind, mut set, mut label) = (None, None, None);
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let mut value = || it.next().cloned().ok_or_else(|| anyhow::anyhow!("setenv {flag} needs a value"));
            match flag.as_str() {
                "--kind" => kind = Some(value()?),
                "--set" => set = Some(value()?),
                "--label" => label = Some(value()?),
                other => anyhow::bail!("setenv takes --kind, --set and --label, not {other:?}"),
            }
        }
        Ok(match (kind, set) {
            (None, None) if label.is_none() => SetenvTarget::Shared,
            (Some(kind), None) if label.is_none() => SetenvTarget::Kind(kind),
            (Some(kind), Some(set)) => SetenvTarget::Named { kind, set, label },
            _ => anyhow::bail!("setenv --set needs --kind, and --label needs --set"),
        })
    }

    pub async fn deliver(self, pairs: std::collections::BTreeMap<String, String>) -> anyhow::Result<()> {
        match self {
            SetenvTarget::Shared => set_env(pairs).await,
            SetenvTarget::Kind(kind) => set_kind_env(&kind, pairs).await,
            SetenvTarget::Named { kind, set, label } => set_named_env(&kind, &set, label, pairs).await,
        }
    }
}

/// This process's arguments minus the binding flags — see [`BINDING_FLAGS`].
fn handoff_args(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--env-overloaded" {
            continue;
        }
        if BINDING_FLAGS.contains(&a.as_str()) {
            it.next();
            continue;
        }
        out.push(a);
    }
    out
}

/// The first agent build that understands the drive frames
/// (`Frame::Write`/`Frame::Read`, added in 0.2.20). A session held by an older
/// daemon is enumerated by `list` but cannot be driven — the caller is told
/// [`rmux_bridge::ErrorCode::UnsupportedByHost`], not `NotFound`, because it is
/// alive.
pub const MIN_DRIVABLE: (u32, u32, u32) = (0, 2, 20);

/// Parse `X.Y.Z` from an installed agent's file name
/// (`rmux-agent-<version>-<fingerprint>`). The version has no dashes and the
/// fingerprint follows one, so the segment after the `rmux-agent-` prefix and
/// before the next `-` is the version. `None` for anything that does not match.
///
/// Dev builds are all `0.1.0`, which parses below `MIN_DRIVABLE` even though a
/// dev binary built from current source *does* have the frames — so callers treat
/// an *own*-daemon session as drivable unconditionally (this build has the
/// frames) and apply the version test only to sibling builds, which on a real
/// host carry real version numbers.
pub fn agent_version(name: &str) -> Option<(u32, u32, u32)> {
    let base = std::path::Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or(name);
    let rest = base.strip_prefix("rmux-agent-")?;
    let ver = rest.split('-').next()?;
    let mut parts = ver.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Why a drive attempt (`write`/`read`) came back empty: the session ended, or
/// its owning daemon is a build too old to drive.
pub enum DriveFailure {
    /// No live daemon holds the name — it ended.
    NotFound,
    /// A daemon holds it, but that daemon predates the drive frames.
    UnsupportedByHost,
}

/// Distinguish "gone" from "owned by an un-driveable daemon" — called only on the
/// failure path, so its extra daemon queries stay off the hot path.
///
/// `owner_of` searches only *sibling* builds and requires a live pid, so a hit is
/// a live session on an older daemon; the version test then decides. A miss means
/// no older daemon holds it — either it ended, or it was the current build's own
/// (which supports the frames, so a failed write there genuinely means gone).
pub async fn drive_failure(session: &str) -> DriveFailure {
    match owner_of(session).await {
        Some(binary) => {
            let name = binary.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            match agent_version(name) {
                Some(v) if v < MIN_DRIVABLE => DriveFailure::UnsupportedByHost,
                _ => DriveFailure::NotFound,
            }
        }
        None => DriveFailure::NotFound,
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
    login_command: Option<&str>,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    spawn_detached_bound(session, cwd, login_command, cols, rows, None).await
}

/// [`spawn_detached`], bound to a credential set — how `rmux spawn` gives a
/// child its parent's provider. A refusal (set not loaded, wrong kind, an old
/// daemon) is an error and nothing is started.
pub async fn spawn_detached_bound(
    session: &str,
    cwd: &str,
    login_command: Option<&str>,
    cols: u16,
    rows: u16,
    binding: Option<Binding>,
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
        // it plainly works when typed. `None` is a plain interactive login shell.
        login_command: login_command.map(str::to_owned),
        env: Default::default(),
        cols,
        rows,
    };
    if let Some(binding) = binding {
        stream.write_all(&Frame::HelloBound { hello, binding }.encode()).await?;
        stream.flush().await?;
        let mut buf = Vec::new();
        let (mut reader, _writer) = tokio::io::split(stream);
        return bound_answer(&mut reader, &mut buf).await;
    }
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
/// Where a session lives, resolved across every installed build.
///
/// A session belongs to the daemon of the build that created it, so after an
/// upgrade the name a caller wants is held by a *previous* daemon. This is the
/// resolution `write` and `kill` do inline; the streaming helpers below share it.
async fn owning_endpoint(session: &str) -> anyhow::Result<ipc::Endpoint> {
    let endpoint = daemon::endpoint()?;
    if !sessions_of(&endpoint).await.iter().any(|s| s.name == session)
        && let Some(binary) = owner_of(session).await
        && let Some(name) = binary.file_name().and_then(|n| n.to_str())
        && let Ok(other) = ipc::Endpoint::for_exe_name(crate::provision::VERSION, name)
    {
        return Ok(other);
    }
    Ok(endpoint)
}

/// A session's recent output, **without attaching to it**.
///
/// The read twin of [`write`]. `None` when no daemon on this host holds the
/// name — the ordinary outcome for a remote caller working from a slightly stale
/// list, and told apart from an empty-but-live buffer by that fact.
///
/// The bytes are the scrollback verbatim: escape sequences and all, exactly as
/// the shell wrote them. The daemon does not emulate a terminal, so it is
/// physically unable to hand back a rendered screen — only the raw stream.
pub async fn read(session: &str) -> anyhow::Result<Option<Vec<u8>>> {
    // Existence is decided from the union of every daemon, so a session held by
    // an older build still reads rather than reporting as gone.
    if !list().await.unwrap_or_default().iter().any(|s| s.name == session) {
        return Ok(None);
    }
    read_scrollback(session).await
}

/// The scrollback of a session **whether or not its shell is still running**.
///
/// [`read`] answers `None` for an exited session, which is right for a remote
/// caller and useless for `rmux spawn` explaining *why* the agent it just
/// started died (`command not found: claude` is in that buffer). The daemon
/// keeps an exited session's terminal until the name is reused, so its last
/// words are still there to read.
pub async fn read_scrollback(session: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let endpoint = owning_endpoint(session).await?;
    let Ok(mut stream) = ipc::connect(&endpoint).await else { return Ok(None) };

    stream.write_all(&Frame::Read { session: session.to_owned() }.encode()).await?;
    stream.flush().await?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some((frame, _)) = Frame::decode(&buf)? {
            return match frame {
                Frame::Scrollback(bytes) => Ok(Some(bytes)),
                other => anyhow::bail!("expected scrollback, got {other:?}"),
            };
        }
        let n = tokio::io::AsyncReadExt::read(&mut stream, &mut chunk).await?;
        // An older daemon that does not know `Read` closes without answering.
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// One thing that happened on a streamed terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Raw bytes the shell produced — backlog first on attach, then live.
    Output(Vec<u8>),
    /// The shell exited; no more output will come.
    Exited(i32),
}

/// A live attachment to an existing terminal session.
///
/// This is the machinery behind "see the terminal in Redstone, like a normal
/// terminal": it attaches to a session the operator already opened and pumps its
/// bytes both ways. Dropping it detaches — the session keeps running, because
/// that is the whole point of the daemon.
///
/// It **cannot create a session**: [`open_terminal_stream`] verifies the name
/// exists first and only then attaches. There is deliberately no path here that
/// spawns a shell, so a live terminal only ever reaches a host the operator both
/// enrolled and already has a session on.
pub struct TerminalStream {
    to_daemon: tokio::sync::mpsc::Sender<Frame>,
    output: tokio::sync::mpsc::Receiver<StreamEvent>,
}

impl TerminalStream {
    /// Type bytes into the terminal. `false` once the connection has ended.
    pub async fn input(&self, data: Vec<u8>) -> bool {
        self.to_daemon.send(Frame::Data(data)).await.is_ok()
    }

    /// Tell the far side the viewport changed. Last attach wins on size, which is
    /// what every multiplexer does and what "a normal terminal" expects.
    pub async fn resize(&self, cols: u16, rows: u16) -> bool {
        self.to_daemon.send(Frame::Resize { cols, rows }).await.is_ok()
    }

    /// The next thing the terminal did, or `None` once it is gone.
    pub async fn next(&mut self) -> Option<StreamEvent> {
        self.output.recv().await
    }
}

/// Attach to an existing terminal and stream it both ways.
///
/// `None` when the session does not exist on any daemon — never a new shell.
pub async fn open_terminal_stream(
    session: &str,
    cols: u16,
    rows: u16,
) -> anyhow::Result<Option<TerminalStream>> {
    if !list().await.unwrap_or_default().iter().any(|s| s.name == session) {
        return Ok(None);
    }
    let endpoint = owning_endpoint(session).await?;
    let Ok(stream) = ipc::connect(&endpoint).await else { return Ok(None) };

    let (mut reader, mut writer) = tokio::io::split(stream);

    // A `Hello` for a name that already exists *reattaches* — see
    // `daemon::open_or_attach`. Existence was just checked, so this joins the
    // live session rather than creating one. `program: None` is irrelevant on
    // the reattach path; it only matters when a session is being created.
    let hello = Hello {
        session: session.to_owned(),
        cwd: None,
        program: None,
        args: Vec::new(),
        login_command: None,
        env: Default::default(),
        cols,
        rows,
    };
    writer.write_all(&Frame::Hello(hello).encode()).await?;
    writer.flush().await?;

    // Bounded, because a remote viewer that stops reading must not let the host's
    // memory grow without limit — the channel fills, `send` blocks the pump, and
    // back-pressure reaches the shell exactly as a slow local terminal would.
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<StreamEvent>(256);
    let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<Frame>(64);

    tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            tokio::select! {
                // Input from the viewer → daemon. `None` means the
                // `TerminalStream` was dropped, which is our cue to detach.
                frame = in_rx.recv() => {
                    let Some(frame) = frame else { break };
                    if writer.write_all(&frame.encode()).await.is_err() {
                        break;
                    }
                }
                // Daemon output → viewer.
                read = reader.read(&mut chunk) => {
                    let Ok(n) = read else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    while let Ok(Some((frame, used))) = Frame::decode(&buf) {
                        buf.drain(..used);
                        match frame {
                            Frame::Data(bytes) => {
                                // A closed receiver means the viewer went away;
                                // stop rather than spin.
                                if out_tx.send(StreamEvent::Output(bytes)).await.is_err() {
                                    return;
                                }
                            }
                            Frame::Exited { code } => {
                                let _ = out_tx.send(StreamEvent::Exited(code)).await;
                                return;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    });

    Ok(Some(TerminalStream { to_daemon: in_tx, output: out_rx }))
}

/// The viewport a one-shot drive attaches an old daemon with. Wide enough that a
/// TUI does not truncate; the operator's own attach resizes it back to their pane.
const DRIVE_COLS: u16 = 120;
const DRIVE_ROWS: u16 = 40;

/// Deliver keystrokes to a session that already exists, without holding an
/// attachment.
///
/// **Drives every daemon, new or old.** This build's own daemon (and any
/// ≥ 0.2.20) takes the one-shot `Frame::Write`. A daemon from *before* that frame
/// existed is driven the way rmux's own terminals drive it — the original
/// `Hello`/`Data` attach, which every daemon has always spoken — so a session
/// started weeks ago under an earlier agent is still typeable. Without this, a
/// busy host that keeps a daemon per rmux upgrade could *list* such a session but
/// never accept input to it: exactly the "the agent connected to Redstone is not
/// the one running it" report.
///
/// `false` means no live daemon holds the name — it ended. It never *creates* a
/// session: the fallback attaches only to a name `owner_of` just confirmed live.
pub async fn write(session: &str, data: &[u8]) -> anyhow::Result<bool> {
    let own = daemon::endpoint()?;
    // Our own daemon is this build, which has `Frame::Write`.
    if sessions_of(&own).await.iter().any(|s| s.name == session) {
        return write_via_frame(&own, session, data).await;
    }
    // Otherwise the session belongs to another installed build's daemon.
    let Some(binary) = owner_of(session).await else { return Ok(false) };
    let Some(name) = binary.file_name().and_then(|n| n.to_str()) else { return Ok(false) };
    let Ok(endpoint) = ipc::Endpoint::for_exe_name(crate::provision::VERSION, name) else {
        return Ok(false);
    };
    match agent_version(name) {
        // Too old for `Frame::Write` — attach with the original protocol instead.
        Some(v) if v < MIN_DRIVABLE => write_via_attach(session, data).await,
        _ => write_via_frame(&endpoint, session, data).await,
    }
}

/// The one-shot `Frame::Write` path, for a daemon new enough to answer it.
///
/// **Never starts a daemon** and **never sends a Hello**: a Hello would *create*
/// the session when the name is unknown, so a stale name from a remote caller
/// would spawn a shell instead of reporting that the conversation had ended.
async fn write_via_frame(
    endpoint: &ipc::Endpoint,
    session: &str,
    data: &[u8],
) -> anyhow::Result<bool> {
    let Ok(mut stream) = ipc::connect(endpoint).await else {
        return Ok(false);
    };

    stream
        .write_all(&Frame::Write { session: session.to_owned(), data: data.to_vec() }.encode())
        .await?;
    stream.flush().await?;

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
        // A daemon that predates this frame closes rather than acking; that is the
        // one the caller routed *away* from, so reaching it here means the session
        // ended between resolution and now — "did not land".
        if n == 0 {
            return Ok(false);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Drive a pre-`Frame::Write` daemon by attaching with the original protocol,
/// delivering the keystrokes, and detaching — the machinery rmux's own live
/// terminals use, so it is known to work against every daemon ever shipped.
///
/// Reuses [`open_terminal_stream`], which verifies the session exists before it
/// attaches (so this cannot create one). **Draining the output is load-bearing:**
/// the stream's pump multiplexes our input and the daemon's output on one task, so
/// if we never read the scrollback replay the pump blocks on a full channel and
/// our keystrokes never reach the daemon. Draining until the output goes idle both
/// frees the pump to deliver the input and confirms the session processed it.
///
/// The attach carries a size, so the session briefly reflows to it — the keystrokes
/// reach the agent's input regardless of width, and the operator's next real attach
/// resizes it back. The one residual risk is the millisecond between the existence
/// check and the attach: a session that ends in it would be recreated, the same
/// narrow window every attach has.
async fn write_via_attach(session: &str, data: &[u8]) -> anyhow::Result<bool> {
    let Some(mut term) = open_terminal_stream(session, DRIVE_COLS, DRIVE_ROWS).await? else {
        return Ok(false);
    };
    if !term.input(data.to_vec()).await {
        return Ok(false);
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(600);
    loop {
        if deadline.saturating_duration_since(tokio::time::Instant::now()).is_zero() {
            break;
        }
        match tokio::time::timeout(std::time::Duration::from_millis(120), term.next()).await {
            Ok(Some(StreamEvent::Output(_))) => continue,
            Ok(Some(StreamEvent::Exited(_))) | Ok(None) => break,
            // Idle: the replay is drained and our input has been echoed. Done.
            Err(_) => break,
        }
    }
    // Dropping `term` detaches; the session keeps running.
    Ok(true)
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

/// Replace one agent kind's credential environment in this build's daemon.
///
/// Unlike [`set_env`] this **waits for the daemon's answer**, and a missing
/// answer is an error. That is the compatibility rule for this frame: a daemon
/// that predates it rejects the unknown frame kind and closes, and the honest
/// report is "the credentials did not land" — never a quiet success after which
/// a session starts against whatever provider the host defaults to.
pub async fn set_kind_env(
    kind: &str,
    pairs: std::collections::BTreeMap<String, String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        rmux_claude_core::env_kind::is_known(kind),
        "unknown agent kind {kind:?} (expected one of {})",
        rmux_claude_core::env_kind::KINDS.join(", ")
    );
    let endpoint = daemon::endpoint()?;
    let mut stream = match ipc::connect(&endpoint).await {
        Ok(stream) => stream,
        Err(_) => {
            spawn_daemon()?;
            wait_for_daemon(&endpoint).await?
        }
    };
    stream
        .write_all(&Frame::SetKindEnv { kind: kind.to_owned(), env: pairs }.encode())
        .await?;
    stream.flush().await?;
    match read_ack(&mut stream).await? {
        Some(true) => Ok(()),
        Some(false) => anyhow::bail!("the agent daemon refused credentials for {kind:?}"),
        None => anyhow::bail!(
            "the agent daemon on this host is too old to keep credentials per agent, so the \
             {kind} credentials were not delivered — restart its sessions or reinstall the agent"
        ),
    }
}

/// Store one named credential set in this build's daemon, from stdin.
///
/// Waits for the answer like [`set_kind_env`]; an older daemon drops the
/// unknown frame and that is reported, not swallowed.
pub async fn set_named_env(
    kind: &str,
    set: &str,
    label: Option<String>,
    pairs: std::collections::BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let mut stream = connect_or_start().await?;
    stream
        .write_all(
            &Frame::SetNamedEnv { kind: kind.to_owned(), set: set.to_owned(), label, env: pairs }.encode(),
        )
        .await?;
    stream.flush().await?;
    match read_answer(&mut stream).await? {
        Some(Ok(())) => Ok(()),
        Some(Err(reason)) => anyhow::bail!("the agent daemon refused the {kind} set {set:?}: {reason}"),
        None => anyhow::bail!(
            "the agent daemon on this host is too old to keep credentials per session, so the \
             {kind} set {set:?} was not delivered — end its sessions or reinstall the agent"
        ),
    }
}

/// Which set `session` runs on, from this build's daemon. `None` when no
/// daemon is running or it is too old to say.
pub async fn get_binding(session: &str) -> Option<BindingReport> {
    get_binding_at(&daemon::endpoint().ok()?, session).await
}

/// [`get_binding`], asking every installed build's daemon until one holds a
/// binding for `session` — sessions outlive upgrades, so the owner may be older.
pub async fn binding_anywhere(session: &str) -> Option<BindingReport> {
    if let Some(r) = get_binding(session).await.filter(|r| r.binding.is_some()) {
        return Some(r);
    }
    for binary in sibling_binaries() {
        let Some(name) = binary.file_name().and_then(|n| n.to_str()) else { continue };
        let Ok(endpoint) = ipc::Endpoint::for_exe_name(crate::provision::VERSION, name) else { continue };
        if let Some(r) = get_binding_at(&endpoint, session).await.filter(|r| r.binding.is_some()) {
            return Some(r);
        }
    }
    None
}

/// [`get_binding`] against a specific daemon.
pub async fn get_binding_at(endpoint: &ipc::Endpoint, session: &str) -> Option<BindingReport> {
    let mut stream = ipc::connect(endpoint).await.ok()?;
    stream.write_all(&Frame::GetBinding { session: session.to_owned() }.encode()).await.ok()?;
    stream.flush().await.ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match Frame::decode(&buf) {
            Ok(Some((Frame::BindingIs(report), _))) => return Some(report),
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => {}
        }
        match tokio::io::AsyncReadExt::read(&mut stream, &mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Move a chained session to `member` and relaunch it ("switch back").
pub async fn switch_member(session: &str, member: &str) -> anyhow::Result<()> {
    let endpoint = daemon::endpoint()?;
    let mut stream = ipc::connect(&endpoint).await?;
    stream
        .write_all(&Frame::Switch { session: session.to_owned(), member: member.to_owned() }.encode())
        .await?;
    stream.flush().await?;
    match read_answer(&mut stream).await? {
        Some(Ok(())) => Ok(()),
        Some(Err(reason)) => anyhow::bail!("{reason}"),
        None => anyhow::bail!("the agent daemon on this host is too old to switch a session's account"),
    }
}

async fn connect_or_start() -> anyhow::Result<ipc::Stream> {
    let endpoint = daemon::endpoint()?;
    match ipc::connect(&endpoint).await {
        Ok(stream) => Ok(stream),
        Err(_) => {
            spawn_daemon()?;
            wait_for_daemon(&endpoint).await
        }
    }
}

/// Read an `Ack` or a `Refused`; `None` when the daemon closed without either.
async fn read_answer<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
) -> anyhow::Result<Option<Result<(), String>>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        if let Some((frame, _)) = Frame::decode(&buf)? {
            return match frame {
                Frame::Ack { ok: true } => Ok(Some(Ok(()))),
                Frame::Ack { ok: false } => Ok(Some(Err("refused".to_owned()))),
                Frame::Refused { reason } => Ok(Some(Err(reason))),
                other => anyhow::bail!("expected an answer, got {other:?}"),
            };
        }
        let n = tokio::io::AsyncReadExt::read(stream, &mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Read one `Ack`; `None` when the daemon closed without one.
async fn read_ack<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> anyhow::Result<Option<bool>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64];
    loop {
        if let Some((frame, _)) = Frame::decode(&buf)? {
            return match frame {
                Frame::Ack { ok } => Ok(Some(ok)),
                other => anyhow::bail!("expected an ack, got {other:?}"),
            };
        }
        let n = tokio::io::AsyncReadExt::read(stream, &mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_daemon_that_closes_without_an_ack_is_an_error_not_a_success() {
        // What an older daemon does with a frame kind it does not know: decode
        // fails, the connection drops. `read_ack` must report that as "no
        // answer", which `set_kind_env` turns into a refusal.
        let (mut client, server) = tokio::io::duplex(64);
        drop(server);
        assert_eq!(read_ack(&mut client).await.unwrap(), None);

        let (mut client, mut server) = tokio::io::duplex(64);
        server.write_all(&Frame::Ack { ok: true }.encode()).await.unwrap();
        assert_eq!(read_ack(&mut client).await.unwrap(), Some(true));
    }

    #[tokio::test]
    async fn a_bound_attach_to_a_daemon_that_closes_is_refused_not_started() {
        // An older daemon cannot decode `HelloBound` and drops the connection.
        // The client must say so and stop — attaching anyway would run the
        // session on that daemon's default provider.
        let (client, server) = tokio::io::duplex(64);
        drop(server);
        let (mut reader, _w) = tokio::io::split(client);
        let err = bound_answer(&mut reader, &mut Vec::new()).await.unwrap_err().to_string();
        assert!(err.contains("too old to bind"), "{err}");

        let (client, mut server) = tokio::io::duplex(256);
        server.write_all(&Frame::Refused { reason: "set 9 is not loaded".into() }.encode()).await.unwrap();
        let (mut reader, _w) = tokio::io::split(client);
        assert!(bound_answer(&mut reader, &mut Vec::new()).await.unwrap_err().to_string().contains("set 9"));

        // Ack, then the replay stays in the buffer for the stream loop.
        let (client, mut server) = tokio::io::duplex(256);
        let mut both = Frame::Ack { ok: true }.encode();
        both.extend(Frame::Data(b"replay".to_vec()).encode());
        server.write_all(&both).await.unwrap();
        let (mut reader, _w) = tokio::io::split(client);
        let mut buf = Vec::new();
        bound_answer(&mut reader, &mut buf).await.unwrap();
        assert_eq!(Frame::decode(&buf).unwrap().unwrap().0, Frame::Data(b"replay".to_vec()));
    }

    #[test]
    fn binding_flags_resolve_against_the_session_kind_and_strip_on_handoff() {
        let flags = BindingFlags { set: Some("2".into()), chain: Some("c".into()), members: Some("1, 2,3".into()), overloaded: false };
        let b = flags.resolve("claude-x").unwrap().unwrap();
        assert_eq!((b.kind.as_str(), b.set.as_str()), ("claude", "2"));
        assert_eq!(b.chain.unwrap().members, ["1", "2", "3"]);
        assert!(BindingFlags { set: Some("1".into()), ..Default::default() }.resolve("term-1").is_err());
        assert!(BindingFlags { set: Some("9".into()), chain: Some("c".into()), members: Some("1".into()), overloaded: false }
            .resolve("claude-x")
            .is_err());
        assert_eq!(BindingFlags::default().resolve("claude-x").unwrap(), None);
        let args: Vec<String> = ["attach", "--session", "s", "--env-set", "1", "--env-overloaded", "--env-chain", "c", "--cols", "80"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(handoff_args(args), ["attach", "--session", "s", "--cols", "80"]);
        assert_eq!(
            SetenvTarget::parse(&["--set".into(), "1".into(), "--kind".into(), "claude".into()]).unwrap(),
            SetenvTarget::Named { kind: "claude".into(), set: "1".into(), label: None }
        );
        assert!(SetenvTarget::parse(&["--set".into(), "1".into()]).is_err());
    }

    #[test]
    fn agent_version_parses_the_installed_name() {
        // `rmux-agent-<version>-<fingerprint>`: the version has no dashes and the
        // fingerprint follows one. Same class of parsing bug as the socket
        // `file_stem` — a host with dots in the version made that one ship.
        assert_eq!(agent_version("rmux-agent-0.2.17-854f82bdd55cb743"), Some((0, 2, 17)));
        assert_eq!(agent_version("rmux-agent-0.2.20-abc"), Some((0, 2, 20)));
        assert_eq!(agent_version("rmux-agent-0.1.0-deadbeef"), Some((0, 1, 0)));
        // A full path is accepted — only the file name is parsed.
        assert_eq!(
            agent_version("/home/anh.nguyen/.rmux/bin/rmux-agent-0.2.19-5fb8eec"),
            Some((0, 2, 19)),
        );
        // Not one of ours, or malformed → None (treated as drivable by callers).
        assert_eq!(agent_version("rmux-agent-bridge"), None);
        assert_eq!(agent_version("something-else"), None);
        assert_eq!(agent_version("rmux-agent-0.2-abc"), None);
    }

    #[test]
    fn the_drive_frames_arrived_in_0_2_20() {
        // The whole point of the flag: sessions on daemons below this are
        // enumerated but not driveable.
        assert!(agent_version("rmux-agent-0.2.20-x").unwrap() >= MIN_DRIVABLE);
        assert!(agent_version("rmux-agent-0.2.24-x").unwrap() >= MIN_DRIVABLE);
        assert!(agent_version("rmux-agent-0.3.0-x").unwrap() >= MIN_DRIVABLE);
        assert!(agent_version("rmux-agent-0.2.19-x").unwrap() < MIN_DRIVABLE);
        assert!(agent_version("rmux-agent-0.2.13-x").unwrap() < MIN_DRIVABLE);
        assert!(agent_version("rmux-agent-0.1.6-x").unwrap() < MIN_DRIVABLE);
    }
}
