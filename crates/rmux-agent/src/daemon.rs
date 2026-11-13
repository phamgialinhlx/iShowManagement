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
//!
//! ## Credentials are named per session
//!
//! A session is **bound** to one named credential set at creation
//! ([`Frame::HelloBound`]) and gets exactly that set at spawn. The per-kind set
//! ([`Frame::SetKindEnv`]) survives only for clients that do not bind. For a
//! fallback chain the binding also carries the chain, and the daemon's monitor
//! ([`monitor`]) moves the session to the next member on a usage limit — on the
//! host, so it works with the app closed.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use rmux_claude_core::env_kind;
use rmux_claude_core::fallback::{self as policy, Decision, FallbackRecord};
use rmux_term::{TermSize, Terminal, TerminalEvent};
use rmux_transport::{CommandSpec, LocalTarget, Target, Tty, local::terminal_env};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::fallback;
use crate::ipc::{self, Endpoint};
use crate::protocol::{Binding, BindingReport, Frame, Hello, encode_data_into};
use crate::session_env;

/// Where the daemon listens, on whichever platform this is.
pub fn endpoint() -> anyhow::Result<Endpoint> {
    Endpoint::current(env!("CARGO_PKG_VERSION"))
}

/// The terminal a session is running **now**.
///
/// A slot rather than a bare terminal because a fallback switch relaunches the
/// process under the same name, and every attached client must follow it to the
/// new one instead of being told the session exited. `respawning` is raised
/// before the old process is killed, so a client that sees its `Exited` knows to
/// wait for `generation` to move rather than to hang up.
struct TermSlot {
    current: Mutex<Arc<Terminal>>,
    generation: tokio::sync::watch::Sender<u64>,
    respawning: AtomicBool,
}

impl TermSlot {
    fn new(terminal: Arc<Terminal>) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new(terminal),
            generation: tokio::sync::watch::channel(0u64).0,
            respawning: AtomicBool::new(false),
        })
    }

    fn current(&self) -> Arc<Terminal> {
        Arc::clone(&self.current.lock())
    }
}

/// A session, plus what the daemon knows about it that the terminal does not.
///
/// The extra fields exist to make a *leak* identifiable, not merely to list
/// things. A name and a pid say what is running; age and attachment say whether
/// anyone still wants it — which is the actual question when a shell has been
/// alive for six weeks and its tab is long gone.
struct Session {
    slot: Arc<TermSlot>,
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
    /// The request it was started from — what a fallback relaunch repeats.
    hello: Hello,
    /// The credential set it runs on. `None` for a client that did not bind,
    /// which gets the per-kind set as before.
    binding: Option<Binding>,
    /// When the current set was bound (unix seconds) — the debounce's floor.
    bound_since: u64,
    last_switch: Option<u64>,
    /// A switch is in progress; the monitor leaves it alone.
    switching: bool,
    /// The chain ran out. Nothing more is detected until the operator moves it,
    /// or the same limit would be logged every cooldown for ever.
    parked: bool,
}

impl Session {
    fn terminal(&self) -> Arc<Terminal> {
        self.slot.current()
    }
}

/// One named credential set.
#[derive(Clone, Debug, Default)]
struct NamedSet {
    label: Option<String>,
    /// An empty value is a removal.
    env: BTreeMap<String, String>,
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
    env: BTreeMap<String, String>,
    /// Credential environment **per agent kind** (`claude`, `deepseek`,
    /// `openai`), applied at spawn only to an *unbound* session of that kind —
    /// see `rmux_claude_core::env_kind`. Same in-memory-only rule as `env`.
    ///
    /// Kept for clients that do not bind. A bound session never sees it: the
    /// per-daemon set is exactly what could not express two accounts on one
    /// host.
    kind_env: BTreeMap<String, BTreeMap<String, String>>,
    /// Named credential sets, keyed `(kind, name)`. Same in-memory-only rule.
    named: BTreeMap<(String, String), NamedSet>,
    /// Where Claude's files and `~/.rmux/fallback.json` live. `None` means the
    /// real home; tests point it at a sandbox.
    home: Option<PathBuf>,
}

impl Sessions {
    fn home(&self) -> Option<PathBuf> {
        self.home.clone().or_else(fallback::home)
    }

    fn label(&self, kind: &str, set: &str) -> Option<String> {
        self.named.get(&(kind.to_owned(), set.to_owned())).and_then(|s| s.label.clone())
    }
}

/// Replace one kind's credential environment.
///
/// **Replace, not merge.** A per-kind set has exactly one writer — the profile
/// apply for that kind — so whatever it sends is the whole truth for that kind,
/// and anything a previous profile set that this one does not is gone. That is
/// strictly stronger than merge-with-empty-means-remove: no clear-list can be
/// incomplete. Empty values are still dropped, so a client that sends the
/// clear-set the old way gets the same answer. Refused (`false`) for a kind
/// the daemon does not know, so a typo is an error rather than an unused set.
fn set_kind_env(
    into: &mut BTreeMap<String, BTreeMap<String, String>>,
    kind: &str,
    env: BTreeMap<String, String>,
) -> bool {
    if !env_kind::is_known(kind) {
        return false;
    }
    let env: BTreeMap<String, String> = env.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    if env.is_empty() {
        into.remove(kind);
    } else {
        into.insert(kind.to_owned(), env);
    }
    true
}

/// Store one named set, replacing any of that name. Refused for an unknown
/// kind, a malformed name, or the reserved [`env_kind::NO_PROFILE`].
fn set_named_env(
    into: &mut BTreeMap<(String, String), NamedSet>,
    kind: &str,
    set: &str,
    label: Option<String>,
    env: BTreeMap<String, String>,
) -> Result<(), String> {
    if !env_kind::is_known(kind) {
        return Err(format!("unknown agent kind {kind:?}"));
    }
    if !env_kind::valid_set_name(set) {
        return Err(format!("{set:?} is not a valid credential set name"));
    }
    if set == env_kind::NO_PROFILE {
        return Err(format!("{:?} is reserved for \"no profile\" and cannot carry variables", env_kind::NO_PROFILE));
    }
    into.insert((kind.to_owned(), set.to_owned()), NamedSet { label, env });
    Ok(())
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
fn merge_env(into: &mut BTreeMap<String, String>, from: BTreeMap<String, String>) {
    for (key, value) in from {
        if value.is_empty() {
            into.remove(&key);
        } else {
            into.insert(key, value);
        }
    }
}

/// The kind-scoped variables an *unbound* session named `session` is spawned with.
fn kind_env_for<'a>(
    kind_env: &'a BTreeMap<String, BTreeMap<String, String>>,
    session: &str,
) -> impl Iterator<Item = (&'a String, &'a String)> {
    env_kind::for_session(session).and_then(|kind| kind_env.get(kind)).into_iter().flatten()
}

/// The named set a binding resolves to, or why it cannot run.
///
/// **A missing set refuses, it never falls back.** After a daemon restart the
/// sets are gone (they are memory-only) and the app re-delivers them before it
/// attaches; a session that reached here without its set would otherwise start
/// on the shared account or the host's own login while the UI named another
/// provider.
fn resolve_set<'a>(g: &'a Sessions, session: &str, binding: &Binding) -> Result<Option<&'a NamedSet>, String> {
    match env_kind::for_session(session) {
        Some(kind) if kind == binding.kind => {}
        Some(kind) => {
            return Err(format!(
                "{session} is a {kind} session and cannot run on a {} credential set",
                binding.kind
            ));
        }
        None => {
            return Err(format!(
                "{session} is not an agent session; only claude-, codex- and dsh- sessions take a credential set"
            ));
        }
    }
    if let Some(chain) = &binding.chain
        && chain.active != binding.set
    {
        return Err(format!("the chain's active member is {}, not {}", chain.active, binding.set));
    }
    if binding.set == env_kind::NO_PROFILE {
        return Ok(None);
    }
    g.named.get(&(binding.kind.clone(), binding.set.clone())).map(Some).ok_or_else(|| {
        format!(
            "the {} credential set {:?} is not loaded in this host's rmux agent, so the session was \
             not started (it would have run on another provider). Start it from rmux, which \
             delivers the set first, or run `rmux-agent setenv --kind {} --set {}` with the \
             variables on stdin.",
            binding.kind, binding.set, binding.kind, binding.set
        )
    })
}

/// The environment a session is spawned with, and the keys its set removes.
///
/// Order: terminal env, the shared env, then **either** the bound set **or**
/// (unbound) the per-kind set, then `Hello.env`, then rmux's own session env.
/// A bound set therefore always wins over the shared account — the precedence
/// rule — and its removals take keys back out of everything before it.
fn compose_env(
    g: &Sessions,
    hello: &Hello,
    binding: Option<&Binding>,
) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let mut env: BTreeMap<String, String> = terminal_env().into_iter().collect();
    for (k, v) in &g.env {
        env.insert(k.clone(), v.clone());
    }
    let mut removals = Vec::new();
    match binding {
        Some(binding) => {
            if let Some(set) = resolve_set(g, &hello.session, binding)? {
                for (k, v) in &set.env {
                    if v.is_empty() {
                        env.remove(k);
                        removals.push(k.clone());
                    } else {
                        env.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        None => {
            for (k, v) in kind_env_for(&g.kind_env, &hello.session) {
                env.insert(k.clone(), v.clone());
            }
        }
    }
    for (k, v) in &hello.env {
        env.insert(k.clone(), v.clone());
    }
    // Last, so neither a socket-delivered env nor a Hello can make a session
    // claim to be another one: `RMUX_SESSION` is what `rmux close` ownership and
    // children.json parentage key on.
    for (k, v) in session_env::session_env(&hello.session) {
        env.insert(k, v);
    }
    Ok((env, removals))
}

/// The launch line a session runs, wherever the client put it: the login
/// command, or the `-c` argument of a program that is a shell.
fn launch_line_of(hello: &Hello) -> Option<&str> {
    if let Some(line) = &hello.login_command {
        return Some(line);
    }
    match hello.args.as_slice() {
        [flag, line] if flag == "-c" => Some(line),
        _ => None,
    }
}

fn with_launch_line(mut hello: Hello, line: String) -> Hello {
    if hello.login_command.is_some() {
        hello.login_command = Some(line);
    } else if let [flag, old] = hello.args.as_mut_slice()
        && flag == "-c"
    {
        *old = line;
    } else {
        hello.login_command = Some(line);
    }
    hello
}

/// Spawn the process for `hello`. The caller holds the lock.
fn spawn_terminal(
    g: &Sessions,
    hello: &Hello,
    binding: Option<&Binding>,
    size: TermSize,
) -> Result<Arc<Terminal>, String> {
    let (env, removals) = compose_env(g, hello, binding)?;

    // Everything a session needs to drive rmux from inside: the `rmux` shim on
    // disk, and — for an agent — the skill that teaches it the CLI. Best-effort
    // both: a session that cannot have them still runs, it just cannot
    // orchestrate. See `session_env`.
    if let Err(e) = session_env::ensure_shim() {
        tracing::debug!(error = %e, "could not write the rmux shim");
    }
    let first_word = hello.login_command.as_deref().and_then(session_env::first_command_word);
    if first_word == Some("claude")
        && let Some(home) = dirs::home_dir().and_then(|h| h.to_str().map(str::to_owned))
        && let Err(e) = rmux_claude_core::skill::install_claude_skill(&home)
    {
        tracing::debug!(error = %e, "could not install the rmux skill for Claude");
    }

    let mut spec = match (&hello.program, &hello.login_command) {
        (Some(program), _) => CommandSpec::new(program).args(hello.args.clone()),
        // `-l -c` and not plain `-c`: the point is the login shell's PATH. The
        // shim directory goes on the line as an assignment prefix, because the
        // profiles a login shell reads can reset an env-only PATH — see
        // `session_env::with_rmux_path`. The *recorded* command stays the line
        // the client sent, so `list` and the app see what they asked for.
        //
        // A set's removals go on the line too, as `unset` before an `exec`: the
        // login shell reads profiles that can export `ANTHROPIC_API_KEY` *after*
        // the daemon built the environment, and a subscription-token session
        // that inherited one would silently bill the API key instead.
        (None, Some(line)) => {
            let line = match session_env::bin_dir() {
                Some(bin) if cfg!(unix) => session_env::with_rmux_path(line, &bin),
                _ => line.clone(),
            };
            let line = session_env::with_unset(&line, &removals);
            CommandSpec::login_shell().arg("-c").arg(line)
        }
        (None, None) => CommandSpec::login_shell(),
    }
    .tty(Tty::Allocate);
    // The daemon spawns locally, so these become real `envp` entries and never
    // appear in an argument list.
    for (key, value) in env {
        spec = spec.env(key, value);
    }

    let command = LocalTarget::new().build_command(&spec).map_err(|e| e.to_string())?;
    let cwd = hello.cwd.as_deref().map(camino::Utf8Path::new);
    Terminal::spawn(&command, cwd, size).map(Arc::new).map_err(|e| e.to_string())
}

/// Run the daemon until the socket is closed.
pub async fn serve() -> anyhow::Result<()> {
    let endpoint = endpoint()?;
    let mut listener = ipc::Listener::bind(&endpoint).await?;

    let sessions = Arc::new(Mutex::new(Sessions::default()));
    tracing::info!(address = %endpoint.address, "rmux agent listening");
    tokio::spawn(monitor(Arc::clone(&sessions)));

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
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn reply<W: AsyncWrite + Unpin>(writer: &mut W, frame: Frame) -> anyhow::Result<()> {
    writer.write_all(&frame.encode()).await?;
    writer.flush().await?;
    Ok(())
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
    let (hello, binding) = loop {
        let mut chunk = [0u8; 4096];
        let n = reader.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "client closed before saying hello");
        buf.extend_from_slice(&chunk[..n]);

        if let Some((frame, used)) = Frame::decode(&buf)? {
            buf.drain(..used);
            match frame {
                Frame::Hello(hello) => break (hello, None),
                Frame::HelloBound { hello, binding } => break (hello, Some(binding)),
                // A client that only delivers environment never opens a session.
                Frame::SetEnv(env) => {
                    merge_env(&mut sessions.lock().env, env);
                    return Ok(());
                }
                // Answered, unlike `SetEnv`: see `Frame::SetKindEnv`. The client
                // waits for this, so a daemon that cannot honour it is loud.
                Frame::SetKindEnv { kind, env } => {
                    let ok = set_kind_env(&mut sessions.lock().kind_env, &kind, env);
                    if !ok {
                        tracing::warn!(%kind, "refused credentials for an unknown agent kind");
                    }
                    return reply(&mut writer, Frame::Ack { ok }).await;
                }
                Frame::SetNamedEnv { kind, set, label, env } => {
                    let result = set_named_env(&mut sessions.lock().named, &kind, &set, label, env);
                    return reply(
                        &mut writer,
                        match result {
                            Ok(()) => Frame::Ack { ok: true },
                            Err(reason) => Frame::Refused { reason },
                        },
                    )
                    .await;
                }
                Frame::GetBinding { session } => {
                    let report = binding_report(&sessions, &session);
                    return reply(&mut writer, Frame::BindingIs(report)).await;
                }
                Frame::Switch { session, member } => {
                    let frame = match switch_to(&sessions, &session, &member).await {
                        Ok(()) => Frame::Ack { ok: true },
                        Err(reason) => Frame::Refused { reason },
                    };
                    return reply(&mut writer, frame).await;
                }
                // Answered here, during the handshake, because listing must not
                // *create* anything. Going through the Hello path to ask what
                // exists would spawn a shell named after the question.
                Frame::List => {
                    return reply(&mut writer, Frame::Sessions(summarise(&sessions))).await;
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
                        let _ = dead.terminal().kill();
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
                    let target = sessions.lock().by_name.get(&session).map(Session::terminal);
                    let ok = match target {
                        Some(terminal) => terminal.write(&data).is_ok(),
                        None => false,
                    };
                    return reply(&mut writer, Frame::Ack { ok }).await;
                }
                // The read twin of `Write`, answered here for the same reason:
                // the caller reads one reply and hangs up, so anything that
                // attaches first writes a replay into a socket already gone.
                //
                // Looks the session up rather than creating — a scrollback read
                // for a name that has ended is an empty buffer, never a new
                // shell.
                Frame::Read { session } => {
                    let scrollback = sessions
                        .lock()
                        .by_name
                        .get(&session)
                        .map(|s| s.terminal().replay().to_vec())
                        .unwrap_or_default();
                    return reply(&mut writer, Frame::Scrollback(scrollback)).await;
                }
                other => anyhow::bail!("expected Hello, got {other:?}"),
            }
        }
    };

    let size = TermSize { cols: hello.cols, rows: hello.rows };
    let bound = binding.is_some();
    let (slot, created) = match open_slot(&sessions, &hello, binding, size) {
        Ok(found) => found,
        // A bound attach is told why, in its own terminal; an unbound one
        // fails the way it always has.
        Err(reason) if bound => {
            tracing::warn!(session = %hello.session, %reason, "refused a bound session");
            return reply(&mut writer, Frame::Refused { reason }).await;
        }
        Err(reason) => anyhow::bail!(reason),
    };
    if bound {
        // Before the replay: the client reads this first, and a daemon that
        // never sends it is one that could not have applied the binding.
        writer.write_all(&Frame::Ack { ok: true }.encode()).await?;
    }

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
    let _ = slot.current().resize(size);

    // --- shell output → client ---------------------------------------------
    let out_slot = Arc::clone(&slot);
    let out = tokio::spawn(async move {
        // Reused across every chunk: this is the path every byte of shell output
        // takes, so allocating per chunk would be the app's busiest allocation.
        let mut frame = Vec::with_capacity(16 * 1024);
        let mut generations = out_slot.generation.subscribe();

        'follow: loop {
            let terminal = out_slot.current();
            generations.borrow_and_update();
            // Replay then stream, atomically — see `Terminal::attach`. Split
            // apart, a reattaching client would miss or duplicate whatever
            // arrived in between.
            let (backlog, mut events) = terminal.attach();
            if !backlog.is_empty() {
                encode_data_into(&mut frame, &backlog);
                if writer.write_all(&frame).await.is_err() {
                    break;
                }
            }
            if let Some(code) = terminal.exit_code()
                && !out_slot.respawning.load(Ordering::SeqCst)
            {
                let _ = writer.write_all(&Frame::Exited { code }.encode()).await;
                break;
            }

            loop {
                match events.recv().await {
                    Ok(TerminalEvent::Output(chunk)) => {
                        encode_data_into(&mut frame, &chunk);
                        if writer.write_all(&frame).await.is_err() {
                            break 'follow;
                        }
                    }
                    Ok(TerminalEvent::Exited { code }) => {
                        // A fallback relaunch: follow the session to its new
                        // process instead of telling the client it ended.
                        if out_slot.respawning.load(Ordering::SeqCst)
                            || generations.has_changed().unwrap_or(false)
                        {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(15),
                                generations.changed(),
                            )
                            .await;
                            continue 'follow;
                        }
                        let _ = writer.write_all(&Frame::Exited { code }.encode()).await;
                        break 'follow;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(chunks = n, "client fell behind; output dropped");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        if out_slot.respawning.load(Ordering::SeqCst) {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(15),
                                generations.changed(),
                            )
                            .await;
                            continue 'follow;
                        }
                        break 'follow;
                    }
                }
            }
        }
    });

    // --- client input → shell ----------------------------------------------
    let result: anyhow::Result<()> = async {
        loop {
            // Drain everything buffered before reading again: terminal input
            // arrives coalesced, and one frame per read would add latency to
            // every keystroke burst. The slot is read per frame so input follows
            // a relaunch too.
            while let Some((frame, used)) = Frame::decode(&buf)? {
                buf.drain(..used);
                match frame {
                    Frame::Data(bytes) => {
                        let _ = slot.current().write(&bytes);
                    }
                    Frame::Resize { cols, rows } => {
                        let _ = slot.current().resize(TermSize { cols, rows });
                    }
                    Frame::Kill { session } => {
                        // Explicit end-of-life: the tab was closed, so the shell
                        // should die rather than linger unreachable.
                        if let Some(dead) = sessions.lock().by_name.remove(&session) {
                            let _ = dead.terminal().kill();
                        }
                        return Ok(());
                    }
                    Frame::SetEnv(env) => {
                        merge_env(&mut sessions.lock().env, env);
                    }
                    // A client has no business announcing these.
                    Frame::Hello(_)
                    | Frame::HelloBound { .. }
                    | Frame::Exited { .. }
                    | Frame::Sessions(_)
                    | Frame::Ack { .. }
                    | Frame::Refused { .. }
                    | Frame::BindingIs(_) => {}
                    // Handshake-only: answered, and an answer has no place in an
                    // attached session's stream.
                    Frame::SetKindEnv { .. }
                    | Frame::SetNamedEnv { .. }
                    | Frame::GetBinding { .. }
                    | Frame::Switch { .. } => {}
                    // Answered only during the handshake, where they cannot be
                    // confused with traffic for an open session. `Write` in
                    // particular: an attached client already has a `Data` path to
                    // its own session, so honouring a one-shot write here would
                    // be a second way to do the same thing — pointed at a session
                    // that need not be the one this connection holds.
                    Frame::List | Frame::Write { .. } | Frame::Read { .. } | Frame::Scrollback(_) => {}
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

/// Find the named session, or start it (unbound).
#[cfg(test)]
fn open_or_attach(
    sessions: &Arc<Mutex<Sessions>>,
    hello: &Hello,
    size: TermSize,
) -> anyhow::Result<(Arc<Terminal>, bool)> {
    let (slot, created) = open_slot(sessions, hello, None, size).map_err(anyhow::Error::msg)?;
    Ok((slot.current(), created))
}

/// Find the named session, or start it bound to `binding`.
///
/// **A live session keeps the binding it was created with.** Reattaching is not
/// the moment to change provider — a conversation that moved account because a
/// pane reconnected would be the silent switch this design forbids. Moving a
/// session is an explicit act ([`Frame::Switch`], or the fallback monitor).
fn open_slot(
    sessions: &Arc<Mutex<Sessions>>,
    hello: &Hello,
    binding: Option<Binding>,
    size: TermSize,
) -> Result<(Arc<TermSlot>, bool), String> {
    let mut guard = sessions.lock();

    if let Some(existing) = guard.by_name.get(&hello.session) {
        // A shell that has exited should not be reattached to — start a new one
        // under the same name rather than handing back a dead terminal.
        if existing.terminal().exit_code().is_none() {
            return Ok((Arc::clone(&existing.slot), false));
        }
        guard.by_name.remove(&hello.session);
    }

    let terminal = spawn_terminal(&guard, hello, binding.as_ref(), size)?;
    let slot = TermSlot::new(terminal);
    guard.by_name.insert(
        hello.session.clone(),
        Session {
            slot: Arc::clone(&slot),
            started: std::time::Instant::now(),
            // The login command is the informative one — it is what carries
            // `claude --resume …`. A bare program name says only "a shell".
            command: hello.login_command.clone().or_else(|| hello.program.clone()),
            attached: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            hello: hello.clone(),
            binding,
            bound_since: fallback::now(),
            last_switch: None,
            switching: false,
            parked: false,
        },
    );
    Ok((slot, true))
}

/// Which set a session is on, with labels. Names only.
fn binding_report(sessions: &Arc<Mutex<Sessions>>, session: &str) -> BindingReport {
    let g = sessions.lock();
    let Some(s) = g.by_name.get(session) else {
        return BindingReport { session: session.to_owned(), ..Default::default() };
    };
    let mut labels = BTreeMap::new();
    if let Some(b) = &s.binding {
        let names = std::iter::once(b.set.clone())
            .chain(b.chain.iter().flat_map(|c| c.members.clone()));
        for name in names {
            if let Some(l) = g.label(&b.kind, &name) {
                labels.insert(name, l);
            }
        }
    }
    BindingReport { session: session.to_owned(), binding: s.binding.clone(), labels, since: s.bound_since }
}

// ── Fallback ─────────────────────────────────────────────────────────────────

/// How often chained sessions are checked for a limit.
const MONITOR_EVERY: std::time::Duration = std::time::Duration::from_secs(3);

/// Watch every chained Claude session for a usage limit, for the life of the
/// daemon. On the host, so a chain keeps working with the app closed.
async fn monitor(sessions: Arc<Mutex<Sessions>>) {
    let mut tick = tokio::time::interval(MONITOR_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        check_chains(&sessions).await;
    }
}

/// One pass of the monitor. Separate so a test can drive it.
async fn check_chains(sessions: &Arc<Mutex<Sessions>>) {
    struct Candidate {
        name: String,
        pid: Option<u32>,
        line: Option<String>,
        scrollback: Vec<u8>,
        bound_since: u64,
        last_switch: Option<u64>,
    }
    let (home, candidates) = {
        let g = sessions.lock();
        let candidates: Vec<Candidate> = g
            .by_name
            .iter()
            .filter(|(_, s)| {
                !s.switching
                    && !s.parked
                    && s.binding.as_ref().is_some_and(|b| b.kind == "claude" && b.chain.is_some())
                    && s.terminal().exit_code().is_none()
            })
            .map(|(name, s)| {
                let t = s.terminal();
                Candidate {
                    name: name.clone(),
                    pid: t.pid(),
                    line: launch_line_of(&s.hello).map(str::to_owned),
                    scrollback: t.replay().to_vec(),
                    bound_since: s.bound_since,
                    last_switch: s.last_switch,
                }
            })
            .collect();
        (g.home(), candidates)
    };
    let Some(home) = home else { return };
    let offset = fallback::utc_offset();
    for c in candidates {
        let now = fallback::now();
        let Some(d) =
            fallback::detect(&home, c.pid, c.line.as_deref(), &c.scrollback, c.bound_since, now, offset)
        else {
            continue;
        };
        if policy::should_act(d.seen_at, c.bound_since, c.last_switch, now) {
            switch_chain(sessions, &c.name, d).await;
        }
    }
}

/// Wait for a freshly launched TUI to draw and go quiet, so typed input lands
/// in a composer that exists.
async fn wait_settled(terminal: &Terminal) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(25);
    let mut last_len = 0usize;
    let mut quiet_since = tokio::time::Instant::now();
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if terminal.exit_code().is_some() {
            return;
        }
        let len = terminal.replay().len();
        if len != last_len {
            last_len = len;
            quiet_since = tokio::time::Instant::now();
        } else if len > 0 && quiet_since.elapsed() >= std::time::Duration::from_millis(1500) {
            return;
        }
    }
}

/// Type a message into a session with the one shared definition of how a
/// message is submitted — text, a settle, then Enter, as separate writes.
async fn submit(terminal: &Terminal, text: &str) {
    for chunk in rmux_claude_core::keys::send_message(text) {
        let _ = terminal.write(&chunk);
        tokio::time::sleep(rmux_claude_core::keys::SUBMIT_SETTLE).await;
    }
}

/// Relaunch a session's process under the same name with a new binding and
/// launch line. Attached clients follow it (see `TermSlot`).
async fn relaunch(
    sessions: &Arc<Mutex<Sessions>>,
    name: &str,
    binding: Binding,
    line: String,
) -> Result<Arc<Terminal>, String> {
    let (slot, old, hello) = {
        let g = sessions.lock();
        let s = g.by_name.get(name).ok_or_else(|| format!("{name} has ended"))?;
        let hello = with_launch_line(s.hello.clone(), line);
        // The new set must resolve *before* anything is killed: a switch that
        // cannot start the new process must not end the old one.
        compose_env(&g, &hello, Some(&binding))?;
        (Arc::clone(&s.slot), s.terminal(), hello)
    };
    let size = old.size();
    slot.respawning.store(true, Ordering::SeqCst);
    let _ = old.kill();
    for _ in 0..150 {
        if old.exit_code().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let spawned = {
        let mut g = sessions.lock();
        let spawned = spawn_terminal(&g, &hello, Some(&binding), size);
        if let Ok(terminal) = &spawned {
            *slot.current.lock() = Arc::clone(terminal);
            if let Some(s) = g.by_name.get_mut(name) {
                s.command = launch_line_of(&hello).map(str::to_owned).or(s.command.take());
                s.hello = hello;
                s.binding = Some(binding);
                s.bound_since = fallback::now();
                s.parked = false;
            }
        }
        spawned
    };
    slot.generation.send_modify(|g| *g += 1);
    slot.respawning.store(false, Ordering::SeqCst);
    spawned
}

/// A chained session hit a limit: move it to the next member, or park it at the
/// end of the chain. Every outcome is logged to `~/.rmux/fallback.json`.
async fn switch_chain(
    sessions: &Arc<Mutex<Sessions>>,
    name: &str,
    detection: fallback::Detection,
) -> Option<FallbackRecord> {
    let now = fallback::now();
    let (home, binding, line) = {
        let mut g = sessions.lock();
        let home = g.home();
        let s = g.by_name.get_mut(name)?;
        if s.switching {
            return None;
        }
        let binding = s.binding.clone()?;
        binding.chain.as_ref()?;
        s.switching = true;
        (home, binding, launch_line_of(&s.hello).map(str::to_owned))
    };
    let mut chain = binding.chain.clone().unwrap_or_default();
    let from = chain.active.clone();
    let hit = &detection.hit;
    let mut decision = chain.on_limit(hit.class, now, hit.resets_at);
    // A member whose set was never delivered is skipped, and the record says so.
    let mut missing = Vec::new();
    for _ in 0..chain.members.len() {
        let Decision::SwitchTo(next) = &decision else { break };
        let loaded = sessions.lock().named.contains_key(&(binding.kind.clone(), next.clone()));
        if loaded || next == env_kind::NO_PROFILE {
            break;
        }
        missing.push(next.clone());
        decision = chain.on_limit(hit.class, now, None);
    }

    let label = |set: &str| sessions.lock().label(&binding.kind, set);
    let mut record = FallbackRecord {
        session: name.to_owned(),
        chain: chain.id.clone(),
        from: from.clone(),
        from_label: label(&from),
        to: None,
        to_label: None,
        reason: hit.class.as_str().to_owned(),
        pattern: hit.pattern.to_owned(),
        line: hit.line.clone(),
        source: detection.source.to_owned(),
        outcome: String::new(),
        at: now,
        resets_at: hit.resets_at,
    };

    match decision {
        Decision::Ignore => {
            if let Some(s) = sessions.lock().by_name.get_mut(name) {
                s.switching = false;
            }
            return None;
        }
        Decision::EndOfChain => {
            record.outcome = if missing.is_empty() {
                "end_of_chain".to_owned()
            } else {
                format!("end_of_chain (not loaded on this host: {})", missing.join(", "))
            };
            if let Some(s) = sessions.lock().by_name.get_mut(name) {
                s.binding = Some(Binding { chain: Some(chain), ..binding.clone() });
                s.last_switch = Some(now);
                s.parked = true;
                s.switching = false;
            }
            tracing::warn!(session = %name, "fallback chain exhausted; the session is waiting");
        }
        Decision::SwitchTo(next) => {
            record.to = Some(next.clone());
            record.to_label = label(&next);
            let new_binding = Binding { kind: binding.kind.clone(), set: next.clone(), chain: Some(chain) };
            let relaunched = match line.as_deref().and_then(|l| policy::resume_line(l, detection.conversation.as_deref())) {
                None => Err("the launch line is not one rmux can resume".to_owned()),
                Some(new_line) => relaunch(sessions, name, new_binding, new_line).await,
            };
            match relaunched {
                Ok(terminal) => {
                    record.outcome = "switched".to_owned();
                    if let Some(s) = sessions.lock().by_name.get_mut(name) {
                        s.last_switch = Some(now);
                    }
                    wait_settled(&terminal).await;
                    submit(&terminal, policy::CONTINUE_MESSAGE).await;
                    tracing::warn!(session = %name, %from, to = %next, "switched account after a limit");
                }
                Err(e) => {
                    record.outcome = format!("failed: {e}");
                    if let Some(s) = sessions.lock().by_name.get_mut(name) {
                        s.last_switch = Some(now);
                        s.parked = true;
                    }
                }
            }
            if let Some(s) = sessions.lock().by_name.get_mut(name) {
                s.switching = false;
            }
        }
    }

    if let Some(home) = home
        && let Err(e) = fallback::append_log(&home, record.clone())
    {
        tracing::warn!(error = %e, "could not record the fallback switch");
    }
    Some(record)
}

/// The operator's "switch back" (or any manual move within the chain): rebind
/// to `member` and relaunch on the same conversation. No message is typed — the
/// operator is there, and decides what the session does next.
async fn switch_to(sessions: &Arc<Mutex<Sessions>>, name: &str, member: &str) -> Result<(), String> {
    let (home, binding, line, pid) = {
        let g = sessions.lock();
        let s = g.by_name.get(name).ok_or_else(|| format!("no session called {name}"))?;
        let binding = s.binding.clone().ok_or_else(|| format!("{name} is not bound to a credential set"))?;
        if s.switching {
            return Err(format!("{name} is already switching"));
        }
        (g.home(), binding, launch_line_of(&s.hello).map(str::to_owned), s.terminal().pid())
    };
    let mut chain = binding.chain.clone().ok_or_else(|| format!("{name} is not on a fallback chain"))?;
    let from = chain.active.clone();
    if !chain.switch_back(member) {
        return Err(format!("{member} is not a member of this session's chain"));
    }
    let conversation = home
        .as_deref()
        .and_then(|h| pid.and_then(|p| fallback::conversation_of_pid(h, p)))
        .or_else(|| line.as_deref().and_then(fallback::conversation_of_line));
    let new_line = line
        .as_deref()
        .and_then(|l| policy::resume_line(l, conversation.as_deref()))
        .ok_or_else(|| "the launch line is not one rmux can resume".to_owned())?;
    let new_binding = Binding { kind: binding.kind.clone(), set: member.to_owned(), chain: Some(chain) };
    relaunch(sessions, name, new_binding, new_line).await?;
    let now = fallback::now();
    if let Some(s) = sessions.lock().by_name.get_mut(name) {
        s.last_switch = Some(now);
    }
    let label = |set: &str| sessions.lock().label(&binding.kind, set);
    let record = FallbackRecord {
        session: name.to_owned(),
        chain: binding.chain.as_ref().map(|c| c.id.clone()).unwrap_or_default(),
        from: from.clone(),
        from_label: label(&from),
        to: Some(member.to_owned()),
        to_label: label(member),
        reason: "operator".to_owned(),
        source: "operator".to_owned(),
        outcome: "switched_back".to_owned(),
        at: now,
        ..Default::default()
    };
    if let Some(home) = home {
        let _ = fallback::append_log(&home, record);
    }
    Ok(())
}

/// Everything the daemon is running, newest last.
///
/// Sorted oldest-first on purpose: age is what identifies a leak, so the ones
/// worth looking at are the ones at the top.
fn summarise(sessions: &Arc<Mutex<Sessions>>) -> Vec<crate::protocol::SessionSummary> {
    let guard = sessions.lock();
    let mut out: Vec<_> = guard
        .by_name
        .iter()
        // A session whose shell has exited is not running, whatever the map
        // still says — reporting it would send the operator to kill a pid that
        // the OS may have reused.
        .filter(|(_, session)| session.terminal().exit_code().is_none())
        .map(|(name, session)| crate::protocol::SessionSummary {
            name: name.clone(),
            pid: session.terminal().pid(),
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
    fn each_kind_s_credentials_reach_only_that_kind_s_sessions() {
        use std::collections::BTreeMap;
        let pairs = |v: &[(&str, &str)]| -> BTreeMap<String, String> {
            v.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        let mut sets = BTreeMap::new();
        assert!(set_kind_env(&mut sets, "deepseek", pairs(&[("DEEPSEEK_API_KEY", "ds")])));
        assert!(set_kind_env(&mut sets, "openai", pairs(&[("OPENAI_API_KEY", "oa")])));
        assert!(set_kind_env(&mut sets, "claude", pairs(&[("ANTHROPIC_BASE_URL", "https://gw")])));

        let keys = |session: &str| -> Vec<String> {
            kind_env_for(&sets, session).map(|(k, _)| k.clone()).collect()
        };
        // The failure this exists to prevent: provider routing leaking across agents.
        assert_eq!(keys("claude-session-1"), ["ANTHROPIC_BASE_URL"]);
        assert_eq!(keys("codex-codexsession-1"), ["OPENAI_API_KEY"]);
        assert_eq!(keys("dsh-dshsession-1"), ["DEEPSEEK_API_KEY"]);
        assert!(keys("term-1").is_empty(), "a shell got a provider credential");
        assert!(keys("pi-pisession-1").is_empty());
    }

    #[test]
    fn switching_a_kind_s_profile_clears_what_the_last_one_set() {
        use std::collections::BTreeMap;
        let mut sets = BTreeMap::new();
        let gw: BTreeMap<String, String> = [
            ("ANTHROPIC_BASE_URL".to_owned(), "https://gw".to_owned()),
            ("ANTHROPIC_AUTH_TOKEN".to_owned(), "tok".to_owned()),
        ]
        .into_iter()
        .collect();
        set_kind_env(&mut sets, "claude", gw);
        // Back to Anthropic: the client sends the clear-set (every managed key
        // empty) and the set must end up empty, not keep the old base URL.
        let clear: BTreeMap<String, String> =
            [("ANTHROPIC_BASE_URL".to_owned(), String::new())].into_iter().collect();
        set_kind_env(&mut sets, "claude", clear);
        assert!(kind_env_for(&sets, "claude-x").next().is_none(), "{sets:?}");
        // A different profile with a different key set replaces, never merges.
        set_kind_env(&mut sets, "claude", [("ANTHROPIC_MODEL".to_owned(), "m".to_owned())].into_iter().collect());
        set_kind_env(&mut sets, "claude", [("ANTHROPIC_API_KEY".to_owned(), "k".to_owned())].into_iter().collect());
        let keys: Vec<&String> = kind_env_for(&sets, "claude-x").map(|(k, _)| k).collect();
        assert_eq!(keys, ["ANTHROPIC_API_KEY"]);
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let mut sets = std::collections::BTreeMap::new();
        assert!(!set_kind_env(&mut sets, "anthropic", Default::default()));
        assert!(sets.is_empty());
    }

    #[tokio::test]
    async fn the_kind_env_frame_is_acknowledged_during_the_handshake() {
        // The client waits for this Ack; without it a daemon that dropped the
        // frame would be indistinguishable from one that applied it.
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        for (kind, expect) in [("openai", true), ("bogus", false)] {
            let (mut client, server) = tokio::io::duplex(4096);
            client
                .write_all(
                    &Frame::SetKindEnv {
                        kind: kind.into(),
                        env: [("OPENAI_API_KEY".to_owned(), "k".to_owned())].into_iter().collect(),
                    }
                    .encode(),
                )
                .await
                .unwrap();
            handle(server, Arc::clone(&sessions)).await.unwrap();
            let mut reply = Vec::new();
            client.read_to_end(&mut reply).await.unwrap();
            let (frame, _) = Frame::decode(&reply).unwrap().unwrap();
            assert_eq!(frame, Frame::Ack { ok: expect }, "{kind}");
        }
        let guard = sessions.lock();
        assert_eq!(guard.kind_env.get("openai").and_then(|e| e.get("OPENAI_API_KEY")).map(String::as_str), Some("k"));
        assert!(guard.env.is_empty(), "a kind-scoped credential leaked into the shared env");
    }

    #[tokio::test]
    async fn a_spawned_session_sees_only_its_own_kind_s_credentials() {
        // End to end through the real spawn path: a process's actual
        // environment, not the selection helper's opinion of it.
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        {
            let mut g = sessions.lock();
            set_kind_env(&mut g.kind_env, "deepseek", [("DEEPSEEK_API_KEY".to_owned(), "ds-secret".to_owned())].into_iter().collect());
            set_kind_env(&mut g.kind_env, "openai", [("OPENAI_API_KEY".to_owned(), "oa-secret".to_owned())].into_iter().collect());
            set_kind_env(&mut g.kind_env, "claude", [("ANTHROPIC_BASE_URL".to_owned(), "https://gw.test".to_owned())].into_iter().collect());
        }
        let size = TermSize { cols: 200, rows: 24 };
        let probe = "echo ENV:${DEEPSEEK_API_KEY:-none}:${OPENAI_API_KEY:-none}:${ANTHROPIC_BASE_URL:-none}:END";
        let mut seen = Vec::new();
        for name in ["claude-kind-probe", "codex-kind-probe", "dsh-kind-probe", "term-kind-probe"] {
            let hello = Hello {
                session: name.into(),
                cwd: None,
                program: Some("sh".into()),
                args: vec!["-c".into(), probe.into()],
                login_command: None,
                env: Default::default(),
                cols: 200,
                rows: 24,
            };
            let (terminal, _) = open_or_attach(&sessions, &hello, size).unwrap();
            let mut line = String::new();
            for _ in 0..100 {
                let out = String::from_utf8_lossy(&terminal.replay()).into_owned();
                if let Some(at) = out.find("ENV:")
                    && let Some(end) = out[at..].find(":END")
                {
                    line = out[at..at + end].to_owned();
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            seen.push(line);
        }
        assert_eq!(
            seen,
            [
                "ENV:none:none:https://gw.test",
                "ENV:none:oa-secret:none",
                "ENV:ds-secret:none:none",
                "ENV:none:none:none",
            ]
        );
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

    /// A scrollback read hands back what the shell wrote, and creates nothing.
    ///
    /// The read twin of the write test. Redstone asks a host for a terminal's
    /// history; the daemon returns its buffer without attaching, and a name that
    /// does not exist is an empty answer rather than a fresh shell.
    #[tokio::test]
    async fn a_read_returns_the_scrollback_and_creates_nothing() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let size = TermSize { cols: 80, rows: 24 };

        // `echo` writes a known line and exits; its output lands in scrollback.
        let hello = Hello {
            session: "live".into(),
            cwd: None,
            program: Some("sh".into()),
            args: vec!["-c".into(), "echo READBACK_MARKER".into()],
            login_command: None,
            env: Default::default(),
            cols: 80,
            rows: 24,
        };
        let (terminal, _) = open_or_attach(&sessions, &hello, size).unwrap();

        // Let the child produce its line.
        for _ in 0..50 {
            if String::from_utf8_lossy(&terminal.replay()).contains("READBACK_MARKER") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        async fn read_once(sessions: &Arc<Mutex<Sessions>>, session: &str) -> Vec<u8> {
            let (client, server) = tokio::io::duplex(1 << 20);
            let (mut client_read, mut client_write) = tokio::io::split(client);
            client_write
                .write_all(&Frame::Read { session: session.into() }.encode())
                .await
                .unwrap();

            handle(server, Arc::clone(sessions)).await.unwrap();

            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                if let Some((Frame::Scrollback(bytes), _)) = Frame::decode(&buf).unwrap() {
                    return bytes;
                }
                let n = client_read.read(&mut chunk).await.unwrap();
                assert!(n > 0, "daemon closed without answering the read");
                buf.extend_from_slice(&chunk[..n]);
            }
        }

        let scrollback = read_once(&sessions, "live").await;
        assert!(
            String::from_utf8_lossy(&scrollback).contains("READBACK_MARKER"),
            "scrollback did not contain the shell's output: {:?}",
            String::from_utf8_lossy(&scrollback),
        );

        // A name nobody holds is an empty buffer, and it must not create.
        let ghost = read_once(&sessions, "ghost").await;
        assert!(ghost.is_empty(), "a read invented output for a missing session");
        assert!(
            !sessions.lock().by_name.contains_key("ghost"),
            "a read created a session"
        );
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
        sessions.lock().by_name.get("young").unwrap().terminal().kill().unwrap();
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

    fn pairs(v: &[(&str, &str)]) -> BTreeMap<String, String> {
        v.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn probe_hello(name: &str, probe: &str) -> Hello {
        Hello {
            session: name.into(),
            cwd: None,
            program: Some("sh".into()),
            args: vec!["-c".into(), probe.into()],
            login_command: None,
            env: Default::default(),
            cols: 200,
            rows: 24,
        }
    }

    async fn read_marker(terminal: &Terminal, start: &str, end: &str) -> String {
        for _ in 0..150 {
            let out = String::from_utf8_lossy(&terminal.replay()).into_owned();
            if let Some(at) = out.find(start)
                && let Some(stop) = out[at..].find(end)
            {
                return out[at..at + stop].to_owned();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        String::new()
    }

    #[tokio::test]
    async fn two_claude_sessions_on_one_daemon_run_on_different_sets() {
        // The thing the per-kind set could not express: two accounts, one host.
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        {
            let mut g = sessions.lock();
            set_named_env(&mut g.named, "claude", "1", Some("Work".into()), pairs(&[("CLAUDE_CODE_OAUTH_TOKEN", "tok-work")])).unwrap();
            set_named_env(&mut g.named, "claude", "2", Some("GLM".into()), pairs(&[("ANTHROPIC_BASE_URL", "https://glm.test"), ("ANTHROPIC_AUTH_TOKEN", "glm")])).unwrap();
            // A per-kind set an older client left behind: a bound session must not see it.
            set_kind_env(&mut g.kind_env, "claude", pairs(&[("ANTHROPIC_MODEL", "stale")]));
        }
        let probe = "echo ENV:${CLAUDE_CODE_OAUTH_TOKEN:-none}:${ANTHROPIC_BASE_URL:-none}:${ANTHROPIC_MODEL:-none}:END";
        let size = TermSize { cols: 200, rows: 24 };
        let mut seen = Vec::new();
        for (name, set) in [("claude-one", "1"), ("claude-two", "2"), ("claude-three", "none")] {
            let binding = Binding { kind: "claude".into(), set: set.into(), chain: None };
            let (slot, _) = open_slot(&sessions, &probe_hello(name, probe), Some(binding), size).unwrap();
            seen.push(read_marker(&slot.current(), "ENV:", ":END").await);
        }
        assert_eq!(seen, ["ENV:tok-work:none:none", "ENV:none:https://glm.test:none", "ENV:none:none:none"]);
    }

    #[tokio::test]
    async fn a_set_s_empty_value_removes_the_shared_account_s_key() {
        // A subscription-token profile must really use the subscription: the
        // stored API-key account (shared env) is taken out, not merely outranked.
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        {
            let mut g = sessions.lock();
            merge_env(&mut g.env, pairs(&[("ANTHROPIC_API_KEY", "sk-ant-api-shared")]));
            set_named_env(
                &mut g.named,
                "claude",
                "5",
                None,
                pairs(&[("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat-x"), ("ANTHROPIC_API_KEY", ""), ("ANTHROPIC_BASE_URL", "")]),
            )
            .unwrap();
        }
        let probe = "echo ENV:${ANTHROPIC_API_KEY:-gone}:${CLAUDE_CODE_OAUTH_TOKEN:-none}:END";
        let binding = Binding { kind: "claude".into(), set: "5".into(), chain: None };
        let (slot, _) = open_slot(&sessions, &probe_hello("claude-tok", probe), Some(binding), TermSize { cols: 200, rows: 24 }).unwrap();
        assert_eq!(read_marker(&slot.current(), "ENV:", ":END").await, "ENV:gone:sk-ant-oat-x");
        // An unbound session still gets the shared account — nothing else changed.
        let (t, _) = open_or_attach(&sessions, &probe_hello("claude-plain", probe), TermSize { cols: 200, rows: 24 }).unwrap();
        assert_eq!(read_marker(&t, "ENV:", ":END").await, "ENV:sk-ant-api-shared:none");
    }

    #[tokio::test]
    async fn a_bound_hello_is_refused_loudly_and_never_spawns_on_another_provider() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        set_named_env(&mut sessions.lock().named, "openai", "3", None, pairs(&[("OPENAI_API_KEY", "k")])).unwrap();
        for (session, binding, expect) in [
            ("claude-x", Binding { kind: "claude".into(), set: "9".into(), chain: None }, "not loaded"),
            ("claude-y", Binding { kind: "openai".into(), set: "3".into(), chain: None }, "cannot run on a openai"),
            ("term-z", Binding { kind: "claude".into(), set: "none".into(), chain: None }, "not an agent session"),
        ] {
            let (mut client, server) = tokio::io::duplex(1 << 16);
            client
                .write_all(&Frame::HelloBound { hello: probe_hello(session, "sleep 5"), binding }.encode())
                .await
                .unwrap();
            handle(server, Arc::clone(&sessions)).await.unwrap();
            let mut reply = Vec::new();
            client.read_to_end(&mut reply).await.unwrap();
            match Frame::decode(&reply).unwrap().unwrap().0 {
                Frame::Refused { reason } => assert!(reason.contains(expect), "{session}: {reason}"),
                other => panic!("{session}: expected a refusal, got {other:?}"),
            }
            assert!(!sessions.lock().by_name.contains_key(session), "{session} was started anyway");
        }
        // Reserved names are refused at delivery.
        assert!(set_named_env(&mut sessions.lock().named, "claude", "none", None, pairs(&[("A", "b")])).is_err());
        assert!(set_named_env(&mut sessions.lock().named, "claude", "a b", None, pairs(&[("A", "b")])).is_err());
    }

    #[tokio::test]
    async fn a_bound_hello_is_acknowledged_before_the_replay() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        set_named_env(&mut sessions.lock().named, "claude", "1", None, pairs(&[("X", "y")])).unwrap();
        let (mut client, server) = tokio::io::duplex(1 << 16);
        let binding = Binding { kind: "claude".into(), set: "1".into(), chain: None };
        client
            .write_all(&Frame::HelloBound { hello: probe_hello("claude-ack", "echo hi; sleep 5"), binding }.encode())
            .await
            .unwrap();
        let task = tokio::spawn(handle(server, Arc::clone(&sessions)));
        let mut buf = vec![0u8; 64];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), client.read(&mut buf)).await.unwrap().unwrap();
        assert_eq!(Frame::decode(&buf[..n]).unwrap().unwrap().0, Frame::Ack { ok: true });
        drop(client);
        let _ = task.await;
        let _ = sessions.lock().by_name.get("claude-ack").unwrap().terminal().kill();
    }

    #[tokio::test]
    async fn a_named_set_is_acknowledged_and_an_old_daemon_s_silence_is_an_error() {
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let (mut client, server) = tokio::io::duplex(4096);
        client
            .write_all(&Frame::SetNamedEnv { kind: "claude".into(), set: "4".into(), label: Some("L".into()), env: pairs(&[("A", "b")]) }.encode())
            .await
            .unwrap();
        handle(server, Arc::clone(&sessions)).await.unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.unwrap();
        assert_eq!(Frame::decode(&reply).unwrap().unwrap().0, Frame::Ack { ok: true });
        assert_eq!(sessions.lock().label("claude", "4").as_deref(), Some("L"));
    }

    /// End to end, in a sandbox: a fake `claude` prints a usage limit and
    /// records it in its transcript; the monitor moves the session to the next
    /// member, relaunches it with `--resume <conversation>` and the new set's
    /// environment, types the continue message, and logs the switch `0600`.
    #[tokio::test]
    async fn a_usage_limit_moves_a_chained_session_to_the_next_member() {
        let home = std::path::PathBuf::from(format!("/tmp/rmux-switch-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("bin")).unwrap();
        let claude = home.join("bin/claude");
        std::fs::write(
            &claude,
            r#"#!/bin/sh
mkdir -p "$SANDBOX/.claude/sessions" "$SANDBOX/.claude/projects/-tmp"
printf '{"pid":%s,"sessionId":"conv-e2e"}' $$ > "$SANDBOX/.claude/sessions/$$.json"
echo "LAUNCH:$*:TOKEN=${CLAUDE_CODE_OAUTH_TOKEN:-none}:KEY=${ANTHROPIC_API_KEY:-none}:END"
if [ "$1" != "--resume" ]; then
  sleep 2
  ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  printf '{"type":"assistant","isApiErrorMessage":true,"timestamp":"%s","message":{"content":[{"type":"text","text":"Claude AI usage limit reached|1999999999"}]}}\n' "$ts" >> "$SANDBOX/.claude/projects/-tmp/conv-e2e.jsonl"
  echo "Claude AI usage limit reached|1999999999"
fi
while read line; do echo "GOT:$line"; done
"#,
        )
        .unwrap();
        std::process::Command::new("chmod").arg("755").arg(&claude).status().unwrap();

        let sessions = Arc::new(Mutex::new(Sessions { home: Some(home.clone()), ..Default::default() }));
        {
            let mut g = sessions.lock();
            merge_env(&mut g.env, pairs(&[("ANTHROPIC_API_KEY", "shared-key")]));
            set_named_env(&mut g.named, "claude", "a", Some("Personal".into()), pairs(&[("CLAUDE_CODE_OAUTH_TOKEN", "tok-a")])).unwrap();
            set_named_env(&mut g.named, "claude", "b", Some("Work".into()), pairs(&[("CLAUDE_CODE_OAUTH_TOKEN", "tok-b"), ("ANTHROPIC_API_KEY", "")])).unwrap();
        }
        let mut hello = probe_hello("claude-e2e", claude.to_str().unwrap());
        hello.env.insert("SANDBOX".into(), home.to_str().unwrap().into());
        let chain = policy::ChainState {
            id: "chain-1".into(),
            members: vec!["a".into(), "b".into()],
            active: "a".into(),
            ..Default::default()
        };
        let binding = Binding { kind: "claude".into(), set: "a".into(), chain: Some(chain) };
        let (slot, _) = open_slot(&sessions, &hello, Some(binding), TermSize { cols: 200, rows: 24 }).unwrap();
        let first = slot.current();
        assert_eq!(read_marker(&first, "LAUNCH:", ":END").await, "LAUNCH::TOKEN=tok-a:KEY=shared-key");

        // Drive the monitor until it acts (the fake waits 2s before the limit).
        let mut switched = false;
        for _ in 0..40 {
            check_chains(&sessions).await;
            if !Arc::ptr_eq(&slot.current(), &first) {
                switched = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        assert!(switched, "the monitor never switched: {}", String::from_utf8_lossy(&first.replay()));
        let second = slot.current();
        assert_eq!(
            read_marker(&second, "LAUNCH:", ":END").await,
            "LAUNCH:--resume conv-e2e:TOKEN=tok-b:KEY=none",
            "relaunched on the next member, resuming the same conversation"
        );
        assert!(!read_marker(&second, "GOT:Continue", "account").await.is_empty(), "the continue message was not typed");

        let report = binding_report(&sessions, "claude-e2e");
        let b = report.binding.unwrap();
        assert_eq!(b.set, "b");
        assert!(b.chain.as_ref().unwrap().exhausted.contains_key("a"));
        assert_eq!(b.chain.unwrap().exhausted["a"].resets_at, Some(1_999_999_999));

        let log = fallback::read_log(&home);
        assert_eq!(log.switches.len(), 1, "{log:?}");
        let r = &log.switches[0];
        assert_eq!((r.from.as_str(), r.to.as_deref(), r.outcome.as_str()), ("a", Some("b"), "switched"));
        assert_eq!((r.from_label.as_deref(), r.to_label.as_deref()), (Some("Personal"), Some("Work")));
        assert_eq!(r.source, "transcript");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(fallback::log_path(&home)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Debounce: the same limit record must not cause a second switch.
        for _ in 0..3 {
            check_chains(&sessions).await;
        }
        assert!(Arc::ptr_eq(&slot.current(), &second), "one limit caused two switches");
        assert_eq!(fallback::read_log(&home).switches.len(), 1);

        let _ = second.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}
