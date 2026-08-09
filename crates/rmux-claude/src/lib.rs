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

/// How long to wait for the caret to land after pressing an option's digit.
///
/// Bounded rather than open-ended: a dialog that ignores the digit must fail
/// quickly and say so, not leave a disabled button spinning. Over SSH a repaint
/// is a round trip, so a single sleep would be either too short on a slow link
/// or needlessly slow on a fast one — hence polling.
const CONFIRM_POLLS: usize = 12;
const CONFIRM_POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(60);

pub mod auth;
pub mod keys;
pub mod profile;
pub mod screen;
pub mod sessions;
pub mod transcript;
pub mod usage;

pub use screen::{Choice, ClaudeState, Prompt, Screen};
pub use sessions::SessionInfo;

/// How Claude should draw itself.
///
/// Claude's **fullscreen** mode moves to the alternate screen and takes over the
/// mouse with SGR tracking. In a terminal emulator inside a webview that is
/// ruinous, and all three of the symptoms rmux shipped with came from it:
///
/// - A drag is *sent to Claude* instead of making a selection, so there is never
///   a selection for the copy shortcut to take. It reads as "copy is broken".
/// - The scroll wheel is also a mouse report, so scrolling does not move the
///   terminal's own scrollback — it round-trips to the far side and waits for a
///   full redraw to come back. That is the scroll lag.
/// - With any-event tracking, every mouse *movement* is another round trip.
///
/// Measured against a real host: forcing fullscreen emits `?1049h`, `?1000h`,
/// `?1002h`, `?1003h` and `?1006h`; with the variables below, none of them.
/// They override an explicit fullscreen request *and* the saved `tui` preference,
/// which is what makes this reliable — `/tui fullscreen` persists, so a user who
/// ever ran it would otherwise carry the problem into every future session.
///
/// Inline costs fullscreen's in-TUI mouse scrolling, its flat memory use and
/// `/focus`. Everything else — every slash command, vim mode, every picker —
/// is identical, because it is the same interactive CLI either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rendering {
    /// Appended to the terminal's own scrollback. rmux's default.
    #[default]
    Inline,
    /// Alternate screen with mouse capture. Only if the operator asks for it.
    Fullscreen,
}

impl Rendering {
    /// Environment assignments to prefix onto the shell line, ready to concatenate.
    ///
    /// A prefix on the command line rather than `CommandSpec::env`, because under
    /// the agent the shell is spawned by the **daemon** — a long-lived process
    /// with its own environment — so env attached to the attach command would
    /// never reach Claude. These values are not secret; anything that is must go
    /// through the agent's `Hello` instead (see `rmux-agent`), because
    /// `spec_to_shell_line` renders env into a command line that `ps` exposes.
    pub fn env_prefix(self) -> &'static str {
        match self {
            Rendering::Inline => {
                "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 CLAUDE_CODE_DISABLE_MOUSE=1 "
            }
            // Nothing: let Claude use whatever it is configured for.
            Rendering::Fullscreen => "",
        }
    }
}

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
        let launch = Self::launch_line(resume, args, Rendering::default());

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

    /// The shell line rmux runs to launch Claude.
    ///
    /// Exposed so the caller can hand it to the agent instead of running it
    /// directly — which is what makes a conversation keep working after rmux is
    /// closed.
    pub fn launch_line(resume: Option<&str>, args: &[String], rendering: Rendering) -> String {
        let mut launch = String::from(rendering.env_prefix());
        launch.push_str("claude");
        if let Some(id) = resume {
            launch.push_str(" --resume ");
            launch.push_str(&rmux_transport::shell_quote(id));
        }
        for arg in args {
            launch.push(' ');
            launch.push_str(&rmux_transport::shell_quote(arg));
        }
        launch
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

    /// Answer a prompt by pressing its digit, confirming only if it is needed.
    ///
    /// `fingerprint` names the prompt being answered. If the screen has moved on,
    /// the answer is **refused** rather than delivered to whatever replaced it —
    /// the guard against answering a question that is no longer being asked,
    /// which in the old design silently confirmed the wrong thing.
    ///
    /// ## Why the digit is not always enough
    ///
    /// Claude draws (at least) two kinds of dialog and they commit differently.
    /// A permission prompt commits on the digit. The question dialog does not —
    /// it prints `Enter to select · ↑↓ to navigate` along its bottom, and a
    /// digit there moves the highlight at most. Sending the digit alone made
    /// that card **completely inert**: reported as "I can't click the first
    /// answer and it keeps showing the question again", with nothing on screen
    /// to suggest the keystroke had been delivered and ignored.
    ///
    /// ## Why this reads the screen back instead of just adding Enter
    ///
    /// Appending Enter unconditionally is the obvious fix and it is unsafe. If a
    /// digit turns out to be a no-op in some dialog, Enter commits whatever
    /// happens to be highlighted — so clicking option 3 would answer option 1.
    /// Answering a question with the wrong option is far worse than failing to
    /// answer it, because nothing afterwards reveals that it happened.
    ///
    /// So the digit is sent, the screen is read back, and Enter follows **only
    /// once the caret is observed on the option that was asked for**. Every case
    /// is then safe:
    ///
    ///  - the dialog committed on the digit → the prompt is gone, no Enter is
    ///    sent, and nothing stray reaches the composer;
    ///  - the digit moved the caret → Enter confirms the option the operator
    ///    clicked;
    ///  - the digit did nothing → the caret is still elsewhere, Enter is **not**
    ///    sent, and this reports that the option could not be selected.
    ///
    /// Rule 1 in `keys` still holds: the *selection* is made by the digit, never
    /// by counting arrow presses against a highlight that moves on its own.
    pub async fn answer(&self, fingerprint: &str, key: &str) -> anyhow::Result<()> {
        let current = self.state();
        let prompt = current.prompt.ok_or_else(|| anyhow::anyhow!("nothing is being asked"))?;

        anyhow::ensure!(prompt.fingerprint == fingerprint, "that question is no longer on screen");
        anyhow::ensure!(
            prompt.choices.iter().any(|c| c.key == key),
            "option {key} is not one of the choices"
        );

        self.terminal.write(&keys::choose(key))?;

        // Long enough for a TUI repaint over SSH, short enough that a dialog
        // which already committed does not leave the operator waiting.
        for _ in 0..CONFIRM_POLLS {
            tokio::time::sleep(CONFIRM_POLL_EVERY).await;

            let Some(now) = self.state().prompt else {
                // Committed on the digit. Nothing further to send.
                return Ok(());
            };

            if now.choices.iter().any(|c| c.key == key && c.selected) {
                return self.terminal.write(keys::CONFIRM);
            }
        }

        anyhow::bail!("option {key} could not be selected — press it in the terminal instead")
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
    fn inline_disables_the_alternate_screen_and_the_mouse() {
        // Verified against a real host: with these two set, Claude emits none of
        // ?1049h / ?1000h / ?1002h / ?1003h / ?1006h. Without them it emits all
        // five, and selection, copy and scrolling all break.
        let line = ClaudeSession::launch_line(None, &[], Rendering::Inline);
        assert!(line.contains("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1"), "{line}");
        assert!(line.contains("CLAUDE_CODE_DISABLE_MOUSE=1"), "{line}");
        // The assignments must precede the program, or the shell treats them as
        // arguments to `claude` rather than as environment.
        assert!(
            line.find("CLAUDE_CODE_DISABLE_MOUSE=1") < line.find("claude"),
            "environment must come before the program: {line}"
        );
    }

    #[test]
    fn fullscreen_sets_nothing_and_leaves_claude_alone() {
        let line = ClaudeSession::launch_line(None, &[], Rendering::Fullscreen);
        assert!(!line.contains("CLAUDE_CODE"), "{line}");
        assert!(line.starts_with("claude"), "{line}");
    }

    #[test]
    fn inline_is_the_default() {
        // The whole point: a user who once ran `/tui fullscreen` carries that
        // preference forever, so rmux has to opt out on every launch.
        assert_eq!(Rendering::default(), Rendering::Inline);
    }

    #[test]
    fn the_resume_id_is_still_quoted() {
        // It reaches a shell, so it is an injection risk regardless of what else
        // the line now carries.
        let line = ClaudeSession::launch_line(Some("a b; rm -rf /"), &[], Rendering::Inline);
        assert!(line.contains("--resume"), "{line}");
        // Contained *within quotes*, so the shell reads it as one word. Checking
        // for the absence of the substring would be wrong — the quoted form still
        // contains it, which is exactly what a correct line looks like.
        assert!(line.ends_with("--resume 'a b; rm -rf /'"), "unquoted resume id: {line}");
    }

    #[test]
    fn extra_arguments_survive_the_prefix() {
        let line =
            ClaudeSession::launch_line(None, &["--model".into(), "opus".into()], Rendering::Inline);
        assert!(line.ends_with("claude --model opus"), "{line}");
    }

    #[test]
    fn resuming_unsupervised_carries_both_the_conversation_and_the_flag() {
        // The two are chosen together on the resume screen, and they travel on
        // the same line — so a change to either must not drop the other. The
        // failure this pins is silent in the worst way: a session the operator
        // launched with permission checks off would come back *with* them, or,
        // far worse, the other way round.
        let line = ClaudeSession::launch_line(
            Some("f00-baa"),
            &["--dangerously-skip-permissions".into()],
            Rendering::Inline,
        );
        // Unquoted, because `shell_quote` only quotes what needs it and a real
        // conversation id is safe characters throughout. `the_resume_id_is_still_quoted`
        // covers the one that does need it.
        assert!(line.contains("--resume f00-baa"), "{line}");
        assert!(line.contains("--dangerously-skip-permissions"), "{line}");
        // And the rendering prefix still leads, or Claude comes back fullscreen
        // with the mouse captured.
        assert!(line.starts_with("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1"), "{line}");
    }

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
    async fn answering_a_stale_prompt_is_refused() {
        // The core guard. Answering a question that has left the screen is how
        // the previous design silently confirmed the wrong thing.
        let session = fake_claude(DIALOG_SCRIPT, TermSize { cols: 60, rows: 12 });

        eventually(|| session.state().prompt.is_some(), std::time::Duration::from_secs(5)).await;

        let err = session
            .answer("a-fingerprint-from-some-older-screen", "1")
            .await
            .expect_err("a stale answer must be refused");
        assert!(err.to_string().contains("no longer on screen"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_option_that_is_not_offered_is_refused() {
        let session = fake_claude(DIALOG_SCRIPT, TermSize { cols: 60, rows: 12 });
        eventually(|| session.state().prompt.is_some(), std::time::Duration::from_secs(5)).await;

        let fingerprint = session.state().prompt.unwrap().fingerprint;
        let err = session.answer(&fingerprint, "9").await.expect_err("option 9 does not exist");
        assert!(err.to_string().contains("not one of the choices"), "got: {err}");
    }

    /// A dialog that **commits on the digit** — Claude's permission prompt.
    ///
    /// Reads one raw byte and replaces the screen, so the prompt is gone by the
    /// time `answer` looks again. `stty -icanon` because a canonical-mode read
    /// blocks for a whole line, which is precisely the behaviour under test.
    const COMMITS_ON_DIGIT: &str = r#"
stty -icanon -echo min 1 time 0 2>/dev/null
printf 'Do you want to make this edit to a.txt?\r\n'
printf '\r\n'
printf ' \xe2\x9d\xaf 1. Yes\r\n'
printf '   2. No\r\n'
a=$(dd bs=1 count=1 2>/dev/null)
printf '\033[2J\033[H'
printf 'ANSWERED:%s\r\n' "$a"
sleep 2
"#;

    /// A dialog that needs **Enter to select** — the question card, which prints
    /// `Enter to select · ↑↓ to navigate` and treats a digit as navigation only.
    ///
    /// The digit moves the caret; the screen is redrawn with it on the chosen
    /// option; nothing is committed until a carriage return arrives.
    const NEEDS_ENTER: &str = r#"
stty -icanon -echo min 1 time 0 2>/dev/null
printf 'Which should I build next?\r\n'
printf '\r\n'
printf ' \xe2\x9d\xaf 1. One\r\n'
printf '   2. Two\r\n'
a=$(dd bs=1 count=1 2>/dev/null)
printf '\033[2J\033[H'
printf 'Which should I build next?\r\n'
printf '\r\n'
printf '   1. One\r\n'
printf ' \xe2\x9d\xaf 2. Two\r\n'
dd bs=1 count=1 >/dev/null 2>&1
printf '\033[2J\033[H'
printf 'ANSWERED:%s\r\n' "$a"
sleep 2
"#;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_dialog_that_commits_on_the_digit_gets_no_stray_enter() {
        let session = fake_claude(COMMITS_ON_DIGIT, TermSize { cols: 60, rows: 12 });
        eventually(|| session.state().prompt.is_some(), std::time::Duration::from_secs(5)).await;

        let fingerprint = session.state().prompt.unwrap().fingerprint;
        session.answer(&fingerprint, "2").await.expect("the digit alone answers this one");

        let delivered = eventually(
            || String::from_utf8_lossy(&session.terminal.replay()).contains("ANSWERED:2"),
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(delivered, "screen: {:?}", session.screen.lock().lines());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_dialog_that_needs_enter_is_confirmed() {
        // The reported bug: the digit alone left this card completely inert, so
        // clicking an option did nothing and the question kept coming back.
        let session = fake_claude(NEEDS_ENTER, TermSize { cols: 60, rows: 12 });
        eventually(|| session.state().prompt.is_some(), std::time::Duration::from_secs(5)).await;

        let fingerprint = session.state().prompt.unwrap().fingerprint;
        session.answer(&fingerprint, "2").await.expect("the caret lands on 2, so Enter follows");

        let delivered = eventually(
            || String::from_utf8_lossy(&session.terminal.replay()).contains("ANSWERED:2"),
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(delivered, "screen: {:?}", session.screen.lock().lines());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_ignored_digit_never_becomes_a_wrong_answer() {
        // **The safety property.** If a digit does nothing, sending Enter anyway
        // would commit whatever is highlighted — clicking option 2 would answer
        // option 1, and nothing afterwards would reveal it. Failing to answer is
        // recoverable; answering the wrong thing is not.
        //
        // This fake never moves its caret, so the caret stays on option 1.
        let session = fake_claude(DIALOG_SCRIPT, TermSize { cols: 60, rows: 12 });
        eventually(|| session.state().prompt.is_some(), std::time::Duration::from_secs(5)).await;

        let fingerprint = session.state().prompt.unwrap().fingerprint;
        let err = session
            .answer(&fingerprint, "2")
            .await
            .expect_err("option 2 was never highlighted, so it must not be confirmed");
        assert!(err.to_string().contains("could not be selected"), "got: {err}");

        // And no Enter reached the process — its `read` is still waiting, so it
        // never printed an answer. This is the assertion that would fail if the
        // confirm were sent on faith.
        assert!(
            !String::from_utf8_lossy(&session.terminal.replay()).contains("ANSWERED:"),
            "nothing may have been answered; screen: {:?}",
            session.screen.lock().lines()
        );
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
