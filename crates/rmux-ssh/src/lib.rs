//! SSH targets, built on the **system `ssh` binary** rather than a Rust SSH
//! implementation.
//!
//! This is the single most consequential decision in the transport layer, so it
//! is worth stating why. Real-world SSH usage depends on an enormous surface:
//! `~/.ssh/config` with `Match`, `Include` and `%h`/`%p`/`%r` token expansion;
//! `ProxyJump` and `ProxyCommand`; agent forwarding; OpenSSH certificates;
//! FIDO/`sk-` keys; PKCS#11; keyboard-interactive 2FA. No Rust crate implements
//! all of it, and the ones that come closest cover perhaps 70% of the config
//! grammar. Shelling out inherits every one of those features permanently and for
//! free — if it works in the user's terminal, it works in rmux.
//!
//! The cost is process-spawn overhead per channel, which [`ControlMaster`]
//! removes: one long-lived master connection owns the authenticated session and
//! every subsequent `ssh` invocation rides it over a Unix socket, so opening a
//! second terminal costs no handshake and no reauthentication.
//!
//! Windows OpenSSH has no `ControlMaster` (it needs Unix domain sockets), which is
//! the one place this design does not reach. That gap is documented in
//! [`mux::ControlMaster::is_supported`] and is where `russh` will be slotted in.

use async_trait::async_trait;
use rmux_transport::{
    ChannelLease, CommandSpec, LowLatencyOutcome, NoConsoleWindow, Output, Platform,
    ResolvedCommand, SshHostId, Target, TargetId, Tty, spec_to_shell_line,
};

pub mod askpass;
pub mod forward;
pub mod config;
pub mod keys;
pub mod mosh;
pub mod mosh_install;
pub mod mux;
pub mod winshell;

pub use config::{ConfigHost, list_hosts};
pub use mosh::{
    Availability as MoshAvailability, MIN_SERVER_VERSION as MOSH_MIN_SERVER_VERSION,
    Refusal as MoshRefusal, Version as MoshVersion,
};
pub use mosh_install::Outcome as MoshInstallOutcome;
pub use mux::{ChannelKind, ControlMaster, MAX_CHANNELS_PER_MASTER, MasterPool, MasterState};
pub use winshell::RemoteShell;

/// An SSH host as a [`Target`].
#[derive(Debug)]
pub struct SshTarget {
    id: TargetId,
    host: SshHostId,
    /// **Shared, not owned.** `ControlMaster::for_host` returns the one master
    /// for this destination, so every target for a host — terminal, claude,
    /// files, metrics, agent, status — serialises on the same lock and rides the
    /// same connection. Building a fresh one per target is what leaked twelve
    /// `ssh -N` processes for five hosts; see `mux::REGISTRY`.
    master: std::sync::Arc<ControlMaster>,
    /// **Where a channel comes from.** The destination's pool of masters: the
    /// primary above plus any opened because every existing one was carrying
    /// its full [`MAX_CHANNELS_PER_MASTER`]. Shared with every other target for
    /// this host, for the same reason the master is.
    pool: std::sync::Arc<MasterPool>,
    platform: parking_lot::RwLock<Option<Platform>>,
    /// What the last low-latency *request* actually got.
    ///
    /// Recorded rather than recomputed, and never requiring a re-probe: a
    /// caller that asked for a low-latency session has to be able to find out
    /// that it did not get one, and why. A session that quietly is not what was
    /// asked for is the "stale means silently wrong" failure — everything
    /// works, so nothing looks wrong, and the thing requested is simply absent.
    low_latency: parking_lot::RwLock<Option<LowLatency>>,
}

/// What a [`CommandSpec::low_latency`] request resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowLatency {
    /// Granted: the session runs over mosh.
    Mosh,
    /// Requested and refused, with the reason to show the operator.
    FellBackToSsh(mosh::Refusal),
}

/// `ssh`'s own exit code for "I could not do this at all".
///
/// It is the one value OpenSSH reserves for itself, and it is deliberately
/// indistinguishable from any remote exit code — which is why it has to be
/// separated out before anything is inferred about the *remote* machine.
const SSH_FAILURE: i32 = 255;


/// Did `ssh` fail, rather than the command it was asked to run?
///
/// Split out so the rule can be tested without a host: it is the difference
/// between "this machine is unreachable" and "this machine is Windows", and
/// those two send the reader to completely different places.
fn is_transport_failure(status: i32) -> bool {
    status == SSH_FAILURE
}


/// How many times to establish a connection before giving up.
///
/// Three, not more. A `ProxyCommand` that is genuinely broken fails the same way
/// every time, and turning one visible error into ten seconds of silence is a
/// worse experience than the error.
const CONNECT_ATTEMPTS: usize = 3;

/// Pause between attempts.
///
/// Long enough for a proxy that lost a race to release whatever it was
/// contending for, short enough that a *real* failure still surfaces quickly:
/// two retries add under a second and a half before the operator sees anything.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_millis(600);

/// Is this failure worth another attempt?
///
/// ## Why a retry is warranted at all
///
/// A `ProxyCommand` host connects through a helper process — `cloudflared`, a
/// bastion, an `aws ssm` wrapper — and when that helper dies before a socket
/// exists, OpenSSH reports the peer it never got:
///
/// ```text
/// Connection closed by UNKNOWN port 65535
/// ```
///
/// `UNKNOWN port 65535` is the tell: there was no peer address, so the failure
/// happened in the proxy rather than on the far host. Some of these are
/// genuinely transient — the helper is warming up, refreshing a token, or
/// declining one of several connections opened at the same instant. On Windows
/// rmux opens several at once by necessity, because there is no `ControlMaster`
/// to share, which is why this shows up there and not on macOS.
///
/// ## What must never be retried, and why it matters more than the retry
///
/// **Authentication failures.** `ssh` already makes two or three password
/// attempts of its own inside a single connection, so retrying multiplies them.
/// Measured today against a real host: repeated failed logins from rmux got this
/// machine banned mid-session, and a `fail2ban` ban reads afterwards as "the
/// server is down" — a far more confusing failure than the one being papered
/// over. A wrong password is also not going to become right by asking again.
///
/// **Host-key problems** are refusals, not blips: retrying a changed host key
/// would hammer a host precisely when the honest answer is that something is
/// wrong.
///
/// So the rule is an allow-list of transport symptoms, *and* a veto on anything
/// that names a credential or a key.
fn is_worth_retrying(stderr: &str) -> bool {
    let reason = stderr.to_ascii_lowercase();

    // The veto comes first: a credential failure that also happens to mention a
    // closed connection must not slip through on the allow-list below.
    let refusal = [
        "permission denied",
        "authentication failed",
        "too many authentication failures",
        "host key verification failed",
        "remote host identification has changed",
        "no supported authentication methods",
        "administratively prohibited",
    ];
    if refusal.iter().any(|r| reason.contains(r)) {
        return false;
    }

    // A proxy that died, or a connection that never completed. `port 65535` is
    // listed explicitly because it is the signature of a ProxyCommand failure
    // even when the wording around it changes between OpenSSH releases.
    let transient = [
        "port 65535",
        "connection closed",
        "connection reset",
        "connection refused",
        "connection timed out",
        "timed out",
        "broken pipe",
        "kex_exchange_identification",
        "banner exchange",
    ];
    transient.iter().any(|t| reason.contains(t))
}

/// Turn ssh's stderr into something that names the fix.
///
/// One case is worth singling out. rmux rides every channel over a single
/// `ControlMaster` connection, and sshd's **`MaxSessions` caps the channels on
/// one connection at 10 by default**. Several sessions on one host — each with
/// terminals, a Claude, metrics polling and file reads — pass that quietly, and
/// the eleventh channel is refused with "administratively prohibited".
///
/// That phrase reads like a permissions or policy problem with the *account*,
/// which is the wrong place to look entirely: nothing is prohibited, a counter
/// is full. Measured on a host with `MaxSessions` unset and 25 sshd processes
/// for one user.
fn explain(stderr: &str) -> String {
    let reason = stderr.trim();
    if reason.is_empty() {
        return ": ssh exited 255 without saying why".to_owned();
    }
    if reason.contains("administratively prohibited") {
        return format!(
            ": {reason}\n\nThis is usually sshd's MaxSessions (default 10) rather than a \
             permissions problem — rmux multiplexes every channel over one connection, so \
             several sessions on one host reach it. Raise MaxSessions in the host's \
             /etc/ssh/sshd_config, or use fewer sessions against it."
        );
    }
    format!(": {reason}")
}

impl SshTarget {
    pub fn new(host: SshHostId) -> Self {
        let pool = MasterPool::for_host(&host);
        let master = pool.primary();
        Self {
            id: TargetId::Ssh(host.clone()),
            host,
            master,
            pool,
            platform: parking_lot::RwLock::new(None),
            low_latency: parking_lot::RwLock::new(None),
        }
    }

    pub fn host(&self) -> &SshHostId {
        &self.host
    }

    pub fn master(&self) -> &ControlMaster {
        &self.master
    }

    /// Bring up the master connection and learn the remote platform.
    ///
    /// Safe to call repeatedly; the master is only started once.
    ///
    /// The mosh probe runs here, **after** the connection has been proven, and
    /// deliberately not before: a host that cannot be reached must produce one
    /// clear error, not an extra failed login first. Repeated `connect` calls
    /// cost nothing — the answer is remembered per host for the run.
    pub async fn connect(&self) -> anyhow::Result<Platform> {
        // The mosh probe is deliberately **not** here. It costs one round
        // trip per host per run, and on the link this feature exists for that
        // is ~270ms added to every connect — paid by every operator, including
        // the ones who never turn low latency on. A caller that means to ask
        // for it awaits `probe_mosh()` itself; `build_command` is sync and can
        // only read the cache, so an unprobed host falls back to ssh with a
        // reason rather than silently pretending.
        self.connect_inner().await
    }

    /// Settle whether mosh can be used for this host, probing at most once per
    /// destination per run.
    ///
    /// One multiplexed `command -v` over the master that is already open, with
    /// no TTY — so it costs no handshake and cannot be mistaken for a session.
    pub async fn probe_mosh(&self) -> mosh::Availability {
        let settled = mosh::availability_for(&self.host, &MoshProbe { target: self }).await;

        // **The one actionable refusal.** A missing local client and an
        // unsupported platform are facts about this machine; a missing
        // `mosh-server` is one package away on a host rmux is already talking
        // to, and the operator asked for it to be done rather than offered.
        //
        // Fired and forgotten on purpose: this function is awaited by
        // `connect`-adjacent paths and by the TRANSPORT panel, and neither may
        // wait on a package install. See `mosh_install` for the two ways this
        // could hurt and what stops each of them.
        if settled == mosh::Availability::Refused(mosh::Refusal::RemoteServerMissing) {
            self.spawn_mosh_job(mosh_install::Job::Install);
        } else if settled.is_available() {
            // **The other half of the same problem.** A host can have a
            // `mosh-server` that is present and too old to be usable for what
            // rmux does with it — see `mosh::MIN_SERVER_VERSION` for the
            // measured reason. That is not a refusal, so it does not go through
            // `Availability` at all; it is the same background, non-blocking,
            // once-per-host attempt against the same ledger.
            //
            // A version rmux could not read is `None`, and `is_outdated` reads
            // that as "leave it alone". Guessing would mean upgrading packages
            // on someone else's server because a string changed format.
            let current = mosh::cached_server_version(&self.host);
            if mosh::is_outdated(current)
                && let Some(from) = current
            {
                self.spawn_mosh_job(mosh_install::Job::Upgrade(from));
            }
        }

        settled
    }

    /// Start a background attempt to install or upgrade `mosh-server` on this
    /// host.
    ///
    /// Nothing here awaits anything. The claim is taken *synchronously* before
    /// the task is spawned, so a second target probing the same host a
    /// millisecond later sees it and does not run a second `sudo`; and so the
    /// UI, which asks for the status immediately after the probe returns, finds
    /// something to await rather than concluding nothing is happening.
    fn spawn_mosh_job(&self, job: mosh_install::Job) {
        // Off means off: nothing attempted, nothing written, and no claim — so
        // the panel shows the plain refusal rather than a state it cannot act on.
        if !mosh_install::enabled() {
            return;
        }
        // The ledger is resolved **before** the claim. With no home directory
        // there is nowhere to remember an attempt, and an attempt that cannot
        // be remembered is one that would run again on every reconnect — which
        // is the whole failure this is written to avoid. Claiming first would
        // also have settled the claim through `Drop`, reporting "the install
        // task ended without reporting" for a condition that has nothing to do
        // with a task.
        let Ok(ledger) = mosh_install::Ledger::standard() else { return };
        let Some(claim) = mosh_install::claim(&self.host, job) else { return };

        let host = self.host.clone();
        tokio::spawn(async move {
            // A *fresh* target rather than a borrow of this one. `ControlMaster`
            // is shared per destination through its registry, so this rides the
            // connection that is already open and costs no handshake — while
            // owning nothing that has to outlive the task.
            let target = SshTarget::new(host.clone());
            let host_handle = InstallTarget { target: &target };
            mosh_install::run_claimed(&host, &host_handle, &ledger, job, claim).await;
        });
    }

    /// What is already known about mosh for this host, without probing.
    pub fn mosh_availability(&self) -> Option<mosh::Availability> {
        mosh::cached(&self.host)
    }

    /// What the last low-latency request on this target resolved to, if one was
    /// made. Never probes; a `FellBackToSsh` carries the reason to show.
    pub fn low_latency_status(&self) -> Option<LowLatency> {
        *self.low_latency.read()
    }

    async fn connect_inner(&self) -> anyhow::Result<Platform> {
        self.master.ensure_started().await?;

        if let Some(platform) = *self.platform.read() {
            return Ok(platform);
        }

        // `uname -s` over the freshly established master. Anything we cannot
        // identify is `Other`, which only costs us the /proc metrics fast path.
        //
        // **Retried when the transport itself failed**, because a `ProxyCommand`
        // host can refuse the first connection and accept the next — see
        // [`is_worth_retrying`] for exactly which failures qualify and why an
        // authentication failure emphatically does not. The probe is a read-only
        // `uname`, so re-running it cannot do anything twice; that is why the
        // retry lives here rather than around `exec` in general, where a caller
        // may be writing a file.
        let mut probe = self.exec(&CommandSpec::new("uname").arg("-s").tty(Tty::None)).await?;

        for attempt in 2..=CONNECT_ATTEMPTS {
            if !is_transport_failure(probe.status) || !is_worth_retrying(&probe.stderr) {
                break;
            }
            // Logged, not swallowed. A retry that hides the reason turns "the
            // proxy is flaky" into "it sometimes takes a moment", and the next
            // person to look has nothing to go on — which is precisely the state
            // this arrived in, with a log that recorded the app starting and
            // nothing about the connection that failed.
            tracing::warn!(
                host = %self.host.alias,
                attempt,
                reason = %mux::summarise_stderr(&probe.stderr),
                "connection failed in the transport; retrying"
            );
            tokio::time::sleep(RETRY_AFTER).await;
            probe = self.exec(&CommandSpec::new("uname").arg("-s").tty(Tty::None)).await?;
        }

        // **A failed `uname` is the Windows signal, not an error.** OpenSSH for
        // Windows hands the command to `cmd.exe` unless `DefaultShell` says
        // otherwise, and `cmd` has no `uname` — so the very first thing rmux
        // does fails, and before this the connection simply never completed.
        // Measured on a real Windows 11 host.
        if probe.status != 0 {
            // **255 is ssh's own failure, not the remote command's.** OpenSSH
            // uses it for every connection-level problem it has — a dropped
            // ControlMaster, an auth failure, `MaxSessions` reached, a network
            // blip — and it never reaches a remote shell to find out what that
            // shell is.
            //
            // Treating it as the Windows signal told the operator that a
            // perfectly ordinary Linux host "is not POSIX and no bash was
            // found", and advised them to install Git for Windows on it. That
            // is a confident answer to a question rmux never got to ask, and it
            // sends the reader somewhere with no relation to what went wrong.
            //
            // `cmd.exe` failing to find `uname` is a *remote* exit code, so the
            // real Windows signal is any non-zero status other than this one.
            if is_transport_failure(probe.status) {
                // The operator gets this on screen; the log gets it too, because
                // "it says it cannot connect" is all anyone can report from
                // another machine otherwise.
                tracing::warn!(
                    host = %self.host.alias,
                    reason = %mux::summarise_stderr(&probe.stderr),
                    "could not reach the host"
                );
                anyhow::bail!("could not reach {}{}", self.host.alias, explain(&probe.stderr));
            }

            if let Some(bash) = self.find_posix_shell().await {
                tracing::info!(bash = %bash, "windows host: routing commands through a POSIX shell");
                winshell::remember(&self.host.alias, RemoteShell::Via { bash });
                *self.platform.write() = Some(Platform::Windows);
                return Ok(Platform::Windows);
            }
            anyhow::bail!(
                "this host's shell is not POSIX and no bash was found. rmux drives hosts with \
                 POSIX shell scripts; on Windows install Git for Windows, or set OpenSSH's \
                 DefaultShell to a POSIX shell."
            );
        }

        let platform = match probe.stdout_or_err()? {
            s if s.eq_ignore_ascii_case("linux") => Platform::Linux,
            s if s.eq_ignore_ascii_case("darwin") => Platform::MacOs,
            // A host already answering `uname` from MSYS/Cygwin is POSIX enough
            // to drive directly — no wrapper needed.
            s if s.starts_with("MINGW") || s.starts_with("CYGWIN") => Platform::Windows,
            other => {
                tracing::debug!(uname = other, "unrecognised remote platform");
                Platform::Other
            }
        };

        *self.platform.write() = Some(platform);
        Ok(platform)
    }

    /// Which POSIX shell this Windows host has, if any.
    ///
    /// Runs as a raw `cmd` line — deliberately *not* through `build_command`,
    /// which would try to wrap it in the very shell being looked for.
    async fn find_posix_shell(&self) -> Option<String> {
        // Accounted like any other one-shot, and over the master the slot was
        // taken on. It spawns `ssh` itself rather than going through `exec`, so
        // without this it would be the one command riding the connection that
        // the channel count could not see.
        let slot = self.pool.acquire(ChannelKind::OneShot).await.ok()?;
        let mut args = self.ssh_argv(slot.master(), Tty::None);
        args.push("--".to_owned());
        args.push(winshell::probe_script());

        let out = tokio::process::Command::new("ssh")
            .no_console_window()
            .args(&args)
            .envs(askpass::env_for_gui_prompts())
            .output()
            .await
            .ok()?;

        winshell::parse_probe(&String::from_utf8_lossy(&out.stdout))
    }

    /// Everything that identifies this *connection* to `ssh`: multiplexing
    /// options, the installed identity, and the user/port overrides.
    ///
    /// Shared by the direct ssh path and by mosh's `--ssh=` bootstrap, which is
    /// the point — mosh's bootstrap **is** an ssh, so a second definition would
    /// be a second chance for ControlMaster, an installed key or a port
    /// override to be lost on one path and not the other.
    ///
    /// **The master is passed in rather than read off `self`.** A channel is
    /// reserved on one specific member of the host's pool, and the
    /// `ControlPath` has to be that member's — write the primary's here and the
    /// command rides a connection nobody accounted for, which is the
    /// over-subscription this pool exists to stop.
    fn connection_options(&self, master: &ControlMaster) -> Vec<String> {
        let mut args = master.client_options();

        // Also here, not only on the master: Windows has no ControlMaster, so
        // every command there is its own connection and authenticates on its
        // own. Harmless where multiplexing works — the socket answers before an
        // identity is ever consulted.
        args.extend(keys::identity_args_for(&self.host.label()));

        if let Some(user) = &self.host.user {
            args.push("-l".to_owned());
            args.push(user.clone());
        }
        if let Some(port) = self.host.port {
            // **ssh's port.** mosh's own `-p` is the UDP range its server
            // binds; the two are not interchangeable.
            args.push("-p".to_owned());
            args.push(port.to_string());
        }

        args
    }

    /// Base `ssh` argv shared by every invocation: connection options, the TTY
    /// request, then the destination.
    fn ssh_argv(&self, master: &ControlMaster, tty: Tty) -> Vec<String> {
        let mut args = self.connection_options(master);

        match tty {
            // `-t` forces TTY allocation even though our stdin is a pipe rather
            // than a terminal; without the second `-t` ssh refuses.
            Tty::Allocate => args.push("-tt".to_owned()),
            Tty::None => args.push("-T".to_owned()),
        }

        args.push(self.host.alias.clone());
        args
    }

    /// The POSIX shell line for `spec`, wrapped for a non-POSIX host if needed.
    ///
    /// **One definition, used by both transports.** The Windows wrapper is the
    /// single place a non-POSIX host differs, and it must not fork into a
    /// second copy just because a second transport arrived — a POSIX *client*
    /// may target a Windows host, and the line has to be built identically
    /// either way.
    fn remote_line(&self, spec: &CommandSpec) -> String {
        let line = spec_to_shell_line(spec);
        match winshell::shell_for(&self.host.alias) {
            RemoteShell::Posix => line,
            RemoteShell::Via { bash } => winshell::wrap(&bash, &line),
        }
    }

    /// The mosh form of `spec`, or `None` when mosh is not to be used.
    ///
    /// Three conditions, and the first is load-bearing:
    ///
    /// 1. **`Tty::Allocate`.** `exec` and `exec_with_input` build their commands
    ///    with `Tty::None` and then *parse stdout*. mosh is a terminal
    ///    emulator: it emits a rendered screen, not the command's bytes. Routing
    ///    a one-shot through it would silently corrupt every file listing,
    ///    search result and metrics sample.
    /// 2. The caller asked for it.
    /// 3. The host was found to support it — read from the cache only, because
    ///    `build_command` is documented never to perform I/O.
    fn mosh_command(&self, spec: &CommandSpec, master: &ControlMaster) -> Option<ResolvedCommand> {
        if spec.tty != Tty::Allocate || !spec.low_latency {
            return None;
        }

        match self.mosh_availability() {
            Some(mosh::Availability::Available) => {}
            other => {
                // Not probed yet is not a refusal to report — there is nothing
                // to tell the operator except that connect has not run.
                if let Some(why) = other.and_then(mosh::Availability::refusal) {
                    *self.low_latency.write() = Some(LowLatency::FellBackToSsh(why));
                    mosh::log_refusal_once(&self.host, why);
                }
                return None;
            }
        }

        // A UTF-8 locale is a *remote environment* setting, so it rides the
        // shell line rather than argv: without one mosh-server refuses to start
        // and a session that does start garbles everything a TUI draws. A
        // caller that set its own locale keeps it.
        let mut spec = spec.clone();
        for key in ["LANG", "LC_ALL"] {
            spec.env.entry(key.to_owned()).or_insert_with(|| mosh::UTF8_LOCALE.to_owned());
        }

        let line = self.remote_line(&spec);
        let args = mosh::argv(&mosh::Bootstrap {
            ssh_options: &self.connection_options(master),
            alias: &self.host.alias,
            remote_line: &line,
        });

        *self.low_latency.write() = Some(LowLatency::Mosh);

        Some(ResolvedCommand {
            program: "mosh".into(),
            args: args.into_iter().map(Into::into).collect(),
            // mosh's bootstrap **is** an ssh, so a password or 2FA host still
            // needs the askpass bridge — without it the prompt has nowhere to
            // go and the session hangs with nothing on screen.
            //
            // **`TERM` and the locale are the *local* wrapper's, and rmux has
            // neither.** `/opt/homebrew/bin/mosh` shells out to `tput colors`
            // and dies on the spot when `TERM` is unset — measured, verbatim:
            // `TERM environment variable not set. Died at .../mosh line 320.`
            // A Finder-launched `.app` inherits no `TERM`, because it has no
            // controlling terminal; `ssh` never cared, since the pty is *ours*
            // and the remote `TERM` rides the shell line. mosh is the first
            // thing rmux spawns that reads its own environment, so it is the
            // first to notice. The locale pair is here for the same reason —
            // the wrapper refuses without a UTF-8 native locale, and the copy
            // already on the shell line only ever reaches the far side.
            //
            // It goes in `env`, never argv: `spec_to_shell_line` renders
            // `CommandSpec::env` *into* the command line, and this is a local
            // process's environment rather than the session's.
            env: mosh::local_env(askpass::env_for_gui_prompts()),
        })
    }

    /// `build_command`, against a named master.
    fn build_command_on(
        &self,
        spec: &CommandSpec,
        master: &ControlMaster,
    ) -> anyhow::Result<ResolvedCommand> {
        // The transport branch, and the only one. A caller asked for low
        // latency — a property of the session, phrased in what the operator
        // feels — and *this* is where a target decides what to do about it.
        // Everything below is unchanged and is still what runs whenever the
        // answer is no.
        if let Some(resolved) = self.mosh_command(spec, master) {
            return Ok(resolved);
        }

        let mut args = self.ssh_argv(master, spec.tty);

        // `--` stops ssh from interpreting the remote command as its own options.
        args.push("--".to_owned());

        // The one place a non-POSIX host differs. Everything above this line —
        // and every caller — still builds an ordinary POSIX shell line, which is
        // the invariant: there is no `if windows` in feature code, only here.
        args.push(self.remote_line(spec));

        Ok(ResolvedCommand {
            program: "ssh".into(),
            args: args.into_iter().map(Into::into).collect(),
            // The *local* ssh process's environment, not the remote command's —
            // the latter is baked into the shell line above. This is where the
            // askpass helper is wired in so credential prompts become native UI.
            env: askpass::env_for_gui_prompts(),
        })
    }
}

/// The host half of [`mosh::Probe`], run over the connection that is already
/// open. Separate from [`SshTarget`] so the probe's surface does not become
/// part of the target's.
struct MoshProbe<'a> {
    target: &'a SshTarget,
}

#[async_trait]
impl mosh::Probe for MoshProbe<'_> {
    async fn remote_server(&self) -> mosh::ServerProbe {
        // Through the ordinary ssh path with no TTY: multiplexed over the
        // ControlMaster already up, so it costs no handshake. The *status* is
        // presence; the output carries the version, and both come back from this
        // one round trip because asking twice would double the cost of the probe
        // on exactly the link this feature exists for.
        let spec = CommandSpec::new("sh").arg("-c").arg(mosh::server_probe_line()).tty(Tty::None);
        match self.target.exec(&spec).await {
            Ok(out) if out.ok() => {
                // Both streams: which one `mosh-server --version` prints to
                // varies by build, and reading only stdout would silently see no
                // version on some hosts.
                let version = mosh::parse_server_version(&out.stdout)
                    .or_else(|| mosh::parse_server_version(&out.stderr));
                mosh::ServerProbe::Present(version)
            }
            _ => mosh::ServerProbe::Missing,
        }
    }
}

/// The host half of [`mosh_install::InstallHost`].
///
/// Deliberately a thin wrapper rather than an impl on [`SshTarget`]: the
/// install's surface — run a line, is this POSIX — is not part of what a target
/// is, and keeping it separate is what lets every branch of the install be
/// tested against a fake with no host anywhere near it.
struct InstallTarget<'a> {
    target: &'a SshTarget,
}

#[async_trait]
impl mosh_install::InstallHost for InstallTarget<'_> {
    async fn run(&self, line: &str) -> anyhow::Result<Output> {
        // `Tty::None`, over the ControlMaster already up. A package manager
        // given a TTY is a package manager that may decide to ask a question.
        self.target.exec(&CommandSpec::new("sh").arg("-c").arg(line).tty(Tty::None)).await
    }

    /// `winshell` is the authority, exactly as it is for every other command.
    fn is_posix(&self) -> bool {
        matches!(winshell::shell_for(&self.target.host.alias), RemoteShell::Posix)
    }
}

#[async_trait]
impl Target for SshTarget {
    fn id(&self) -> &TargetId {
        &self.id
    }

    fn build_command(&self, spec: &CommandSpec) -> anyhow::Result<ResolvedCommand> {
        // On the **primary** master, and with no channel reserved: this is the
        // synchronous surface, which can neither wait for a slot nor open a
        // member. Everything that spawns something long-lived goes through
        // `open_channel` instead; what is left here is the callers that only
        // want to see an argv (tests, `claude_login`'s one-off pty).
        self.build_command_on(spec, &self.master)
    }

    async fn open_channel(
        &self,
        spec: &CommandSpec,
    ) -> anyhow::Result<(ResolvedCommand, ChannelLease)> {
        // The kind is read off the pty request, which is the honest signal:
        // everything rmux holds open wants one, and everything that parses
        // stdout must not have one. See `mux::ChannelKind`.
        let slot = self.pool.acquire(ChannelKind::from(spec.tty)).await?;
        let resolved = self.build_command_on(spec, slot.master())?;
        Ok((resolved, slot.into_lease()))
    }

    /// Run a one-shot over a **reserved** channel.
    ///
    /// The lease lives exactly as long as the process: taken before the spawn,
    /// dropped when the output has been read. That is what makes polls — file
    /// listings, metrics, transcript tails, which are what fill a connection —
    /// visible to the accounting at all, and what lets one of them report "the
    /// connection is full" instead of waiting for ever.
    async fn exec(&self, spec: &CommandSpec) -> anyhow::Result<Output> {
        let (resolved, _lease) = self.open_channel(&spec.clone().tty(Tty::None)).await?;

        let mut cmd = tokio::process::Command::new(&resolved.program);
        cmd.no_console_window();
        cmd.args(&resolved.args);
        for (k, v) in &resolved.env {
            cmd.env(k, v);
        }

        let out = cmd.output().await?;
        Ok(Output {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    async fn exec_with_input(
        &self,
        spec: &CommandSpec,
        input: &[u8],
    ) -> anyhow::Result<Output> {
        let (resolved, _lease) = self.open_channel(&spec.clone().tty(Tty::None)).await?;
        rmux_transport::run_feeding(&resolved, None, rmux_transport::Stdin::Bytes(input)).await
    }

    /// Streams from disk, holding the channel lease for the whole transfer —
    /// a multi-gigabyte upload is the longest-lived one-shot there is, and the
    /// accounting must see it for exactly that long.
    async fn exec_with_input_file(
        &self,
        spec: &CommandSpec,
        input: &std::path::Path,
        sent: &std::sync::atomic::AtomicU64,
    ) -> anyhow::Result<Output> {
        let (resolved, _lease) = self.open_channel(&spec.clone().tty(Tty::None)).await?;
        rmux_transport::run_feeding(&resolved, None, rmux_transport::Stdin::File { path: input, sent }).await
    }

    fn platform(&self) -> Option<Platform> {
        *self.platform.read()
    }

    async fn ensure_ready(&self) -> anyhow::Result<()> {
        // Keep the shared control master warm. On Windows/unsupported this is a
        // no-op (`ensure_alive` returns `Unsupported`), so the russh path is
        // untouched.
        self.master.ensure_alive().await?;
        Ok(())
    }

    /// Close the shared control master.
    ///
    /// `ssh -O exit` rather than dropping this target and hoping: the master
    /// outlives any one `Arc`, and a disconnect that quietly leaves the
    /// connection up is indistinguishable from one that never ran. On Windows
    /// there is no master and `stop` is a no-op, which is correct — every
    /// command there is its own connection and there is nothing to tear down.
    async fn disconnect(&self) {
        // **The whole pool**, not just the primary. A host that filled one
        // connection has more than one, and stopping the first while another
        // still carries channels is a disconnect that half-worked — which, as
        // the `server_disconnect` note says, is indistinguishable from one that
        // did nothing.
        self.pool.stop_all().await;
    }

    /// The transport-neutral reading of [`SshTarget::low_latency_status`], for
    /// the callers that hold an `Arc<dyn Target>` and cannot name mosh.
    fn low_latency_outcome(&self) -> Option<LowLatencyOutcome> {
        Some(match self.low_latency_status()? {
            LowLatency::Mosh => LowLatencyOutcome::Granted,
            LowLatency::FellBackToSsh(why) => {
                LowLatencyOutcome::Declined { reason: why.reason().to_owned() }
            }
        })
    }

    /// Settle mosh for this host and say what was found.
    ///
    /// **This is the call that has to happen before `build_command`.** The
    /// probe was deliberately taken out of [`SshTarget::connect`] — it cost a
    /// round trip per host per run on exactly the link this feature exists to
    /// fix — so the cache is empty until someone asks. `build_command` is sync
    /// and reads the cache only, and an unprobed host is *not a refusal*: it
    /// falls back to ssh with nothing to report. So a spawn path that means to
    /// set `low_latency` awaits this first, and the UI awaits it to decide
    /// whether to show a control at all.
    ///
    /// Cached per host, so the second and every later call is free.
    async fn probe_low_latency(&self) -> LowLatencyOutcome {
        match self.probe_mosh().await {
            mosh::Availability::Available => LowLatencyOutcome::Granted,
            mosh::Availability::Refused(why) => {
                LowLatencyOutcome::Declined { reason: why.reason().to_owned() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SshTarget {
        SshTarget::new(SshHostId::new("devbox"))
    }


    /// The failure this retry exists for, in OpenSSH's own words.
    #[test]
    fn a_dead_proxy_command_is_retried() {
        // `UNKNOWN port 65535` means there was never a peer socket, so the
        // failure happened in the ProxyCommand rather than on the far host.
        assert!(is_worth_retrying("Connection closed by UNKNOWN port 65535"));
        assert!(is_worth_retrying(
            "/bin/sh: line 0: exec: cloudflared: not found\nConnection closed by UNKNOWN port 65535"
        ));
        // The wording around the port has changed between OpenSSH releases; the
        // port has not.
        assert!(is_worth_retrying("kex_exchange_identification: Connection closed by remote host"));
        assert!(is_worth_retrying("Connection timed out during banner exchange"));
    }

    /// **The half that matters most.**
    ///
    /// `ssh` already makes two or three password attempts inside one connection.
    /// Retrying multiplies them, and measured against a real host today, repeated
    /// failed logins from rmux got the client banned mid-session — which reads
    /// afterwards as "the server is down", a worse and much more confusing
    /// failure than the one a retry would have papered over.
    #[test]
    fn a_rejected_credential_is_never_retried() {
        assert!(!is_worth_retrying("root@host: Permission denied (publickey,password)."));
        assert!(!is_worth_retrying("Too many authentication failures"));
        assert!(!is_worth_retrying("No supported authentication methods available"));

        // A refusal is not a blip. Retrying a changed host key would hammer a
        // host at exactly the moment the honest answer is "something is wrong".
        assert!(!is_worth_retrying("Host key verification failed."));
        assert!(!is_worth_retrying(
            "@@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@@"
        ));

        // sshd's MaxSessions counter being full is a real limit, not a race;
        // `explain` already tells the operator how to raise it.
        assert!(!is_worth_retrying(
            "channel 3: open failed: administratively prohibited: open failed"
        ));
    }

    #[test]
    fn the_veto_beats_the_allow_list() {
        // A credential failure that also mentions a closed connection must not
        // slip through because one word matched. Order of evaluation is the
        // whole guarantee here.
        assert!(!is_worth_retrying(
            "Connection closed by 10.0.0.1 port 22\nPermission denied (publickey)."
        ));
    }

    #[test]
    fn an_unrecognised_failure_is_left_alone() {
        // Silence is not evidence of a transient fault, and retrying everything
        // turns one clear error into several seconds of nothing.
        assert!(!is_worth_retrying(""));
        assert!(!is_worth_retrying("ssh: Could not resolve hostname devbox"));
    }

    #[test]
    fn the_retry_budget_stays_small() {
        // Three attempts, ~600ms apart: under a second and a half of extra wait
        // before a genuine failure reaches the operator.
        assert_eq!(CONNECT_ATTEMPTS, 3);
        assert!(RETRY_AFTER.as_millis() * (CONNECT_ATTEMPTS as u128 - 1) < 2_000);
    }

    #[test]
    fn a_full_channel_counter_is_not_reported_as_a_permissions_problem() {
        let msg = explain("channel 3: open failed: administratively prohibited: open failed");
        assert!(msg.contains("MaxSessions"), "must name the actual limit: {msg}");
        // The original text is kept — it is what the operator will search for.
        assert!(msg.contains("administratively prohibited"));

        // Anything else is passed through untouched rather than guessed at.
        let other = explain("Permission denied (publickey).");
        assert!(!other.contains("MaxSessions"), "{other}");
        assert!(other.contains("Permission denied"));

        assert!(explain("   ").contains("without saying why"));
    }

    /// The bug: **a Linux host that ssh could not reach was reported as
    /// "not POSIX", with advice to install Git for Windows on it.**
    ///
    /// `connect` reads a non-zero `uname -s` as the Windows signal, which is
    /// right when the non-zero came from `cmd.exe`. It is wrong when it came
    /// from ssh itself — 255 means ssh never reached a remote shell at all, so
    /// there is nothing to conclude about what that shell is.
    ///
    /// This pins the classification rather than the message, because the
    /// classification is the part that sent the operator to the wrong machine.
    #[test]
    fn sshs_own_failure_is_not_a_verdict_on_the_remote_shell() {
        assert_eq!(SSH_FAILURE, 255, "OpenSSH reserves 255 for its own failures");

        // A dropped connection, an auth failure, MaxSessions — all 255, and
        // none of them say anything about the far side.
        assert!(is_transport_failure(255));

        // `cmd.exe` not finding `uname` is a *remote* exit code. Those are the
        // ones that may legitimately mean Windows.
        assert!(!is_transport_failure(1), "cmd.exe's 'not recognized'");
        assert!(!is_transport_failure(9009), "cmd.exe's command-not-found");
        assert!(!is_transport_failure(127), "a POSIX shell's command-not-found");
    }

    #[test]
    fn terminal_commands_request_a_tty_and_end_with_a_separator() {
        let t = target();
        let resolved = t.build_command(&CommandSpec::login_shell()).unwrap();

        assert_eq!(resolved.program, "ssh");
        let args: Vec<String> =
            resolved.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();

        assert!(args.contains(&"-tt".to_owned()), "interactive commands need a TTY: {args:?}");
        assert!(args.contains(&"devbox".to_owned()));

        // The remote command must be the final argument, guarded by `--`.
        let sep = args.iter().position(|a| a == "--").expect("missing -- separator");
        assert_eq!(sep, args.len() - 2);
        // `-i` as well as `-l`: the login shell has to read `.zshrc`, which is
        // where version managers put their PATH. See `CommandSpec::login_shell`.
        assert_eq!(args[args.len() - 1], r#""$SHELL" -l -i"#);
    }

    #[test]
    fn non_interactive_commands_disable_the_tty() {
        let t = target();
        let resolved = t.build_command(&CommandSpec::new("uname").arg("-s").tty(Tty::None)).unwrap();
        let args: Vec<String> =
            resolved.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();

        assert!(args.contains(&"-T".to_owned()));
        assert!(!args.contains(&"-tt".to_owned()));
        assert_eq!(args[args.len() - 1], "uname -s");
    }

    #[test]
    fn user_and_port_overrides_are_passed_as_flags() {
        // Passed as flags rather than folded into a `user@host` string so that a
        // ~/.ssh/config Host alias still resolves.
        let t = SshTarget::new(SshHostId {
            alias: "devbox".to_owned(),
            user: Some("deploy".to_owned()),
            port: Some(2222),
        });
        let resolved = t.build_command(&CommandSpec::login_shell()).unwrap();
        let args: Vec<String> =
            resolved.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();

        let l = args.iter().position(|a| a == "-l").unwrap();
        assert_eq!(args[l + 1], "deploy");
        let p = args.iter().position(|a| a == "-p").unwrap();
        assert_eq!(args[p + 1], "2222");
        assert_eq!(args[args.len() - 3], "devbox");
    }

    #[test]
    fn remote_paths_with_spaces_are_quoted_not_split() {
        let t = target();
        let spec = CommandSpec::login_shell().cwd("/srv/my project");
        let resolved = t.build_command(&spec).unwrap();
        let last = resolved.args.last().unwrap().to_string_lossy().into_owned();

        assert_eq!(last, r#"cd '/srv/my project' && "$SHELL" -l -i"#);
    }

    // ---- mosh: the low-latency transport ----------------------------------

    /// Answers whatever it was built with, without touching a machine.
    struct FakeProbe(bool);

    #[async_trait]
    impl mosh::Probe for FakeProbe {
        fn local_client(&self) -> bool {
            self.0
        }
        async fn remote_server(&self) -> mosh::ServerProbe {
            if self.0 {
                // Current, so nothing in these tests starts an upgrade attempt.
                mosh::ServerProbe::Present(Some(mosh::MIN_SERVER_VERSION))
            } else {
                mosh::ServerProbe::Missing
            }
        }
    }

    /// A target whose host is already known to support (or refuse) mosh.
    async fn target_with_mosh(alias: &str, available: bool) -> SshTarget {
        let host = SshHostId::new(alias);
        mosh::availability_for(&host, &FakeProbe(available)).await;
        SshTarget::new(host)
    }

    fn argv_of(resolved: &ResolvedCommand) -> Vec<String> {
        resolved.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    /// **The rule everything else rests on.**
    ///
    /// `exec` and `exec_with_input` build with `Tty::None` and then parse
    /// stdout. mosh is a terminal emulator — it emits a rendered screen, not the
    /// command's bytes — so routing a one-shot through it would silently corrupt
    /// every file listing, search result and metrics sample. Asking for low
    /// latency must not be able to move that line.
    #[tokio::test]
    async fn a_one_shot_is_always_ssh_even_when_mosh_is_available_and_asked_for() {
        let t = target_with_mosh("mosh-oneshot", true).await;
        assert_eq!(t.mosh_availability(), Some(mosh::Availability::Available));

        let spec = CommandSpec::new("uname").arg("-s").tty(Tty::None).low_latency(true);
        let resolved = t.build_command(&spec).unwrap();

        assert_eq!(resolved.program, "ssh");
        let args = argv_of(&resolved);
        assert!(args.contains(&"-T".to_owned()), "{args:?}");
        assert_eq!(args.last().unwrap(), "uname -s");
    }

    #[tokio::test]
    async fn an_interactive_session_that_asked_for_low_latency_gets_mosh() {
        let t = target_with_mosh("mosh-yes", true).await;
        let resolved = t.build_command(&CommandSpec::login_shell().low_latency(true)).unwrap();

        assert_eq!(resolved.program, "mosh");
        let args = argv_of(&resolved);

        // The destination is the alias, verbatim, after mosh's own `--`.
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[sep + 1], "mosh-yes");
        // The remote command is `sh -c <line>`, one argument, and the line is
        // the same POSIX line the ssh path would have sent — plus a UTF-8
        // locale, which mosh needs and which is a *remote environment* setting.
        assert_eq!(args[args.len() - 3], "sh");
        assert_eq!(args[args.len() - 2], "-c");
        let line = args.last().unwrap();
        assert!(line.contains(r#""$SHELL" -l -i"#), "{line}");
        assert!(line.contains("LANG=en_US.UTF-8"), "{line}");
        assert!(line.contains("LC_ALL=en_US.UTF-8"), "{line}");

        // The bootstrap is an ssh, so the askpass bridge must still be wired in
        // or a password host hangs with no prompt anywhere.
        for (key, value) in askpass::env_for_gui_prompts() {
            assert_eq!(resolved.env.get(&key), Some(&value), "askpass key {key} was dropped");
        }

        // And the local wrapper's own environment rides beside it. `mosh` runs
        // `tput colors` before it reaches ssh at all, so an unset `TERM` kills
        // the session on this side with `Died at .../mosh line 320.` — measured
        // from a live pane, which is why this assertion exists.
        assert!(!resolved.env["TERM"].is_empty());
        assert!(resolved.env["LC_ALL"].contains("UTF-8"));

        assert_eq!(t.low_latency_status(), Some(LowLatency::Mosh));
    }

    /// The default path must be untouched — byte for byte.
    #[tokio::test]
    async fn without_the_request_the_argv_is_exactly_what_it_was() {
        let t = target_with_mosh("mosh-unasked", true).await;
        let spec = CommandSpec::login_shell().cwd("/srv/app");

        let before = t.build_command(&spec).unwrap();
        let asked_not_to = t.build_command(&spec.clone().low_latency(false)).unwrap();

        assert_eq!(before, asked_not_to);
        assert_eq!(before.program, "ssh");
        assert_eq!(t.low_latency_status(), None, "nothing was requested, so there is nothing to report");
    }

    /// A session that quietly is not what was asked for is the silently-wrong
    /// kind of wrong, so the refusal has to be retrievable — and carry a reason.
    #[tokio::test]
    async fn a_refused_request_falls_back_to_ssh_and_says_why() {
        let t = target_with_mosh("mosh-no", false).await;
        let resolved = t.build_command(&CommandSpec::login_shell().low_latency(true)).unwrap();

        assert_eq!(resolved.program, "ssh");
        let why = match t.low_latency_status() {
            Some(LowLatency::FellBackToSsh(why)) => why,
            other => panic!("the fallback was not reported: {other:?}"),
        };
        assert!(!why.reason().is_empty());
        // The availability itself is readable without probing anything again.
        assert_eq!(t.mosh_availability(), Some(mosh::Availability::Refused(why)));

        // And readable through the trait, which is what a caller holding an
        // `Arc<dyn Target>` actually has — a fallback nobody can read is a
        // fallback nobody reports.
        let via_trait: &dyn Target = &t;
        match via_trait.low_latency_outcome() {
            Some(LowLatencyOutcome::Declined { reason }) => assert!(!reason.is_empty()),
            other => panic!("the trait did not report the decline: {other:?}"),
        }
    }

    /// `build_command` is documented never to perform I/O, so an unprobed host
    /// cannot be asked — and an unknown answer is not a refusal to report.
    #[tokio::test]
    async fn an_unprobed_host_falls_back_without_claiming_a_reason() {
        let t = SshTarget::new(SshHostId::new("mosh-unprobed"));
        assert_eq!(t.mosh_availability(), None);

        let resolved = t.build_command(&CommandSpec::login_shell().low_latency(true)).unwrap();
        assert_eq!(resolved.program, "ssh");
        assert_eq!(t.low_latency_status(), None);
    }

    /// The ssh port belongs to the bootstrap. mosh's own `--port` is the UDP
    /// range its server binds, and confusing them breaks both halves at once.
    #[tokio::test]
    async fn the_port_and_user_overrides_ride_the_bootstrap_not_mosh() {
        let host = SshHostId {
            alias: "mosh-port".to_owned(),
            user: Some("deploy".to_owned()),
            port: Some(2222),
        };
        mosh::availability_for(&host, &FakeProbe(true)).await;
        let t = SshTarget::new(host);

        let resolved = t.build_command(&CommandSpec::login_shell().low_latency(true)).unwrap();
        let args = argv_of(&resolved);
        let ssh = args.iter().find(|a| a.starts_with("--ssh=")).expect("no bootstrap");

        assert!(ssh.contains("-p 2222"), "{ssh}");
        assert!(ssh.contains("-l deploy"), "{ssh}");
        assert!(!args.iter().any(|a| a == "-p" || a.starts_with("--port")), "{args:?}");
        // And the destination is still the alias — never a resolved HostName.
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[sep + 1], "mosh-port");
    }

    /// mosh re-quotes what it hands to `ssh`, and the remote login shell
    /// re-parses the result. A fragment that escapes here is a command someone
    /// else's machine runs.
    #[tokio::test]
    async fn a_hostile_path_cannot_escape_the_mosh_command() {
        let t = target_with_mosh("mosh-hostile", true).await;
        let spec = CommandSpec::login_shell()
            .cwd("/tmp/'; touch /tmp/pwned; `id`; $(whoami) && rm -rf /")
            .low_latency(true);

        let resolved = t.build_command(&spec).unwrap();
        let args = argv_of(&resolved);

        // Everything hostile is inside the single final argument, and it is
        // inert: the quote was closed, escaped and reopened.
        let line = args.last().unwrap();
        assert!(line.contains(r"'\''"), "the quote was not escaped: {line}");
        assert_eq!(
            args.iter().filter(|a| a.contains("pwned")).count(),
            1,
            "the payload appears in more than one argv position: {args:?}"
        );
        // Nothing reached the bootstrap, which mosh splits with shellwords.
        let ssh = args.iter().find(|a| a.starts_with("--ssh=")).unwrap();
        for fragment in ["pwned", "whoami", "rm -rf"] {
            assert!(!ssh.contains(fragment), "{fragment:?} reached the bootstrap: {ssh}");
        }
    }

    #[test]
    fn a_hostile_cwd_cannot_break_out_of_its_quotes() {
        let t = target();
        let spec = CommandSpec::new("ls").cwd("/tmp/'; touch /tmp/pwned; '");
        let resolved = t.build_command(&spec).unwrap();
        let last = resolved.args.last().unwrap().to_string_lossy().into_owned();

        // The payload survives as inert text inside the quoted argument.
        assert!(last.starts_with("cd '/tmp/'\\''; touch /tmp/pwned; '\\'''"));
    }
}
