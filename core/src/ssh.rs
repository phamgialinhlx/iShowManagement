//! SSH command construction (Phase 2).
//!
//! We connect **by Host alias** and let the system `ssh` resolve everything from
//! `~/.ssh/config` (keys, user, port, ProxyJump). ControlMaster multiplexing
//! means Console authenticates once and later exec/forward/SOCKS calls reuse the
//! same connection. Mirrors `references/tsmanager/server/ssh.js`.

#[cfg(unix)]
use std::path::PathBuf;

use portable_pty::CommandBuilder;

/// Unix-socket path for this alias's ControlMaster connection. Scoped by pid so
/// concurrent app instances don't collide. Unix-only — Windows OpenSSH has no
/// multiplexing, so nothing there builds a control path.
#[cfg(unix)]
pub fn control_path(alias: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ism-{}-{}.ctl", std::process::id(), alias));
    p
}

/// Base `ssh` options shared by interactive sessions and (later) `exec`.
///
/// ControlMaster multiplexing is Unix-only — it needs a Unix domain socket, which
/// Windows OpenSSH does not implement. On Windows we omit those options and each
/// invocation dials (and re-authenticates) its own connection: correct, but
/// slower, and a keyboard-interactive host will prompt per call.
fn control_args(alias: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    #[cfg(unix)]
    {
        let cp = control_path(alias);
        args.push("-o".into());
        args.push("ControlMaster=auto".into());
        args.push("-o".into());
        args.push(format!("ControlPath={}", cp.display()));
        args.push("-o".into());
        args.push("ControlPersist=60".into());
    }
    #[cfg(not(unix))]
    let _ = alias;
    args.push("-o".into());
    args.push("ServerAliveInterval=30".into());
    args
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
///
/// The trailing `set` commands make clipboard work out of the box (see
/// plans/2026-07-23-bidirectional-clipboard.md): `set-clipboard on` forwards
/// inner apps' OSC 52 (the default `external` swallows it), and the
/// `clipboard` terminal-feature tells tmux our terminal accepts OSC 52 —
/// stock xterm-256color terminfo lacks `Ms`, so tmux would otherwise stay
/// silent. `\;` reaches tmux as its command separator; the remote shell eats
/// the backslash.
pub fn tmux_remote(session: &str) -> String {
    format!(
        "tmux new-session -A -s {session} \\; set -g set-clipboard on \\; set -as terminal-features ',xterm*:clipboard'"
    )
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
///
/// No-op on Windows: `control_args` omits ControlMaster there, so no master can
/// ever appear and polling would burn the full timeout on every single exec.
#[cfg(not(unix))]
pub(crate) async fn wait_for_master(_alias: &str, _max: std::time::Duration) {}

/// Unix implementation — see the Windows no-op stub above.
#[cfg(unix)]
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

/// Like [`exec`], but pipes `input` to the command's stdin. Binary-safe and
/// uncoupled from the argument-length limit, so it can move a file's worth of
/// bytes (the save path). Local runs `sh -c`; remote rides the ControlMaster.
pub async fn exec_with_input(
    target: Target<'_>,
    command: &str,
    input: &[u8],
    timeout: std::time::Duration,
) -> ExecOutput {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;
    let mut cmd = match target {
        Target::Local => {
            let mut c = Command::new("sh");
            c.arg("-c").arg(command);
            c
        }
        Target::Remote(alias) => {
            wait_for_master(alias, std::time::Duration::from_secs(3)).await;
            let mut c = Command::new("ssh");
            for a in exec_args(alias) {
                c.arg(a);
            }
            c.arg(command); // ssh runs this as the remote command
            c
        }
    };
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ExecOutput {
                ok: false,
                stdout: String::new(),
                stderr: e.to_string(),
            }
        }
    };
    // Write stdin concurrently with reading stdout/stderr. A sequential
    // write-then-read would deadlock once the payload exceeds the pipe buffer:
    // write_all blocks on a full stdin pipe while the command can't drain it
    // (its stdout pipe is full and unread). stdin is dropped at the end of
    // `write`, signaling EOF to the command.
    let stdin = child.stdin.take();
    let write = async move {
        if let Some(mut stdin) = stdin {
            // A write error (e.g. the command closes stdin early) is intentionally
            // swallowed: it does not always surface via the command's exit status
            // (a partial write that closes with a clean EOF exits 0). Callers that
            // need byte-exact delivery — notably files::save_command — verify the
            // payload size themselves before acting on it.
            let _ = stdin.write_all(input).await;
        }
    };
    match tokio::time::timeout(timeout, async move {
        let ((), out) = tokio::join!(write, child.wait_with_output());
        out
    })
    .await
    {
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

    #[cfg(unix)]
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

    /// Windows OpenSSH has no connection multiplexing, so those options must be
    /// absent — passing ControlPath there fails the connection outright. The
    /// keepalive and argument shape still apply.
    #[cfg(not(unix))]
    #[test]
    fn ssh_args_omit_multiplexing_on_windows() {
        let a = ssh_args("myhost", true);
        let joined = a.join(" ");
        assert!(!joined.contains("ControlMaster"), "{joined}");
        assert!(!joined.contains("ControlPath"), "{joined}");
        assert!(!joined.contains("ControlPersist"), "{joined}");
        assert!(joined.contains("ServerAliveInterval=30"), "{joined}");
        assert!(a.contains(&"-tt".to_string()));
        assert_eq!(a.last().unwrap(), "myhost");
    }

    #[test]
    fn non_interactive_omits_tt() {
        let a = ssh_args("h", false);
        assert!(!a.contains(&"-tt".to_string()));
        assert_eq!(a.last().unwrap(), "h");
    }

    #[cfg(unix)]
    #[test]
    fn control_path_is_pid_scoped_under_tmp() {
        let p = control_path("web");
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("ism-"));
        assert!(name.ends_with("-web.ctl"), "{name}");
    }

    #[test]
    fn tmux_remote_is_attach_or_create() {
        assert_eq!(
            tmux_remote("work"),
            "tmux new-session -A -s work \\; set -g set-clipboard on \\; set -as terminal-features ',xterm*:clipboard'"
        );
    }

    // Local exec runs `sh -c`, so these two are Unix-only. The remote path they
    // stand in for is unaffected: it executes on the Linux host regardless of
    // client OS.
    #[cfg(unix)]
    #[tokio::test]
    async fn exec_with_input_pipes_stdin_to_cat() {
        let out = exec_with_input(
            Target::Local,
            "cat",
            b"hello world",
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(out.ok, "stderr: {}", out.stderr);
        assert_eq!(out.stdout, "hello world");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_with_input_handles_large_payload() {
        // ~240 KB of valid UTF-8 — larger than the OS pipe buffer (64 KB), so
        // this guards against a sequential write-then-read deadlock (write_all
        // would block on a full stdin pipe while the command's stdout pipe fills
        // and waits for a reader that hasn't started). The save path sends file
        // content this way, so large-payload transport must not hang. Valid UTF-8
        // round-trips exactly through the lossy-String stdout — the actual use
        // case, since only valid-UTF-8 files are editable.
        let bytes: Vec<u8> = b"hello world ".repeat(20_000);
        let out = exec_with_input(
            Target::Local,
            "cat",
            &bytes,
            std::time::Duration::from_secs(10),
        )
        .await;
        assert!(out.ok, "stderr: {}", out.stderr);
        assert_eq!(out.stdout.as_bytes(), &bytes[..]);
    }
}
