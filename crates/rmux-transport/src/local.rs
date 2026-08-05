//! The local machine as a [`Target`].
//!
//! This is what makes rmux a normal IDE when you are not connected to anything.

use std::collections::BTreeMap;

use async_trait::async_trait;

use crate::{
    CommandSpec, NoConsoleWindow, Output, Platform, ResolvedCommand, Target, TargetId, Tty,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct LocalTarget;

impl LocalTarget {
    pub fn new() -> Self {
        Self
    }
}

/// The user's shell, or a reasonable default per platform.
///
/// **On Windows this looks for bash before falling back to `COMSPEC`.** rmux
/// asks for a login shell as `$SHELL -l -i`, and `cmd.exe` understands neither
/// flag — so resolving to `COMSPEC` produced a shell that refused to start, on
/// the one platform where the whole session model already runs through a POSIX
/// layer. `SHELL` itself is no help: Windows sets it to `cmd.exe` and MSYS
/// leaves it there, and even when it *is* corrected it holds an MSYS path
/// (`/usr/bin/bash`) that a native Windows process cannot execute. So the disk
/// is asked instead, which is the only answer that is true for the process
/// doing the spawning.
#[cfg(windows)]
fn default_shell() -> String {
    if let Some(found) = crate::WINDOWS_BASH_CANDIDATES
        .iter()
        .find(|path| std::path::Path::new(path).exists())
    {
        return (*found).to_owned();
    }
    std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_owned())
}

#[cfg(not(windows))]
fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
}

#[async_trait]
impl Target for LocalTarget {
    fn id(&self) -> &TargetId {
        &TargetId::Local
    }

    fn build_command(&self, spec: &CommandSpec) -> anyhow::Result<ResolvedCommand> {
        // `$SHELL` is a portable request for "the login shell", not a literal
        // program name — resolve it here since there is no remote shell to expand it.
        let program = if spec.program == "$SHELL" {
            default_shell()
        } else {
            spec.program.clone()
        };

        Ok(ResolvedCommand {
            program: program.into(),
            args: spec.args.iter().map(Into::into).collect(),
            env: spec.env.clone(),
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
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
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
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        // Take the handle so it is dropped (closing stdin) before we await the
        // output — a child reading to EOF would otherwise block forever.
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
        Some(Platform::current())
    }
}

/// Environment a freshly spawned local terminal should inherit.
///
/// Deliberately minimal: we set the variables a terminal needs and let the
/// user's shell rc files do everything else.
pub fn terminal_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("TERM".to_owned(), "xterm-256color".to_owned());
    env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
    env.insert("TERM_PROGRAM".to_owned(), "rmux".to_owned());
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a one-liner through whatever shell this machine actually has.
    ///
    /// These used to spawn `echo` and `sh` directly, which are not programs on
    /// Windows — `echo` is a `cmd` builtin and `sh` is not on `PATH` — so both
    /// tests failed there with "program not found" long before anything in this
    /// file was touched. The *behaviour* being asserted is not platform
    /// specific, so only the way the shell is invoked is.
    fn shell(script: &str) -> CommandSpec {
        if cfg!(windows) {
            CommandSpec::new("cmd").arg("/c").arg(script)
        } else {
            CommandSpec::new("sh").arg("-c").arg(script)
        }
    }

    #[tokio::test]
    async fn exec_captures_stdout() {
        let target = LocalTarget::new();
        let out = target.exec(&shell("echo hi")).await.unwrap();
        assert!(out.ok());
        assert_eq!(out.stdout.trim(), "hi");
    }

    #[tokio::test]
    async fn exec_reports_failure_with_stderr() {
        let target = LocalTarget::new();
        // `cmd` needs the redirect written as `1>&2`, and separates statements
        // with `&` rather than `;`.
        let script = if cfg!(windows) {
            "echo boom 1>&2 & exit 3"
        } else {
            "echo boom >&2; exit 3"
        };

        let out = target.exec(&shell(script)).await.unwrap();
        assert_eq!(out.status, 3);
        assert!(out.stdout_or_err().is_err());
        assert!(out.stderr.contains("boom"));
    }

    #[test]
    fn login_shell_resolves_locally() {
        let target = LocalTarget::new();
        let resolved = target.build_command(&CommandSpec::login_shell()).unwrap();
        // Unlike the SSH target, nothing downstream will expand `$SHELL` for us.
        assert_ne!(resolved.program, "$SHELL");
        assert!(!resolved.program.is_empty());
    }
}
