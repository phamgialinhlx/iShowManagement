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
//! free — if it works in the user's terminal, it works in zmux.
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
use zmux_transport::{
    CommandSpec, NoConsoleWindow, Output, Platform, ResolvedCommand, SshHostId, Target, TargetId,
    Tty, spec_to_shell_line,
};

pub mod askpass;
pub mod forward;
pub mod config;
pub mod keys;
pub mod mux;
pub mod winshell;

pub use config::{ConfigHost, list_hosts};
pub use mux::{ControlMaster, MasterState};
pub use winshell::RemoteShell;

/// An SSH host as a [`Target`].
#[derive(Debug)]
pub struct SshTarget {
    id: TargetId,
    host: SshHostId,
    master: ControlMaster,
    platform: parking_lot::RwLock<Option<Platform>>,
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
/// zmux opens several at once by necessity, because there is no `ControlMaster`
/// to share, which is why this shows up there and not on macOS.
///
/// ## What must never be retried, and why it matters more than the retry
///
/// **Authentication failures.** `ssh` already makes two or three password
/// attempts of its own inside a single connection, so retrying multiplies them.
/// Measured today against a real host: repeated failed logins from zmux got this
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
/// One case is worth singling out. zmux rides every channel over a single
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
             permissions problem — zmux multiplexes every channel over one connection, so \
             several sessions on one host reach it. Raise MaxSessions in the host's \
             /etc/ssh/sshd_config, or use fewer sessions against it."
        );
    }
    format!(": {reason}")
}

impl SshTarget {
    pub fn new(host: SshHostId) -> Self {
        let master = ControlMaster::new(host.clone());
        Self {
            id: TargetId::Ssh(host.clone()),
            host,
            master,
            platform: parking_lot::RwLock::new(None),
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
    pub async fn connect(&self) -> anyhow::Result<Platform> {
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
        // otherwise, and `cmd` has no `uname` — so the very first thing zmux
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
            // is a confident answer to a question zmux never got to ask, and it
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
                "this host's shell is not POSIX and no bash was found. zmux drives hosts with \
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
        let mut args = self.ssh_argv(Tty::None);
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

    /// Base `ssh` argv shared by every invocation: multiplexing options, the
    /// user/port overrides, then the destination.
    fn ssh_argv(&self, tty: Tty) -> Vec<String> {
        let mut args = self.master.client_options();

        match tty {
            // `-t` forces TTY allocation even though our stdin is a pipe rather
            // than a terminal; without the second `-t` ssh refuses.
            Tty::Allocate => args.push("-tt".to_owned()),
            Tty::None => args.push("-T".to_owned()),
        }

        if let Some(user) = &self.host.user {
            args.push("-l".to_owned());
            args.push(user.clone());
        }
        if let Some(port) = self.host.port {
            args.push("-p".to_owned());
            args.push(port.to_string());
        }

        args.push(self.host.alias.clone());
        args
    }
}

#[async_trait]
impl Target for SshTarget {
    fn id(&self) -> &TargetId {
        &self.id
    }

    fn build_command(&self, spec: &CommandSpec) -> anyhow::Result<ResolvedCommand> {
        let mut args = self.ssh_argv(spec.tty);

        // `--` stops ssh from interpreting the remote command as its own options.
        args.push("--".to_owned());

        // The one place a non-POSIX host differs. Everything above this line —
        // and every caller — still builds an ordinary POSIX shell line, which is
        // the invariant: there is no `if windows` in feature code, only here.
        let line = spec_to_shell_line(spec);
        args.push(match winshell::shell_for(&self.host.alias) {
            RemoteShell::Posix => line,
            RemoteShell::Via { bash } => winshell::wrap(&bash, &line),
        });

        Ok(ResolvedCommand {
            program: "ssh".into(),
            args: args.into_iter().map(Into::into).collect(),
            // The *local* ssh process's environment, not the remote command's —
            // the latter is baked into the shell line above. This is where the
            // askpass helper is wired in so credential prompts become native UI.
            env: askpass::env_for_gui_prompts(),
        })
    }

    async fn exec(&self, spec: &CommandSpec) -> anyhow::Result<Output> {
        let resolved = self.build_command(&spec.clone().tty(Tty::None))?;

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
        use tokio::io::AsyncWriteExt;

        let resolved = self.build_command(&spec.clone().tty(Tty::None))?;

        let mut cmd = tokio::process::Command::new(&resolved.program);
        cmd.no_console_window();
        cmd.args(&resolved.args);
        for (k, v) in &resolved.env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        // Closed before awaiting output: the remote `cat` reads until EOF, so
        // holding stdin open would deadlock.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        stdin.write_all(input).await?;
        stdin.flush().await?;
        drop(stdin);

        let out = child.wait_with_output().await?;
        Ok(Output {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
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
        self.master.stop().await;
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
    /// failed logins from zmux got the client banned mid-session — which reads
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
