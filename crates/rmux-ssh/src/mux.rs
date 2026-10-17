//! Connection multiplexing via OpenSSH's `ControlMaster`.
//!
//! One master `ssh` process authenticates and holds the transport open; every
//! later `ssh` to the same host connects to its Unix socket instead of doing a
//! fresh handshake. Opening a fifth terminal on a host therefore costs a process
//! spawn and nothing else — no key exchange, no re-auth, no 2FA prompt.
//!
//! We deliberately use `ControlPersist=no` so the master dies with rmux rather
//! than lingering in the user's session after the app quits.
//!
//! ## One connection is not enough, and more connections is not the opposite of
//! ## one connection
//!
//! sshd's `MaxSessions` caps the channels on **one** connection — default 10 —
//! and rmux rides everything over the master: every terminal, every Claude and
//! pi pane, every file listing, metrics sample and transcript tail. Measured on
//! a live host: a master up for 1h51m answering `ssh -O check`, **39 client
//! commands queued onto it**, and three panes sitting on `CONNECTING . 163s`.
//! The host was fine — a direct `ssh` with `ControlPath=none` returned
//! instantly. Everything past the tenth channel waits for a slot, and the
//! sessions holding the slots are themselves stuck behind the same wall, so the
//! wait never ends.
//!
//! So this module holds a **pool** of masters per destination
//! ([`MasterPool`]), each carrying at most [`MAX_CHANNELS_PER_MASTER`], and
//! opens another only when every existing one is provably full.
//!
//! **That is not a retreat from the registry below, and the distinction is the
//! whole design.** The bug the registry fixed was *uncontrolled duplication* —
//! `SshTarget::new` built a master per target, so racers spawned blindly and
//! unlinked each other's sockets. This is *controlled pooling*: a member is
//! opened under the pool's own lock, by a caller that has just scanned every
//! existing member and found no free slot, and its socket name says which
//! member it is. Duplication is two masters nobody asked for; pooling is a
//! second master asked for by name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rmux_transport::{ChannelLease, NoConsoleWindow, SshHostId, Tty};
use tokio::process::{Child, Command};

/// Where the master stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MasterState {
    Stopped,
    Starting,
    Running,
    /// Multiplexing is unavailable on this platform; each command dials its own
    /// connection.
    Unsupported,
}

/// **One master per destination, for the whole process.**
///
/// This registry is the fix for a measured leak: twelve `ssh -N` masters for
/// five hosts, with only five sockets on disk — seven processes holding a TCP
/// connection nothing could ever multiplex over.
///
/// The cause was that `SshTarget::new` built a *fresh* `ControlMaster` every
/// time, each with its own state lock, and six subsystems (terminal, claude,
/// files, metrics, agent, claude_status) resolve their own target for the same
/// host. Two of them racing both read `Stopped`, both got `false` from
/// `check_alive`, both called `clear_stale_socket` — so the second **deleted the
/// socket the first had just bound**, orphaning a live master unreachable by
/// anything — and both spawned.
///
/// A per-instance guard cannot serialise that. Sharing the instance can, and it
/// is the only thing that can: the lock has to be the same lock.
///
/// It is now a registry of [`MasterPool`]s rather than of single masters, and
/// the key has not changed: one *destination* still resolves to one place that
/// decides, holding the one lock that can serialise the racers above.
static REGISTRY: LazyLock<Mutex<HashMap<String, Arc<MasterPool>>>> =
    LazyLock::new(Default::default);

/// How many channels rmux will put on **one** master connection.
///
/// Eight, which is a deliberate margin under OpenSSH's default `MaxSessions
/// 10` rather than a tuned number. The margin exists because **the server's
/// real value is unknown to us**: `MaxSessions` can be set lower, we do not
/// read the host's `sshd_config`, and nothing in the protocol reports it — a
/// refused channel comes back as "administratively prohibited", which reads as
/// a permissions problem and is not one (see `explain` in `lib.rs`). Leaving
/// two spare also leaves room for the operator's own `ssh` to the same host
/// piggybacking on our master, which `check_alive` deliberately allows.
///
/// It is a *ceiling per master*, never a ceiling on sessions: past it the pool
/// opens another master, and a session is never refused for want of a slot.
pub const MAX_CHANNELS_PER_MASTER: usize = 8;

/// How many masters one destination may have.
///
/// Six, so forty-eight channels, which is far past what any operator drives by
/// hand on one machine; it is a guard against a runaway caller rather than a
/// budget anyone should reach. At the cap a **session is still never refused**
/// — it is put on the least-loaded master and may push it past
/// [`MAX_CHANNELS_PER_MASTER`], because losing a shell is worse than a channel
/// that has to queue. One-shots never open a member at all; see
/// [`MasterPool::acquire`].
const MAX_MASTERS_PER_HOST: usize = 6;

/// How long a one-shot waits for a slot before failing with a reason.
///
/// A bounded wait, because the failure this file exists to fix was an
/// *unbounded* one: nothing timed out, so a queued channel waited for ever and
/// the pane said `CONNECTING` until someone killed it. Five seconds is long
/// enough to ride out a burst — six subsystems resolve a host at once when a
/// session opens — and short enough that a metrics widget reports "the
/// connection is full" instead of hanging.
const ONE_SHOT_WAIT: Duration = Duration::from_secs(5);

/// How often that wait re-checks for a freed slot.
const SLOT_POLL: Duration = Duration::from_millis(100);

/// How many **destinations** may hold live masters at once.
///
/// A guard against accumulation rather than a tuned number — the count is one
/// per host the operator actually uses, and eight distinct hosts open at the
/// same moment is already well past normal. It **evicts**, never refuses:
/// turning the ninth host away would surface as "this host will not connect"
/// with nothing naming the cause, which is a worse failure than the one this
/// file exists to fix.
///
/// It counts destinations rather than master processes on purpose. A pooled
/// host legitimately holds several, and counting those would make a busy
/// machine evict itself.
const MAX_LIVE_MASTERS: usize = 8;

/// How long a destination must have gone untouched before it may be evicted.
///
/// Age **and** emptiness. The original note here said whether a master still
/// carries live channels "is not cheaply knowable from here" — `ssh -O check`
/// reports that the master is alive, not how many sessions ride it — and
/// deduced that age alone had to stand in for it. That premise is gone: a
/// channel is now a [`ChannelSlot`] held for its lifetime, so the count is
/// exact and [`enforce_master_cap`] refuses to evict a pool that is carrying
/// anything. The age test stays as the second half of the question, because a
/// pool with no channels may still be a host the operator is about to use.
const EVICTABLE_AFTER: Duration = Duration::from_secs(10 * 60);

#[derive(Debug)]
pub struct ControlMaster {
    host: SshHostId,
    /// Which member of its destination's pool this is. `0` is the primary, and
    /// its socket name is the one every earlier build already used.
    index: usize,
    socket: PathBuf,
    /// Channels currently riding this master, as leases held by their owners.
    ///
    /// The number sshd is counting, tracked here because nothing else can see
    /// it: a terminal holds its channel for its whole life, so this cannot be
    /// derived from what is in flight. Every increment hands out a
    /// [`ChannelSlot`] whose `Drop` is the decrement.
    channels: AtomicUsize,
    state: Mutex<MasterState>,
    child: Mutex<Option<Child>>,
    /// Held across the whole start, so N concurrent callers produce one `ssh -N`.
    ///
    /// A `tokio` mutex rather than the `parking_lot` one beside it, because this
    /// one is genuinely held across `.await` points; `state` never is.
    start_lock: tokio::sync::Mutex<()>,
    /// Bumped once per completed start attempt, so a caller that queued on
    /// `start_lock` can tell "nobody has started since I looked" from "the
    /// winner's attempt already covers mine".
    start_epoch: AtomicU64,
    /// What the last completed start attempt returned, for those waiters to
    /// adopt. Cloneable, hence `String` rather than `anyhow::Error`.
    last_outcome: Mutex<Option<Result<MasterState, String>>>,
    /// Unix millis of the last time anything used this master.
    last_used: AtomicU64,
    /// Test seam: when set, `start_once` counts a spawn and reports success
    /// instead of running a real `ssh`. See `concurrent_starts_spawn_once`.
    #[cfg(test)]
    fake_spawns: Mutex<Option<Arc<AtomicU64>>>,
}

impl ControlMaster {
    /// Member 0, unpooled. Only the tests build one of these directly; every
    /// production master comes from a [`MasterPool`], which is what decides
    /// *which* member it is.
    #[cfg(test)]
    fn new(host: SshHostId) -> Self {
        Self::new_member(host, 0)
    }

    /// Member `index` of a destination's pool.
    fn new_member(host: SshHostId, index: usize) -> Self {
        let socket = control_path_for_member(&host, index);
        let state =
            if Self::is_supported() { MasterState::Stopped } else { MasterState::Unsupported };
        Self {
            host,
            index,
            socket,
            channels: AtomicUsize::new(0),
            state: Mutex::new(state),
            child: Mutex::new(None),
            start_lock: tokio::sync::Mutex::new(()),
            start_epoch: AtomicU64::new(0),
            last_outcome: Mutex::new(None),
            last_used: AtomicU64::new(now_millis()),
            #[cfg(test)]
            fake_spawns: Mutex::new(None),
        }
    }

    /// The **primary** master for this destination — the same one every time.
    ///
    /// Keyed by the identity the socket path already uses (alias, user, port),
    /// so a different user or a different port is a different master, exactly as
    /// it is a different socket.
    ///
    /// This is member 0 of the destination's pool, and its socket path is
    /// byte-identical to what every earlier build used. Callers that want *a*
    /// channel ask [`MasterPool::acquire`]; this is for the things that are
    /// about the destination as a whole — its state, its warm-keep, its
    /// teardown.
    pub fn for_host(host: &SshHostId) -> Arc<ControlMaster> {
        MasterPool::for_host(host).primary()
    }

    /// Channels currently counted against this master.
    pub fn live_channels(&self) -> usize {
        self.channels.load(Ordering::Acquire)
    }

    /// Take one of this master's [`MAX_CHANNELS_PER_MASTER`] slots, if one is
    /// free.
    ///
    /// A compare-exchange loop rather than a `fetch_add` and a check: the
    /// add-then-check form overshoots under concurrency and then has to put the
    /// channel back, and a count that goes over the limit before it comes back
    /// is a count the ninth caller read.
    fn try_reserve(self: &Arc<Self>) -> Option<ChannelSlot> {
        let mut seen = self.channels.load(Ordering::Acquire);
        loop {
            if seen >= MAX_CHANNELS_PER_MASTER {
                return None;
            }
            match self.channels.compare_exchange_weak(
                seen,
                seen + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.touch();
                    return Some(ChannelSlot { master: Arc::clone(self), counted: true });
                }
                Err(actual) => seen = actual,
            }
        }
    }

    /// Take a slot whether or not one is free.
    ///
    /// Only for a session at [`MAX_MASTERS_PER_HOST`], where the alternative is
    /// refusing a terminal. The channel may then have to queue on the far side,
    /// which is a slow shell; refusing is no shell at all.
    fn reserve_regardless(self: &Arc<Self>) -> ChannelSlot {
        self.channels.fetch_add(1, Ordering::AcqRel);
        self.touch();
        ChannelSlot { master: Arc::clone(self), counted: true }
    }

    /// A slot that counts nothing, for a platform with no shared connection to
    /// fill.
    fn unaccounted(self: &Arc<Self>) -> ChannelSlot {
        ChannelSlot { master: Arc::clone(self), counted: false }
    }

    /// Whether OpenSSH connection multiplexing works here.
    ///
    /// `ControlMaster` requires Unix domain sockets, which Windows OpenSSH does
    /// not provide — it fails with "getsockname failed". On Windows every command
    /// opens its own connection until the `russh` path lands.
    pub const fn is_supported() -> bool {
        !cfg!(windows)
    }

    pub fn state(&self) -> MasterState {
        *self.state.lock()
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket
    }

    /// Unix millis of the last use, for the idle-eviction sweep.
    fn last_used(&self) -> u64 {
        self.last_used.load(Ordering::Relaxed)
    }

    fn touch(&self) {
        self.last_used.store(now_millis(), Ordering::Relaxed);
    }

    /// `ssh` options every client invocation needs so it finds the master.
    pub fn client_options(&self) -> Vec<String> {
        if !Self::is_supported() {
            return Vec::new();
        }
        // Every channel rmux opens comes through here, which makes this the
        // truest signal of "something is still using this host".
        self.touch();
        vec![
            "-o".to_owned(),
            format!("ControlPath={}", self.socket.display()),
            // Never start a master implicitly from a client command — that would
            // race several terminals opened at once into competing masters.
            "-o".to_owned(),
            "ControlMaster=no".to_owned(),
        ]
    }

    /// Start the master if it is not already up.
    ///
    /// **Concurrent callers wait for the in-flight start and adopt its result.**
    /// This used to `bail!` on seeing `Starting`, which was defensible when every
    /// target had a master of its own — a second caller there was a second
    /// master. With one shared instance a concurrent caller is the *ordinary*
    /// case (six subsystems resolve the same host at once when a session opens),
    /// and failing them would turn the fix into a different bug.
    pub async fn ensure_started(&self) -> anyhow::Result<MasterState> {
        if !Self::is_supported() {
            return Ok(MasterState::Unsupported);
        }
        self.touch();

        if self.state() == MasterState::Running {
            return Ok(MasterState::Running);
        }

        // Read the epoch *before* queueing: if it has moved by the time the lock
        // is ours, a start ran while we waited and its answer is ours too.
        let seen = self.start_epoch.load(Ordering::SeqCst);
        let _turn = self.start_lock.lock().await;
        if self.start_epoch.load(Ordering::SeqCst) != seen
            && let Some(outcome) = self.last_outcome.lock().clone()
        {
            return outcome.map_err(anyhow::Error::msg);
        }

        let outcome = self.start_once().await;

        // Recorded before the epoch is bumped, so a waiter that sees the new
        // epoch is guaranteed to find the answer that goes with it.
        *self.last_outcome.lock() =
            Some(outcome.as_ref().map(|state| *state).map_err(ToString::to_string));
        self.start_epoch.fetch_add(1, Ordering::SeqCst);

        if outcome.is_ok() {
            enforce_master_cap();
        }
        outcome
    }

    /// The actual start. Only ever reached by one caller at a time.
    async fn start_once(&self) -> anyhow::Result<MasterState> {
        #[cfg(test)]
        {
            let fake = self.fake_spawns.lock().clone();
            if let Some(counter) = fake {
                counter.fetch_add(1, Ordering::SeqCst);
                // Long enough that every other caller is genuinely queued behind
                // the lock while this "start" is in flight.
                tokio::time::sleep(Duration::from_millis(50)).await;
                *self.state.lock() = MasterState::Running;
                return Ok(MasterState::Running);
            }
        }

        // Piggyback on a master the user already has open (from their own shell,
        // or a previous rmux run) rather than starting a competing one.
        if self.check_alive().await {
            *self.state.lock() = MasterState::Running;
            return Ok(MasterState::Running);
        }

        // The socket exists but nothing answered, so it is debris from a run that
        // died without cleaning up — a crash, a kill -9, a machine that slept.
        //
        // This must be removed before starting a master, and the failure mode if
        // it is not is nasty: OpenSSH will not bind over an existing socket file.
        // It prints "ControlSocket ... already exists, disabling multiplexing"
        // and then connects *anyway* without multiplexing, so the poll below
        // never sees a live master and spins for the full timeout before failing.
        // The symptom is every host hanging for a minute after any unclean exit.
        //
        // Reaching here at all is now the privilege of the single winning
        // starter: it was two callers racing into *this* line that deleted a
        // socket the other had just bound.
        self.clear_stale_socket().await;

        *self.state.lock() = MasterState::Starting;

        if let Some(dir) = self.socket.parent() {
            tokio::fs::create_dir_all(dir).await.ok();
        }

        let mut cmd = Command::new("ssh");
        cmd.no_console_window();
        cmd.arg("-N") // no remote command; this process only holds the transport
            .arg("-o")
            .arg(format!("ControlPath={}", self.socket.display()))
            .arg("-o")
            .arg("ControlMaster=yes")
            // Die with rmux instead of outliving it in the user's session.
            .arg("-o")
            .arg("ControlPersist=no")
            // Notice a dead link in ~90s rather than hanging on a half-open socket
            // — but not sooner: a *busy* host (heavy load, into swap) answers a
            // keepalive slowly, and a 45s window dropped the master on such a host
            // mid-session, detaching every pane through it. 90s tolerates the stall;
            // a genuinely dead host still surfaces, and the pane auto-reconnects
            // either way (`ClaudePanel`'s exit poll).
            .arg("-o")
            .arg("ServerAliveInterval=15")
            .arg("-o")
            .arg("ServerAliveCountMax=6");

        // **The key rmux installed, offered to the connection that authenticates.**
        //
        // Every client command is multiplexed through this master, so this is the
        // one place credentials are actually checked. `~/.ssh/rmux_<host>_ed25519`
        // is not a name OpenSSH tries by itself, so without this the key sits on
        // the host being ignored and the password prompt returns for ever.
        for arg in crate::keys::identity_args_for(&self.host.label()) {
            cmd.arg(arg);
        }

        if let Some(user) = &self.host.user {
            cmd.arg("-l").arg(user);
        }
        if let Some(port) = self.host.port {
            cmd.arg("-p").arg(port.to_string());
        }
        cmd.arg(&self.host.alias);

        for (k, v) in super::askpass::env_for_gui_prompts() {
            cmd.env(k, v);
        }

        // stdin must stay open: closing it makes OpenSSH exit immediately when a
        // ProxyJump is in play.
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        // Taken now, while we still own the handle. `ssh` writes the *reason* for
        // every failure here — "cloudflared: command not found", "Permission
        // denied", "Host key verification failed" — and without this the operator
        // is shown `exit status: 255` and nothing else. 255 is OpenSSH's generic
        // "it did not work"; on its own it is indistinguishable between a missing
        // ProxyCommand helper, a rejected key and an unreachable host.
        let mut stderr = child.stderr.take();
        *self.child.lock() = Some(child);

        // The socket appears asynchronously, after authentication completes.
        // Poll rather than sleeping a fixed amount so 2FA has room to finish.
        for _ in 0..600 {
            if self.socket.exists() && self.check_alive().await {
                *self.state.lock() = MasterState::Running;
                self.touch();
                return Ok(MasterState::Running);
            }
            // The lock is taken and released *before* the await below. A
            // `parking_lot` guard is `!Send`, so holding one across an await
            // makes this future `!Send` — and every `#[tauri::command]` that
            // reaches it then fails to compile, several files away from the
            // cause. Scoping it here keeps the borrow off the await point.
            let exited = {
                let mut child = self.child.lock();
                child.as_mut().and_then(|c| c.try_wait().ok().flatten())
            };

            if let Some(status) = exited {
                *self.state.lock() = MasterState::Stopped;
                let reason = read_stderr(stderr.take()).await;
                anyhow::bail!(
                    "ssh master for {} exited early ({status}){}",
                    self.host.alias,
                    if reason.is_empty() { String::new() } else { format!(": {reason}") }
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        *self.state.lock() = MasterState::Stopped;
        anyhow::bail!("timed out waiting for ssh master to {}", self.host.alias)
    }

    /// Guarantee a live master, reviving one that has died.
    ///
    /// Unlike [`ensure_started`], this does **not** trust the cached `Running`
    /// state. A master killed by a dropped link or a `ServerAlive` timeout
    /// leaves the state reading `Running` while the socket is dead — and
    /// `ensure_started` would then short-circuit on that state and never
    /// restart it. Verifying with [`check_alive`] first, and clearing a stale
    /// `Running` when the socket is gone, is what lets a *polled* caller (the
    /// metrics widget) keep the master warm instead of paying a full handshake
    /// on every tick.
    ///
    /// [`ensure_started`]: ControlMaster::ensure_started
    /// [`check_alive`]: ControlMaster::check_alive
    pub async fn ensure_alive(&self) -> anyhow::Result<MasterState> {
        if !Self::is_supported() {
            return Ok(MasterState::Unsupported);
        }
        self.touch();
        if self.check_alive().await {
            *self.state.lock() = MasterState::Running;
            return Ok(MasterState::Running);
        }
        // The socket is gone (or never came up). Drop a stale `Running` so
        // `ensure_started` restarts rather than short-circuiting — but leave a
        // genuine `Starting` alone, so a concurrent start is not disturbed.
        {
            let mut state = self.state.lock();
            if *state == MasterState::Running {
                *state = MasterState::Stopped;
            }
        }
        self.ensure_started().await
    }

    /// Ask OpenSSH whether the master is live (`ssh -O check`).
    pub async fn check_alive(&self) -> bool {
        if !Self::is_supported() || !self.socket.exists() {
            return false;
        }
        Command::new("ssh")
            .no_console_window()
            .arg("-O")
            .arg("check")
            .arg("-o")
            .arg(format!("ControlPath={}", self.socket.display()))
            .arg(&self.host.alias)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Remove a socket file left behind by a dead master.
    ///
    /// Only ever called once [`check_alive`] has reported nothing listening, so
    /// this cannot delete a socket another process is still using.
    ///
    /// [`check_alive`]: ControlMaster::check_alive
    pub(crate) async fn clear_stale_socket(&self) -> bool {
        if !self.socket.exists() {
            return false;
        }
        match tokio::fs::remove_file(&self.socket).await {
            Ok(()) => {
                tracing::debug!(socket = %self.socket.display(), "removed a stale control socket");
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not remove a stale control socket");
                false
            }
        }
    }

    /// Tear the master down.
    pub async fn stop(&self) {
        if Self::is_supported() && self.socket.exists() {
            let _ = Command::new("ssh")
                .no_console_window()
                .arg("-O")
                .arg("exit")
                .arg("-o")
                .arg(format!("ControlPath={}", self.socket.display()))
                .arg(&self.host.alias)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.start_kill();
        }
        *self.state.lock() = MasterState::Stopped;
    }

    /// Kill masters left over from a previous run that nothing can ever use.
    ///
    /// **Called once on startup, before anything opens a connection.** That
    /// ordering is load-bearing: a master of our own that is mid-start has not
    /// created its socket yet and would look exactly like the orphans below.
    ///
    /// Two kinds are reaped, and both were measured on a live machine: `ssh -N`
    /// processes whose `ControlPath` file does not exist at all (the orphaned-at-
    /// birth case this module's registry now prevents — seven of twelve), and
    /// ones whose socket exists but has no listener (debris from an unclean
    /// exit). A master that **answers** is never touched: a previous run's live
    /// master is exactly what `check_alive`'s piggyback wants to find.
    ///
    /// Candidates are identified by the `ControlPath=` argument pointing inside
    /// our own `~/.rmux/mux` directory — never by a bare `pgrep ssh`, which
    /// would kill connections the operator opened by hand.
    pub async fn reap_orphans() {
        if !Self::is_supported() {
            return;
        }
        reap_orphans_impl().await;
    }
}

/// What a channel is *for*, which is what decides whether it may be refused.
///
/// The two are not a tuning knob, they are two different failures. A session
/// holds its channel until the operator closes it, so a plain semaphore over
/// "channels per host" would turn a hang-under-load into a hard ceiling on
/// sessions per host — six panes on one machine is ordinary, and the ninth
/// terminal would simply never open. A one-shot is the opposite: it is over in
/// a moment, a thousand of them run per hour, and when there is no room the
/// only acceptable answer is to say so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    /// A channel held for as long as the operator keeps it: a terminal, a
    /// Claude pane, a pi pane. **Never refused** — it opens another master
    /// instead.
    Session,
    /// A channel for one command: a file listing, a metrics sample, a
    /// transcript tail, `uname`. Waits briefly for a slot and then **fails with
    /// a reason** rather than waiting for ever.
    OneShot,
}

impl From<Tty> for ChannelKind {
    /// A pty is the tell. Everything rmux keeps open — a shell, an agent
    /// attach, a Claude or pi TUI — asks for `Tty::Allocate`, and everything
    /// that parses stdout asks for `Tty::None` (it has to: a rendered screen is
    /// not a file listing).
    fn from(tty: Tty) -> Self {
        match tty {
            Tty::Allocate => ChannelKind::Session,
            Tty::None => ChannelKind::OneShot,
        }
    }
}

/// One channel's slot on one master, released when this is dropped.
#[derive(Debug)]
pub struct ChannelSlot {
    master: Arc<ControlMaster>,
    /// False for the no-multiplexing platforms, where nothing was counted.
    counted: bool,
}

impl ChannelSlot {
    /// The master this channel must ride. **Not any master for the host** —
    /// the slot was taken on this one, so the `ControlPath` has to be its own
    /// or the accounting describes a connection the command never used.
    pub fn master(&self) -> &Arc<ControlMaster> {
        &self.master
    }

    /// Hand the slot to whoever owns the spawned process.
    pub fn into_lease(self) -> ChannelLease {
        ChannelLease::held_by(Box::new(self))
    }
}

impl Drop for ChannelSlot {
    fn drop(&mut self) {
        if self.counted {
            self.master.channels.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// The masters for **one destination**.
///
/// N of them, each carrying at most [`MAX_CHANNELS_PER_MASTER`] channels, with
/// a new one opened only when every existing one is full. The pool is the unit
/// the registry holds, so there is still exactly one place per destination that
/// decides — which is what makes the decision serialisable at all.
#[derive(Debug)]
pub struct MasterPool {
    host: SshHostId,
    /// Member 0 always exists and is the primary; later members are appended.
    /// The lock is held for *scan-and-open together*, never across an await:
    /// two callers that both scan and both find the pool full would otherwise
    /// both open a member, which is the uncontrolled duplication this module
    /// exists to prevent.
    members: Mutex<Vec<Arc<ControlMaster>>>,
    /// Test seam: when set, every member of this pool reports a successful
    /// start without running `ssh`, and counts the attempt. See the pool tests.
    #[cfg(test)]
    fake: Mutex<Option<Arc<AtomicU64>>>,
}

impl MasterPool {
    fn new(host: SshHostId) -> Self {
        let primary = Arc::new(ControlMaster::new_member(host.clone(), 0));
        Self {
            host,
            members: Mutex::new(vec![primary]),
            #[cfg(test)]
            fake: Mutex::new(None),
        }
    }

    /// The pool for this destination — **the same one every time**.
    pub fn for_host(host: &SshHostId) -> Arc<MasterPool> {
        let key = identity_key(host);
        let mut registry = REGISTRY.lock();
        if let Some(existing) = registry.get(&key) {
            existing.primary().touch();
            return Arc::clone(existing);
        }
        let pool = Arc::new(MasterPool::new(host.clone()));
        registry.insert(key, Arc::clone(&pool));
        pool
    }

    /// Member 0: the one whose socket path is what every earlier build used.
    pub fn primary(&self) -> Arc<ControlMaster> {
        Arc::clone(&self.members.lock()[0])
    }

    /// Every master this destination currently has.
    pub fn members(&self) -> Vec<Arc<ControlMaster>> {
        self.members.lock().clone()
    }

    /// Channels counted across the whole pool.
    pub fn live_channels(&self) -> usize {
        self.members.lock().iter().map(|m| m.live_channels()).sum()
    }

    /// Reserve a channel, and say which master it rides.
    ///
    /// The order matters and is the fix:
    ///
    /// 1. **Reuse before opening.** Every existing member is scanned first, so
    ///    a freed slot is taken rather than paying a handshake for a master
    ///    nobody needs.
    /// 2. **A session opens a member rather than waiting.** It is never
    ///    refused: a terminal lost because a metrics poller filled the
    ///    connection is unacceptable.
    /// 3. **A one-shot waits, briefly, and then fails with a reason.** It never
    ///    opens a member — one-shots are what *fill* a connection, and letting
    ///    a poll open connections would spend a handshake per tick.
    pub async fn acquire(&self, kind: ChannelKind) -> anyhow::Result<ChannelSlot> {
        // Nothing is multiplexed here, so there is no per-connection counter to
        // run into and nothing to account. Never waits, never refuses.
        if !ControlMaster::is_supported() {
            return Ok(self.primary().unaccounted());
        }

        /// What the locked scan decided.
        enum Decision {
            /// A slot on a master that is already up.
            Took(ChannelSlot),
            /// A slot on a member just opened, which still has to be started.
            Opened(ChannelSlot),
            /// Nothing free, and this caller may not open anything. Carries the
            /// member count, for the message.
            Full(usize),
        }

        let deadline = tokio::time::Instant::now() + ONE_SHOT_WAIT;
        loop {
            let decision = {
                let mut members = self.members.lock();
                if let Some(slot) = members.iter().find_map(ControlMaster::try_reserve) {
                    Decision::Took(slot)
                } else if kind == ChannelKind::OneShot {
                    Decision::Full(members.len())
                } else if members.len() < MAX_MASTERS_PER_HOST {
                    let member = Arc::new(ControlMaster::new_member(
                        self.host.clone(),
                        next_member_index(&members),
                    ));
                    #[cfg(test)]
                    {
                        *member.fake_spawns.lock() = self.fake.lock().clone();
                    }
                    members.push(Arc::clone(&member));
                    let slot = member
                        .try_reserve()
                        .expect("a master opened this instant has every slot free");
                    Decision::Opened(slot)
                } else {
                    // At the cap. A session is still never refused — the
                    // least-loaded master takes it and may go over.
                    let least = members
                        .iter()
                        .min_by_key(|m| m.live_channels())
                        .expect("a pool always has its primary");
                    Decision::Took(least.reserve_regardless())
                }
            };

            match decision {
                Decision::Took(slot) => return Ok(slot),
                Decision::Opened(slot) => {
                    // Started here rather than left to the client command,
                    // which would connect directly and unmultiplexed. A failure
                    // is logged and *not* fatal: that direct connection is a
                    // working terminal without multiplexing, which beats no
                    // terminal.
                    if let Err(e) = slot.master().ensure_started().await {
                        tracing::warn!(
                            host = %self.host.alias,
                            member = slot.master().index,
                            error = %e,
                            "could not open another control master; this channel \
                             dials its own connection"
                        );
                    } else {
                        tracing::info!(
                            host = %self.host.alias,
                            member = slot.master().index,
                            "opened another control master: the existing ones each \
                             carry {MAX_CHANNELS_PER_MASTER} channels"
                        );
                    }
                    return Ok(slot);
                }
                Decision::Full(masters) => {
                    if tokio::time::Instant::now() >= deadline {
                        anyhow::bail!(
                            "{}: every multiplexed connection to this host is full \
                             ({masters} x {MAX_CHANNELS_PER_MASTER} channels, the server's \
                             own MaxSessions defaults to 10). Close a session on it, or \
                             try again.",
                            self.host.alias
                        );
                    }
                    tokio::time::sleep(SLOT_POLL).await;
                }
            }
        }
    }

    /// Tear every master down and keep only the primary.
    ///
    /// `disconnect` means the operator asked for this host to be let go, so the
    /// pooled members go with it; member 0 stays as the object the destination
    /// is known by, stopped, ready to start again.
    pub async fn stop_all(&self) {
        let members = self.members();
        for member in &members {
            member.stop().await;
        }
        let mut live = self.members.lock();
        live.truncate(1);
    }

    /// Unix millis of the most recent use of any member.
    fn last_used(&self) -> u64 {
        self.members.lock().iter().map(|m| m.last_used()).max().unwrap_or_default()
    }

    /// Is any member of this pool up?
    fn is_running(&self) -> bool {
        self.members.lock().iter().any(|m| m.state() == MasterState::Running)
    }
}

/// The lowest member index not currently in use.
///
/// **Not `members.len()`.** `stop_all` and any future removal can leave a gap,
/// and `len()` would then hand a new member an index another one already holds
/// — two masters sharing one socket, which is precisely the failure the
/// fingerprinted socket names exist to prevent.
fn next_member_index(members: &[Arc<ControlMaster>]) -> usize {
    (0..).find(|i| !members.iter().any(|m| m.index == *i)).expect("usize is not exhaustible")
}

/// Evict idle masters once there are more than [`MAX_LIVE_MASTERS`].
///
/// Least-recently-used first, and only ones untouched for [`EVICTABLE_AFTER`].
/// The master that has just started is by definition the most recently used, so
/// it can never be its own victim.
fn enforce_master_cap() {
    let pools: Vec<Arc<MasterPool>> = REGISTRY.lock().values().cloned().collect();
    let mut live: Vec<Arc<MasterPool>> = pools.into_iter().filter(|p| p.is_running()).collect();
    if live.len() <= MAX_LIVE_MASTERS {
        return;
    }

    live.sort_by_key(|p| p.last_used());
    let over = live.len() - MAX_LIVE_MASTERS;
    let now = now_millis();
    let victims: Vec<Arc<MasterPool>> = live
        .into_iter()
        .filter(|p| {
            // **Never evict a connection something is riding.** A pooled
            // master can carry six panes and still not be *touched* for ten
            // minutes — `client_options` runs when a channel is built, not
            // while it lives — so age on its own would have killed exactly the
            // sessions this file exists to keep alive.
            p.live_channels() == 0
                && Duration::from_millis(now.saturating_sub(p.last_used())) >= EVICTABLE_AFTER
        })
        .take(over)
        .collect();

    if victims.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for victim in victims {
            tracing::info!(
                host = %victim.host.alias,
                idle_ms = now_millis().saturating_sub(victim.last_used()),
                "evicting an idle destination: more than {MAX_LIVE_MASTERS} are open"
            );
            victim.stop_all().await;
        }
    });
}

#[cfg(not(unix))]
async fn reap_orphans_impl() {}

#[cfg(unix)]
async fn reap_orphans_impl() {
    let dir = mux_dir();
    let Ok(out) = Command::new("ps")
        .no_console_window()
        .args(["-eo", "pid=,args="])
        .stdin(Stdio::null())
        .output()
        .await
    else {
        tracing::debug!("could not list processes; skipping the control-master sweep");
        return;
    };

    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((pid, socket)) = parse_master_line(line, &dir) else { continue };

        let reason = if !socket.exists() {
            "its control socket does not exist"
        } else if socket_answers(&socket).await {
            continue;
        } else {
            let _ = tokio::fs::remove_file(&socket).await;
            "its control socket has no listener"
        };

        tracing::info!(pid, socket = %socket.display(), "reaping a leaked ssh master: {reason}");
        let _ = Command::new("kill")
            .no_console_window()
            .arg("-TERM")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

/// Does anything hold the listening end of this socket?
///
/// A plain connect rather than `ssh -O check`: it needs no destination argument
/// (which would have to be parsed back out of the process's argv, and a
/// mis-parse here kills the wrong thing), it spawns nothing, and it errs in the
/// safe direction — a socket that accepts is kept, whatever it then says.
#[cfg(unix)]
async fn socket_answers(path: &Path) -> bool {
    tokio::net::UnixStream::connect(path).await.is_ok()
}

/// One `ps` row, if it is an `ssh -N` master of **ours**.
///
/// Pure so the identification can be tested without processes — and it is the
/// part worth testing, because everything downstream of it is a `kill`.
// Only the Unix sweep calls it; on Windows there is no ControlMaster to reap.
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_master_line(line: &str, mux_dir: &Path) -> Option<(u32, PathBuf)> {
    let line = line.trim();
    let (pid, args) = line.split_once(char::is_whitespace)?;
    let pid: u32 = pid.parse().ok()?;
    let args = args.trim_start();

    // It must be `ssh` itself, and a master: `-N` (hold the transport, run no
    // remote command) is what every master rmux starts carries and what an
    // ordinary session does not.
    let program = args.split_whitespace().next()?;
    if Path::new(program).file_name()? != "ssh" {
        return None;
    }
    if !args.split_whitespace().any(|a| a == "-N") {
        return None;
    }

    // And the ControlPath must be inside our own directory. An `ssh -N` the
    // operator started by hand — a tunnel, a jump host — is none of our
    // business, and killing one would be unforgivable.
    let socket = args
        .split_whitespace()
        .find_map(|a| a.strip_prefix("ControlPath="))
        .map(PathBuf::from)?;
    if socket.parent() != Some(mux_dir) || socket.extension()? != "sock" {
        return None;
    }
    Some((pid, socket))
}

fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

impl Drop for ControlMaster {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.start_kill();
            // Killing the master leaves its socket file behind, and that debris
            // is what makes the *next* connection hang. Remove it here as well
            // as on the way in, so an ordinary quit leaves nothing to trip over.
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

/// Socket path for a host.
///
/// Kept short on purpose: a Unix socket path is capped near 104 bytes on macOS,
/// and OpenSSH's own `%C` hash exists for exactly this reason. A long
/// `~/Library/Application Support/...` prefix plus a descriptive host name blows
/// that limit and fails with a confusing bind error.
/// Socket path for member `index` of a destination's pool.
///
/// **Member 0 is byte-identical to what `control_path_for` always produced**,
/// and that is load-bearing rather than tidy: `short_hash`'s input is still
/// `identity_key` and nothing is appended, so an upgrade finds the master it
/// left behind instead of orphaning it — a reap away from every live session on
/// the machine. Later members get a `-<index>` suffix, which keeps them
/// distinguishable from the primary on sight (and in `ps`, which is where the
/// 39 queued channels were counted), while still living in `~/.rmux/mux` with
/// a `.sock` extension so the orphan reaper recognises them as ours.
fn control_path_for_member(host: &SshHostId, index: usize) -> PathBuf {
    let mut dir = mux_dir();
    let stem = short_hash(host);
    if index == 0 {
        dir.push(format!("{stem}.sock"));
    } else {
        dir.push(format!("{stem}-{index}.sock"));
    }
    dir
}

/// Where every control socket lives.
fn mux_dir() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
    dir.push(".rmux");
    dir.push("mux");
    dir
}

/// What makes one destination different from another: the alias verbatim, plus
/// the overrides that change *which authenticated session* it is.
///
/// The registry and the socket name are keyed by the same string on purpose —
/// two masters that would share a socket are the same master.
fn identity_key(host: &SshHostId) -> String {
    format!(
        "{}|{}|{}",
        host.alias,
        host.user.as_deref().unwrap_or(""),
        host.port.map(|p| p.to_string()).unwrap_or_default()
    )
}

/// Short stable hash of the full destination (FNV-1a, 64-bit).
fn short_hash(host: &SshHostId) -> String {
    let key = identity_key(host);

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;


    /// **The assertion that pins the whole fix.**
    ///
    /// Measured before it: twelve `ssh -N` masters for five hosts, seven of them
    /// holding a connection with no socket anyone could multiplex over. The
    /// cause was one `ControlMaster` per `SshTarget`, and `resolved()` builds a
    /// target per call, for six subsystems, per host.
    #[test]
    fn one_master_per_destination_for_the_whole_process() {
        let host = SshHostId::new("registry-identity-test");
        let a = ControlMaster::for_host(&host);
        let b = ControlMaster::for_host(&host);
        assert!(Arc::ptr_eq(&a, &b), "the same destination must hand back the same master");

        // A different alias is a different machine.
        let other = ControlMaster::for_host(&SshHostId::new("registry-identity-other"));
        assert!(!Arc::ptr_eq(&a, &other));

        // A different user is a different authenticated session, and a different
        // port is a different destination — both already get their own socket,
        // so both must get their own master or two masters share one socket.
        let as_root = ControlMaster::for_host(&SshHostId {
            alias: "registry-identity-test".to_owned(),
            user: Some("root".to_owned()),
            port: None,
        });
        assert!(!Arc::ptr_eq(&a, &as_root));

        let on_2222 = ControlMaster::for_host(&SshHostId {
            alias: "registry-identity-test".to_owned(),
            user: None,
            port: Some(2222),
        });
        assert!(!Arc::ptr_eq(&a, &on_2222));
        assert!(!Arc::ptr_eq(&as_root, &on_2222));

        // The registry key and the socket name must stay the same identity, or
        // two entries could resolve to one socket.
        assert_ne!(a.socket_path(), as_root.socket_path());
        assert_ne!(a.socket_path(), on_2222.socket_path());
    }

    /// N concurrent callers, exactly one `ssh -N`.
    ///
    /// Opening a session resolves the same host from six subsystems at once, so
    /// this is the ordinary case rather than a corner. It used to be *two* bugs
    /// at once: each caller had its own state lock, and a caller that did manage
    /// to observe `Starting` was failed with an error.
    #[tokio::test]
    async fn concurrent_starts_spawn_once() {
        if !ControlMaster::is_supported() {
            return;
        }
        // Not registered: a fake spawn counter stands in for `ssh`, so nothing
        // here touches the network, the filesystem or a real host.
        let master = Arc::new(ControlMaster::new(SshHostId::new("concurrent-start-test")));
        let spawns = Arc::new(AtomicU64::new(0));
        *master.fake_spawns.lock() = Some(Arc::clone(&spawns));

        let mut tasks = Vec::new();
        for _ in 0..6 {
            let master = Arc::clone(&master);
            tasks.push(tokio::spawn(async move { master.ensure_started().await }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().unwrap(), MasterState::Running);
        }

        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "six callers must produce one master, not six"
        );

        // And a later caller short-circuits on the running state rather than
        // starting a second one.
        assert_eq!(master.ensure_started().await.unwrap(), MasterState::Running);
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
    }

    /// A concurrent caller waits for the in-flight start and is *told* what
    /// happened, rather than being failed for arriving second.
    #[tokio::test]
    async fn a_second_caller_is_not_refused_for_arriving_during_a_start() {
        if !ControlMaster::is_supported() {
            return;
        }
        let master = Arc::new(ControlMaster::new(SshHostId::new("starting-not-an-error")));
        *master.fake_spawns.lock() = Some(Arc::new(AtomicU64::new(0)));
        *master.state.lock() = MasterState::Starting;

        // The old code matched on this state and bailed with "already starting".
        assert_eq!(master.ensure_started().await.unwrap(), MasterState::Running);
    }

    /// The identification that stands between this sweep and a `kill`.
    #[test]
    fn only_our_own_masters_are_reap_candidates() {
        let dir = PathBuf::from("/Users/x/.rmux/mux");
        let ours = "4321 ssh -N -o ControlPath=/Users/x/.rmux/mux/deadbeef0badf00d.sock \
                    -o ControlMaster=yes -o ControlPersist=no devbox";
        assert_eq!(
            parse_master_line(ours, &dir),
            Some((4321, PathBuf::from("/Users/x/.rmux/mux/deadbeef0badf00d.sock")))
        );

        // An ordinary session the operator is sitting in.
        assert_eq!(parse_master_line("77 ssh devbox", &dir), None);
        // Their own tunnel: `-N`, but nothing to do with us.
        assert_eq!(parse_master_line("78 ssh -N -L 8080:localhost:80 devbox", &dir), None);
        // Their own multiplexed connection, with a ControlPath of their own.
        assert_eq!(
            parse_master_line("79 ssh -N -o ControlPath=/Users/x/.ssh/cm-devbox.sock devbox", &dir),
            None
        );
        // Something else entirely that merely mentions the path.
        assert_eq!(
            parse_master_line(
                "80 tail -f /Users/x/.rmux/mux/deadbeef0badf00d.sock",
                &dir
            ),
            None
        );
        // Not a pid.
        assert_eq!(parse_master_line("CMD ssh -N", &dir), None);
        assert_eq!(parse_master_line("", &dir), None);
    }

    #[test]
    fn the_master_cap_is_a_ceiling_not_a_refusal() {
        // Named here so the number cannot drift silently: it is a guard against
        // accumulation, and it evicts rather than turning a host away.
        assert_eq!(MAX_LIVE_MASTERS, 8);
        assert!(EVICTABLE_AFTER >= Duration::from_secs(60));
    }

    #[test]
    fn socket_path_stays_within_the_unix_socket_length_limit() {
        // macOS caps sun_path near 104 bytes; exceeding it fails at bind time
        // with an error that does not mention length.
        let host = SshHostId::new("a-very-long-descriptive-production-hostname.internal.example.com");
        let path = control_path_for_member(&host, 0);
        assert!(path.to_string_lossy().len() < 100, "control path too long: {}", path.display());
    }

    #[test]
    fn distinct_destinations_get_distinct_sockets() {
        let a = control_path_for_member(&SshHostId::new("host-a"));
        let b = control_path_for_member(&SshHostId::new("host-b"));
        assert_ne!(a, b);

        // Same alias but a different user is a different authenticated session
        // and must not share a master.
        let root = control_path_for_member(&SshHostId {
            alias: "host-a".to_owned(),
            user: Some("root".to_owned()),
            port: None,
        });
        assert_ne!(a, root);
    }

    #[test]
    fn socket_path_is_stable_across_calls() {
        let host = SshHostId::new("devbox");
        assert_eq!(control_path_for_member(&host, 0), control_path_for_member(&host, 0));
    }

    #[test]
    fn client_options_never_start_a_competing_master() {
        let master = ControlMaster::new(SshHostId::new("devbox"));
        let opts = master.client_options();
        if ControlMaster::is_supported() {
            assert!(opts.iter().any(|o| o == "ControlMaster=no"));
            assert!(opts.iter().any(|o| o.starts_with("ControlPath=")));
        } else {
            assert!(opts.is_empty());
        }
    }

    // Binds a Unix socket to produce the debris, so it can only run where there
    // are Unix sockets. ControlMaster is a POSIX-only feature anyway.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_socket_is_cleared_before_starting_a_master() {
        // Reproduces the debris a crashed run leaves: binding a listener creates
        // the socket file, and dropping the listener does NOT remove it.
        let master = ControlMaster::new(SshHostId::new("stale-socket-test"));
        let socket = master.socket_path().clone();

        if let Some(dir) = socket.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        drop(listener);

        assert!(socket.exists(), "dropping a listener should leave the file behind");
        assert!(!master.check_alive().await, "nothing is listening on it");

        // Left in place, OpenSSH would refuse to bind and quietly disable
        // multiplexing, making every connection hang until the timeout.
        assert!(master.clear_stale_socket().await);
        assert!(!socket.exists(), "the stale socket should be gone");

        // Clearing again is harmless — the path is simply absent.
        assert!(!master.clear_stale_socket().await);
    }

    #[tokio::test]
    async fn unsupported_platforms_degrade_instead_of_failing() {
        let master = ControlMaster::new(SshHostId::new("devbox"));
        if !ControlMaster::is_supported() {
            assert_eq!(master.state(), MasterState::Unsupported);
            assert_eq!(master.ensure_started().await.unwrap(), MasterState::Unsupported);
            // The warm-keep path degrades the same way, so the metrics poller
            // does not error every tick on a host with no multiplexing.
            assert_eq!(master.ensure_alive().await.unwrap(), MasterState::Unsupported);
        }
    }

    #[tokio::test]
    async fn ensure_alive_does_not_trust_a_stale_running_state() {
        // The bug this guards: a master dies (dropped link, ServerAlive timeout)
        // but the cached state still reads `Running`. `ensure_started` alone
        // short-circuits on that and never restarts — so a metrics poll then
        // pays a full handshake forever. `ensure_alive` must verify the socket
        // is actually live and clear the stale state before it tries again.
        if !ControlMaster::is_supported() {
            return;
        }
        // An unresolvable host, so the restart attempt fails fast rather than
        // dialling anything real; bounded by a timeout so a pathological ssh
        // config cannot hang the suite either way.
        let master = ControlMaster::new(SshHostId::new("rmux.invalid.example.test"));
        assert!(!master.check_alive().await, "no socket, so nothing is alive");
        *master.state.lock() = MasterState::Running; // pretend a dead master left this behind

        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(20), master.ensure_alive()).await;

        assert_ne!(
            master.state(),
            MasterState::Running,
            "a dead socket must not leave the cached state reading Running",
        );
    }
}

/// What `ssh` said before it gave up.
///
/// Trimmed to the last few lines and stripped of the noise OpenSSH prints on the
/// way down. The line that matters is almost always the last one — the earlier
/// ones are `debug1:` chatter or a banner — and an error card that reproduces
/// forty lines of transcript is one nobody reads.
async fn read_stderr(stderr: Option<tokio::process::ChildStderr>) -> String {
    use tokio::io::AsyncReadExt as _;

    let Some(mut stderr) = stderr else { return String::new() };
    let mut buf = Vec::new();
    // The process has already exited, so this cannot block on a live pipe.
    let _ = stderr.read_to_end(&mut buf).await;

    summarise_stderr(&String::from_utf8_lossy(&buf))
}

/// The useful part of what `ssh` printed.
///
/// Pure, so the filtering is testable without a real failing connection — the
/// two cases below are transcripts from actual runs against a `cloudflared`
/// host, which is where this behaviour was pinned down.
pub(crate) fn summarise_stderr(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // Verbose tracing and the version banner explain nothing about *why*,
        // and they crowd out the line that does.
        .filter(|l| !l.starts_with("debug") && !l.starts_with("OpenSSH_"))
        .collect();

    // Last three at most: a failing ProxyCommand prints its own message and then
    // ssh prints one of its own, and both are worth seeing.
    let keep = lines.len().saturating_sub(3);
    lines[keep..].join(" \u{b7} ")
}

#[cfg(test)]
mod stderr_tests {
    use super::summarise_stderr;

    /// Exit 255 is OpenSSH's only failure code, so the *reason* is the whole
    /// diagnostic. These two transcripts differ in cause and are identical in
    /// status — which is why discarding stderr made them indistinguishable.
    #[test]
    fn a_missing_proxycommand_helper_is_reported() {
        // Measured: a Finder-launched app has no /opt/homebrew/bin on PATH.
        let out = summarise_stderr(
            "/bin/sh: line 0: exec: cloudflared: not found\nConnection closed by UNKNOWN port 65535\n",
        );
        assert!(out.contains("cloudflared: not found"), "{out}");
        assert!(out.contains("Connection closed"), "{out}");
    }

    #[test]
    fn a_rejected_credential_is_reported_differently() {
        let out = summarise_stderr("deploy@example.invalid: Permission denied (publickey,password).\n");
        assert!(out.contains("Permission denied"), "{out}");
        assert!(!out.contains("not found"), "the two causes must not blur: {out}");
    }

    #[test]
    fn the_banner_and_debug_chatter_are_dropped() {
        let out = summarise_stderr(
            "OpenSSH_9.8p1, LibreSSL 3.3.6\ndebug1: Reading configuration data\ndebug1: Connecting\nHost key verification failed.\n",
        );
        assert_eq!(out, "Host key verification failed.");
    }

    #[test]
    fn nothing_printed_yields_nothing_rather_than_punctuation() {
        // The caller appends ": {reason}" only when this is non-empty; a lone
        // separator would read as a truncated message.
        assert_eq!(summarise_stderr(""), "");
        assert_eq!(summarise_stderr("\n  \n"), "");
    }
}
