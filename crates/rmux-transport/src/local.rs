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

/// Point a bare `sh` at a real shell on Windows.
///
/// **Every feature in rmux asks for `sh -c <posix script>`** — listing Claude's
/// sessions, reading a transcript, the whole of `rmux-fs`, metrics. That is the
/// design: one code path for local and remote, POSIX everywhere, with the
/// platform branch living in the `Target` impl. `SshTarget` already honours it
/// for Windows *hosts* (`winshell.rs`); `LocalTarget` did not, so on a Windows
/// machine `Command::new("sh")` looked for `sh.exe` on `PATH`, did not find it,
/// and every one of those features failed with **"program not found"**.
///
/// Reported as "we cannot see or list existing Claude sessions on our local
/// host", which is that error arriving at the one feature whose failure is most
/// visible — but it was never specific to Claude.
///
/// Only `sh` and `bash` are rewritten. Anything else is a real program name and
/// resolving it here would be this function deciding what the caller meant.
#[cfg(windows)]
fn resolve_program(program: &str) -> String {
    if program != "sh" && program != "bash" {
        return program.to_owned();
    }
    crate::WINDOWS_BASH_CANDIDATES
        .iter()
        .find(|path| std::path::Path::new(path).exists())
        .map(|found| (*found).to_owned())
        // No bash on this machine: leave the name alone so the error names the
        // program that is missing. Rewriting it to a path that does not exist
        // would report the *candidate* instead, sending whoever reads the log to
        // a directory that was never going to be there.
        .unwrap_or_else(|| program.to_owned())
}

#[cfg(not(windows))]
fn resolve_program(program: &str) -> String {
    program.to_owned()
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
            resolve_program(&spec.program)
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

    /// **A bare `sh -c` must run a POSIX script, on every platform.**
    ///
    /// This is the contract the whole app is written against: `rmux-fs`, search,
    /// metrics, transcripts and Claude's session list all build
    /// `CommandSpec::new("sh").arg("-c")`, and the platform branch is supposed to
    /// live in the `Target` impl. `LocalTarget` did not honour it, so on Windows
    /// every one of those failed with "program not found" — reported as "we
    /// cannot list Claude sessions on our local host".
    ///
    /// The tests below noticed the same thing and *worked around* it, switching
    /// to `cmd` on Windows with a comment explaining that `sh` is not a program
    /// there. That turned a live bug into a documented workaround, so nothing
    /// failed while every user-facing POSIX call did. This asserts the contract
    /// instead.
    ///
    /// Skipped, loudly, where there is no POSIX shell to find: a machine without
    /// Git for Windows genuinely cannot do this, and a hard failure there would
    /// be a test failing for the environment rather than the code.
    ///
    /// **Running it is not enough — see the sibling test below.** `sh` resolves
    /// on `PATH` inside Git Bash, which is where `cargo test` is usually typed,
    /// so this passes with or without the fix. The app is a GUI process and has
    /// no such `PATH`. Measured: `sh` is on `PATH` in Git Bash and absent in
    /// PowerShell, which is why the bug reached users while the suite was green.
    #[tokio::test]
    async fn a_posix_script_runs_through_a_bare_sh() {
        if cfg!(windows)
            && !crate::WINDOWS_BASH_CANDIDATES
                .iter()
                .any(|p| std::path::Path::new(p).exists())
        {
            eprintln!("skipped: no POSIX shell on this machine");
            return;
        }

        let target = LocalTarget::new();
        // Deliberately POSIX-only syntax: `cmd` cannot parse `$(...)`, and a `;`
        // separator, so passing this proves a real shell ran it.
        let spec = CommandSpec::new("sh")
            .arg("-c")
            .arg("x=$(echo one); for i in 1 2; do printf '%s-' \"$x\"; done; echo done");
        let out = target.exec(&spec).await.unwrap();

        assert!(out.ok(), "status {} stderr {:?}", out.status, out.stderr);
        assert_eq!(out.stdout.trim(), "one-one-done", "stderr {:?}", out.stderr);
    }

    /// `sh` must be resolved to an absolute shell, not left to `PATH`.
    ///
    /// This is the assertion that actually holds the fix down. Spawning proves
    /// nothing on a developer's machine: `cargo test` is typed in Git Bash,
    /// where `sh` *is* on `PATH`, so the exec test above passes with the fix
    /// reverted — verified, by reverting it. The app is a GUI process launched
    /// from Explorer and inherits no such `PATH`, which is the whole reason
    /// users saw "program not found" while the suite was green.
    ///
    /// Checking `build_command`'s output instead is immune to that: the
    /// resolution either happened or it did not.
    #[test]
    #[cfg(windows)]
    fn a_bare_sh_is_resolved_to_a_real_shell_on_windows() {
        if !crate::WINDOWS_BASH_CANDIDATES.iter().any(|p| std::path::Path::new(p).exists()) {
            eprintln!("skipped: no POSIX shell on this machine");
            return;
        }

        let target = LocalTarget::new();
        let resolved =
            target.build_command(&CommandSpec::new("sh").arg("-c").arg("true")).unwrap();
        let program = resolved.program.to_string_lossy().into_owned();

        assert_ne!(program, "sh", "left as a bare name, so it depends on PATH");
        assert!(
            std::path::Path::new(&program).is_absolute(),
            "must be an absolute path, got {program:?}"
        );
        assert!(
            std::path::Path::new(&program).exists(),
            "resolved to something that is not there: {program:?}"
        );
        // The arguments must survive untouched, or the script is not what ran.
        let args: Vec<String> =
            resolved.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, vec!["-c".to_owned(), "true".to_owned()]);
    }

    /// A real program name is never rewritten.
    ///
    /// The resolution above is narrow on purpose — widen it and this function
    /// starts deciding what the caller meant by `git` or `docker`.
    #[test]
    fn a_program_that_is_not_a_shell_is_left_alone() {
        let target = LocalTarget::new();
        let resolved = target.build_command(&CommandSpec::new("git").arg("status")).unwrap();
        assert_eq!(resolved.program.to_string_lossy(), "git");
    }

    /// Run a one-liner through whatever shell this machine actually has.
    ///
    /// Kept using `cmd` on Windows: these two assert `exec`'s plumbing — stdout
    /// capture and a non-zero status with stderr — which has nothing to do with
    /// which shell runs, and `cmd` proves the plumbing works for a *native*
    /// program too. The POSIX contract is asserted directly above.
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
