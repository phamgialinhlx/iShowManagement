//! SSH command construction (Phase 2).
//!
//! We connect **by Host alias** and let the system `ssh` resolve everything from
//! `~/.ssh/config` (keys, user, port, ProxyJump). ControlMaster multiplexing
//! means Console authenticates once and later exec/forward/SOCKS calls reuse the
//! same connection. Mirrors `references/tsmanager/server/ssh.js`.

use std::path::PathBuf;

use portable_pty::CommandBuilder;

/// Unix-socket path for this alias's ControlMaster connection. Scoped by pid so
/// concurrent app instances don't collide.
pub fn control_path(alias: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ism-{}-{}.ctl", std::process::id(), alias));
    p
}

/// Base `ssh` options shared by interactive sessions and (later) `exec`.
fn control_args(alias: &str) -> Vec<String> {
    let cp = control_path(alias);
    vec![
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        format!("ControlPath={}", cp.display()),
        "-o".into(),
        "ControlPersist=60".into(),
        "-o".into(),
        "ServerAliveInterval=30".into(),
    ]
}

/// argv (after the program name) for an interactive session to `alias`.
/// `-tt` forces remote PTY allocation so full-screen apps (tmux) work.
pub fn ssh_args(alias: &str, interactive: bool) -> Vec<String> {
    let mut args = control_args(alias);
    if interactive {
        args.push("-tt".into());
    }
    args.push(alias.to_string());
    args
}

/// A ready-to-spawn `ssh` command for `alias`. When `remote` is set it is passed
/// as the remote command (e.g. the tmux attach-or-create line); otherwise ssh
/// starts the login shell.
pub fn ssh_command(alias: &str, remote: Option<&str>) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("ssh");
    for a in ssh_args(alias, true) {
        cmd.arg(a);
    }
    if let Some(r) = remote {
        cmd.arg(r);
    }
    cmd.env("TERM", "xterm-256color");
    cmd
}

/// The tmux attach-or-create remote command for `session`.
pub fn tmux_remote(session: &str) -> String {
    format!("tmux new-session -A -s {session}")
}

/// Where a command runs: the app's own machine, or a remote host by alias.
#[derive(Clone, Copy)]
pub enum Target<'a> {
    Local,
    Remote(&'a str),
}

/// Result of a one-shot command (manager reads/actions).
pub struct ExecOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// argv for a non-interactive `exec` over the shared connection. `BatchMode=yes`
/// means it reuses an existing ControlMaster and *fails fast* rather than
/// hanging on a password prompt if no master is up (open a Console first).
pub(crate) fn exec_args(alias: &str) -> Vec<String> {
    let mut a = control_args(alias);
    a.push("-o".into());
    a.push("BatchMode=yes".into());
    a.push(alias.to_string());
    a
}

/// `-o` options (no alias) for scp/rsync: reuse the ControlMaster, fail fast.
pub fn transfer_opts(alias: &str) -> Vec<String> {
    let mut a = control_args(alias);
    a.push("-o".into());
    a.push("BatchMode=yes".into());
    a
}

/// A background `ssh -N -L 127.0.0.1:<local>:127.0.0.1:<remote>` command (port forward).
pub fn forward_command(alias: &str, local: u16, remote: u16) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("ssh");
    for a in control_args(alias) {
        cmd.arg(a);
    }
    cmd.arg("-N");
    cmd.arg("-L");
    cmd.arg(format!("127.0.0.1:{local}:127.0.0.1:{remote}"));
    cmd.arg(alias);
    cmd.env("TERM", "xterm-256color");
    cmd
}

/// A background `ssh -N -D 127.0.0.1:<port>` command (SOCKS proxy).
pub fn socks_command(alias: &str, port: u16) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("ssh");
    for a in control_args(alias) {
        cmd.arg(a);
    }
    cmd.arg("-N");
    cmd.arg("-D");
    cmd.arg(format!("127.0.0.1:{port}"));
    cmd.arg(alias);
    cmd.env("TERM", "xterm-256color");
    cmd
}

/// Poll until a ControlMaster for `alias` is ready (multiplex-able), or `max`
/// elapses. Lets exec/tail calls ride the console's shared connection instead of
/// racing a *second* one open — which, on a cloudflared ProxyCommand host, ssh
/// "disables multiplexing" and dials independently, and that dial is intermittently
/// torn down ("Connection closed by UNKNOWN port 65535") and leaks an orphaned
/// cloudflared. If no master appears (no live console), we just proceed.
pub(crate) async fn wait_for_master(alias: &str, max: std::time::Duration) {
    use tokio::process::Command;
    let cp = control_path(alias);
    let deadline = std::time::Instant::now() + max;
    loop {
        let ready = Command::new("ssh")
            .arg("-O")
            .arg("check")
            .arg("-o")
            .arg(format!("ControlPath={}", cp.display()))
            .arg(alias)
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ready || std::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

/// Run `command` on `target` and capture its output, with a timeout.
pub async fn exec(target: Target<'_>, command: &str, timeout: std::time::Duration) -> ExecOutput {
    use tokio::process::Command;
    let mut cmd = match target {
        Target::Local => {
            let mut c = Command::new("sh");
            c.arg("-c").arg(command);
            c
        }
        Target::Remote(alias) => {
            // Ride the console's ControlMaster rather than racing a second dial.
            wait_for_master(alias, std::time::Duration::from_secs(3)).await;
            let mut c = Command::new("ssh");
            for a in exec_args(alias) {
                c.arg(a);
            }
            c.arg(command); // ssh runs this as the remote command
            c
        }
    };
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) => ExecOutput {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Ok(Err(e)) => ExecOutput {
            ok: false,
            stdout: String::new(),
            stderr: e.to_string(),
        },
        Err(_) => ExecOutput {
            ok: false,
            stdout: String::new(),
            stderr: "command timed out".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_args_include_multiplexing_and_keepalive() {
        let a = ssh_args("myhost", true);
        let joined = a.join(" ");
        assert!(joined.contains("ControlMaster=auto"), "{joined}");
        assert!(joined.contains("ControlPersist=60"), "{joined}");
        assert!(joined.contains("ServerAliveInterval=30"), "{joined}");
        assert!(joined.contains("ControlPath="), "{joined}");
        // -tt present for interactive; alias is the final argument.
        assert!(a.contains(&"-tt".to_string()));
        assert_eq!(a.last().unwrap(), "myhost");
    }

    #[test]
    fn non_interactive_omits_tt() {
        let a = ssh_args("h", false);
        assert!(!a.contains(&"-tt".to_string()));
        assert_eq!(a.last().unwrap(), "h");
    }

    #[test]
    fn control_path_is_pid_scoped_under_tmp() {
        let p = control_path("web");
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("ism-"));
        assert!(name.ends_with("-web.ctl"), "{name}");
    }

    #[test]
    fn tmux_remote_is_attach_or_create() {
        assert_eq!(tmux_remote("work"), "tmux new-session -A -s work");
    }
}
