//! Driving a Claude Code session on any [`Target`].
//!
//! A session is the real `claude` CLI running in a PTY — locally, or through
//! `ssh` on a remote host. rmux watches the rendered screen and answers it.
//!
//! **The architectural point.** The previous generation ran Claude on the host,
//! relayed its state to a server, and had a poller press keys on our behalf.
//! Every failure it suffered came from that gap: a prompt could be answered after
//! the screen had moved on (an eaten answer), or shown after it had already been
//! resolved (a ghost card). Here the PTY is ours. The screen we parse and the
//! keystroke we send are the same object, with no hop in between, so neither
//! failure is expressible.

use std::sync::Arc;

use parking_lot::Mutex;
use rmux_term::{TermSize, Terminal, TerminalEvent};
use rmux_transport::{CommandSpec, Target, Tty};

pub mod auth;
pub mod profile;
pub mod screen;
pub mod usage;

/// Claude Code's interface as data, re-exported from where it now lives.
///
/// `keys`, `launch`, `sessions` and `transcript` moved to `rmux-claude-core` so
/// **`rmux-agent`'s bridge can share the one definition** without dragging
/// `reqwest` and `alacritty_terminal` onto every host rmux touches. Re-exported
/// under the paths every existing caller already knows — `rmux_claude::keys`,
/// `rmux_claude::transcript` — because renaming them buys nothing.
pub use rmux_claude_core::{keys, launch, sessions, transcript};
pub use rmux_claude_core::{launch_line, Rendering};

pub use screen::{Choice, ClaudeState, Prompt, Screen};
pub use sessions::SessionInfo;

/// A running Claude session.
pub struct ClaudeSession {
    terminal: Arc<Terminal>,
    screen: Arc<Mutex<Screen>>,
}

impl ClaudeSession {
    /// Launch `claude` on `target` in `cwd`.
    ///
    /// Run through a **login shell** rather than executed directly: `claude` is
    /// usually installed under `~/.local/bin` or a version manager's directory,
    /// neither of which is on the PATH of a non-interactive ssh command. Skipping
    /// the login shell produces "claude: command not found" on a host where it is
    /// plainly installed — verified on a real server during development.
    pub fn start<T: Target + ?Sized>(
        target: &T,
        cwd: Option<&str>,
        args: &[String],
        size: TermSize,
    ) -> anyhow::Result<Self> {
        Self::start_resuming(target, cwd, None, args, size)
    }

    /// Launch `claude`, optionally resuming an existing conversation.
    ///
    /// See [`Rendering`] for why the default is inline.
    ///
    /// Resuming keeps the context of previous work, which is usually worth far
    /// more than a clean slate — so the UI offers it before it offers a new one.
    pub fn start_resuming<T: Target + ?Sized>(
        target: &T,
        cwd: Option<&str>,
        resume: Option<&str>,
        args: &[String],
        size: TermSize,
    ) -> anyhow::Result<Self> {
        // One builder for both paths — the agent path had drifted from this one,
        // and the rendering flags must not apply to only half of them.
        let launch = launch_line(resume, args, Rendering::default());

        // `login_shell()`, not a hand-built `$SHELL -l`: it carries `-i` as well,
        // and that is what makes `.zshrc` load. `zsh -l` reads `.zprofile` and
        // `.zlogin` and nothing else, while every version manager writes its PATH
        // into `.zshrc` — so without it this is "command not found: claude" on a
        // host where claude plainly works when typed. This path had drifted from
        // the agent's; both build the shell the same way now.
        let mut spec = CommandSpec::login_shell()
            .arg("-c")
            .arg(launch)
            .tty(Tty::Allocate)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor");

        if let Some(cwd) = cwd {
            spec = spec.cwd(cwd.to_owned());
        }

        Self::start_with_spec(target, spec, cwd, size)
    }

    /// Start from a command the caller built.
    ///
    /// The screen parsing does not care what produced the bytes, so a session
    /// hosted by the agent is the same object as one spawned directly.
    pub fn start_with_spec<T: Target + ?Sized>(
        target: &T,
        spec: CommandSpec,
        cwd: Option<&str>,
        size: TermSize,
    ) -> anyhow::Result<Self> {
        let command = target.build_command(&spec)?;
        // Local `cwd` is applied by the PTY; a remote one is a `cd` in the shell
        // line, and passing it here too would resolve it against this machine.
        // A spec that carries no cwd of its own (the agent applies it) must not
        // get one here either.
        let local_cwd = (target.id().is_local() && spec.cwd.is_some())
            .then(|| cwd.map(camino::Utf8Path::new))
            .flatten();

        let terminal = Arc::new(Terminal::spawn(&command, local_cwd, size)?);
        let screen = Arc::new(Mutex::new(Screen::new(size.cols, size.rows)));

        // Mirror everything the PTY produces into the emulator, so `state()` is
        // always describing the screen as it is right now.
        let (backlog, mut receiver) = terminal.attach();
        screen.lock().feed(&backlog);

        let mirror = Arc::clone(&screen);
        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(TerminalEvent::Output(chunk)) => mirror.lock().feed(&chunk),
                    Ok(TerminalEvent::Exited { .. }) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Dropped output means the emulated screen no longer
                        // matches the real one, and a stale screen is exactly
                        // what produces a ghost prompt. Say so rather than
                        // quietly parsing a screen we know is wrong.
                        tracing::warn!(chunks = n, "claude screen fell behind; state may be stale");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(Self { terminal, screen })
    }

    pub fn terminal(&self) -> &Arc<Terminal> {
        &self.terminal
    }

    /// What Claude is showing right now.
    pub fn state(&self) -> ClaudeState {
        self.screen.lock().state()
    }

    /// The Claude sessions already recorded for `folder` on `target`.
    ///
    /// Offered before starting, so a conversation can be resumed rather than
    /// restarted — the context of yesterday's work is usually worth more than a
    /// clean slate.
    /// Every Claude session on the host, whichever folder it was started in.
    ///
    /// The listing that makes resuming practical. Finding the folder first is
    /// backwards — the operator remembers the *conversation*, not which of forty
    /// checkouts it happened in — and each transcript records its own `cwd`, so
    /// rmux can simply read where each one belongs and set the folder itself.
    pub async fn list_all<T: Target + ?Sized>(target: &T) -> anyhow::Result<Vec<SessionInfo>> {
        let spec = CommandSpec::new("sh")
            .arg("-c")
            .arg(sessions::list_all_sessions_script())
            .tty(Tty::None);

        let out = target.exec(&spec).await?;
        // A host Claude has never run on exits non-zero from the `[ -d ]` guard
        // on some shells; that is "no sessions", not a failure.
        if out.status != 0 {
            return Ok(Vec::new());
        }
        Ok(sessions::parse_all_sessions(out.stdout.as_bytes()))
    }

    pub async fn list<T: Target + ?Sized>(
        target: &T,
        folder: &str,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let spec = CommandSpec::new("sh")
            .arg("-c")
            .arg(sessions::list_sessions_script(folder))
            .tty(Tty::None);

        let out = target.exec(&spec).await?;
        // A folder Claude has never run in exits non-zero from the `[ -d ]`
        // guard on some shells; that is "no sessions", not a failure.
        if out.status != 0 {
            return Ok(Vec::new());
        }
        Ok(sessions::parse_sessions(out.stdout.as_bytes()))
    }

    /// The rendered screen as plain text — for diagnostics and tests.
    pub fn screen_text(&self) -> String {
        self.screen.lock().lines().join("\n")
    }

    /// Send a message to Claude.
    pub async fn send(&self, text: &str) -> anyhow::Result<()> {
        for (i, chunk) in keys::send_message(text).into_iter().enumerate() {
            if i > 0 {
                // Applied locally, never as a round trip — a network hop here is
                // how the old design lost the Enter on a slow link.
                tokio::time::sleep(keys::SUBMIT_SETTLE).await;
            }
            self.terminal.write(&chunk)?;
        }
        Ok(())
    }

    /// Interrupt whatever Claude is doing.
    pub fn interrupt(&self) -> anyhow::Result<()> {
        self.terminal.write(keys::interrupt())
    }

    pub fn resize(&self, size: TermSize) -> anyhow::Result<()> {
        self.screen.lock().resize(size.cols, size.rows);
        self.terminal.resize(size)
    }

    pub fn is_running(&self) -> bool {
        self.terminal.exit_code().is_none()
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[test]
    fn the_launch_shell_reads_rc_files() {
        // `zsh -l` sources `.zprofile` and `.zlogin` and *not* `.zshrc`, which is
        // where every version manager writes its PATH — so without `-i` this is
        // "command not found: claude" on a host where claude plainly works when
        // typed. Reported on a real server, twice: this path had drifted from
        // the agent's, which had already been fixed.
        let target = rmux_transport::LocalTarget::new();
        let spec = CommandSpec::login_shell().arg("-c").arg("claude");
        let resolved = target.build_command(&spec).unwrap();
        let args: Vec<_> = resolved.args.iter().map(|a| a.to_string_lossy()).collect();
        assert!(args.iter().any(|a| a == "-i"), "not an interactive shell: {args:?}");
        assert!(args.iter().any(|a| a == "-l"), "not a login shell: {args:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_transport::LocalTarget;

    /// Wait for a condition, or give up.
    async fn eventually(mut check: impl FnMut() -> bool, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if check() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        false
    }

    /// A stand-in that draws a Claude-style dialog and echoes what is pressed, so
    /// the whole loop can be exercised without a real Claude installed.
    fn fake_claude(script: &str, size: TermSize) -> ClaudeSession {
        let spec = CommandSpec::new("sh").arg("-c").arg(script).tty(Tty::Allocate);
        let command = LocalTarget::new().build_command(&spec).unwrap();
        let terminal = Arc::new(Terminal::spawn(&command, None, size).unwrap());
        let screen = Arc::new(Mutex::new(Screen::new(size.cols, size.rows)));

        let (backlog, mut receiver) = terminal.attach();
        screen.lock().feed(&backlog);
        let mirror = Arc::clone(&screen);
        tokio::spawn(async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    TerminalEvent::Output(chunk) => mirror.lock().feed(&chunk),
                    TerminalEvent::Exited { .. } => break,
                }
            }
        });

        ClaudeSession { terminal, screen }
    }

    const DIALOG_SCRIPT: &str = r#"
printf 'Do you want to make this edit to a.txt?\r\n'
printf '\r\n'
printf ' \xe2\x9d\xaf 1. Yes\r\n'
printf '   2. No\r\n'
read -r answer
printf 'ANSWERED:%s\r\n' "$answer"
sleep 2
"#;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_dialog_on_screen_becomes_structured_state() {
        let session = fake_claude(DIALOG_SCRIPT, TermSize { cols: 60, rows: 12 });

        let found =
            eventually(|| session.state().prompt.is_some(), std::time::Duration::from_secs(5))
                .await;
        assert!(
            found,
            "the dialog should have been detected; screen: {:?}",
            session.screen.lock().lines()
        );

        let prompt = session.state().prompt.unwrap();
        assert_eq!(prompt.question, "Do you want to make this edit to a.txt?");
        assert_eq!(prompt.choices.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_message_is_delivered_and_submitted() {
        let session = fake_claude(
            "read -r line; printf 'GOT:%s\\r\\n' \"$line\"; sleep 2",
            TermSize { cols: 60, rows: 10 },
        );

        session.send("run the tests").await.unwrap();

        let delivered = eventually(
            || String::from_utf8_lossy(&session.terminal.replay()).contains("GOT:run the tests"),
            std::time::Duration::from_secs(5),
        )
        .await;
        // Proves the Enter arrived as its own write: folded into the text, `read`
        // would still be waiting.
        assert!(delivered, "screen: {:?}", session.screen.lock().lines());
    }
}
