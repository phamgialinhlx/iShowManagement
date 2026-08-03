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
    CommandSpec, Output, Platform, ResolvedCommand, SshHostId, Target, TargetId, Tty,
    spec_to_shell_line,
};

pub mod askpass;
pub mod forward;
pub mod config;
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
        let probe = self.exec(&CommandSpec::new("uname").arg("-s").tty(Tty::None)).await?;

        // **A failed `uname` is the Windows signal, not an error.** OpenSSH for
        // Windows hands the command to `cmd.exe` unless `DefaultShell` says
        // otherwise, and `cmd` has no `uname` — so the very first thing rmux
        // does fails, and before this the connection simply never completed.
        // Measured on a real Windows 11 host.
        if probe.status != 0 {
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
        let mut args = self.ssh_argv(Tty::None);
        args.push("--".to_owned());
        args.push(winshell::probe_script());

        let out = tokio::process::Command::new("ssh")
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SshTarget {
        SshTarget::new(SshHostId::new("devbox"))
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
