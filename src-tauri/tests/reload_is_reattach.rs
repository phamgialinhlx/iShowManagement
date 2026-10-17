//! **RELOAD must re-attach. It must never end the session.**
//!
//! Every pane header carries a RELOAD control, because a stalled `ssh` leaves a
//! pane that looks alive and is not: drop detection keys on the attach process
//! *exiting*, and the keepalive gives that 90 seconds
//! (`ServerAliveInterval=15` x `ServerAliveCountMax=6`). Until then nothing
//! arrives and typing goes nowhere, and the operator's only previous recovery was
//! closing the pane.
//!
//! Closing is the thing this must not become. A shell and its agent run under
//! `rmux-agent` on the target *precisely so* they survive a view going away, and
//! the two commands that end that work are `terminal_close` (which carries an
//! `agent kill`) and `claude_end_session`. A reload reaching either destroys
//! exactly the long-running work it was pressed to rescue — silently, on a host,
//! and a day's work at a time. There is no runtime check that could catch it
//! without killing a real session to find out, so the check is a read of the code.
//!
//! It lives in Rust rather than in a `ui/*-check.ts` harness for one reason: this
//! runs. A browser harness is opened by hand at `localhost:5273`, and the
//! assertion that matters most must not depend on somebody remembering to look.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("src-tauri has a parent").to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every file the reload path runs through.
///
/// `ClaudePanel` holds `reconnect` and the header button for **both** agent panes
/// — a Claude pane and a pi pane are the same panel with a different provider, so
/// one copy is also what stops the two drifting. `WorkspaceDeck` holds the shell
/// pane's, and `Terminal` holds the rebuild it drives.
const RELOAD_PATH_FILES: &[&str] = &[
    "ui/src/components/ClaudePanel.tsx",
    "ui/src/components/Terminal.tsx",
    "ui/src/components/WorkspaceDeck.tsx",
    "ui/src/components/SessionPaneChrome.tsx",
];

/// Commands that end work on the target. Neither may be reachable from a reload.
const DESTRUCTIVE: &[&str] = &["terminal_close", "claude_end_session", "claudeEndSession"];

#[test]
fn reload_never_ends_a_session() {
    for file in RELOAD_PATH_FILES {
        let source = read(file);
        for (line_no, line) in source.lines().enumerate() {
            // A comment may *name* the forbidden command — the reasoning above is
            // the reason these files are worth reading — so only code counts.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with('*') || code.starts_with("/*") {
                continue;
            }
            for bad in DESTRUCTIVE {
                assert!(
                    !code.contains(bad),
                    "{file}:{} reaches `{bad}`, which ends the work on the target. \
                     A reload must re-attach: `terminal_detach` / `claude_stop` drop the \
                     local view only.\n  {code}",
                    line_no + 1,
                );
            }
        }
    }
}

#[test]
fn the_reload_control_exists_in_every_pane_header() {
    let chrome = read("ui/src/components/SessionPaneChrome.tsx");
    assert!(chrome.contains("export const RELOAD_PATH"), "the RELOAD icon is defined once, here");
    assert!(
        chrome.contains("disabled?: boolean"),
        "IconButton must be able to report an in-flight reattach beside itself",
    );

    // The agent panes: one header, rendered by `ClaudePanel` for both providers.
    let panel = read("ui/src/components/ClaudePanel.tsx");
    assert!(panel.contains("RELOAD_PATH"), "the agent panes' header has no RELOAD");
    for pane in ["ClaudeSessionPane", "PiSessionPane"] {
        let source = read(&format!("ui/src/components/{pane}.tsx"));
        assert!(
            source.contains("<ClaudePanel"),
            "{pane} must take its header from ClaudePanel, or it has no RELOAD",
        );
    }

    // The shell pane.
    let deck = read("ui/src/components/WorkspaceDeck.tsx");
    assert!(deck.contains("RELOAD_PATH"), "the shell pane's header has no RELOAD");
    assert!(
        deck.contains("generation={generation}"),
        "the shell pane's RELOAD must drive TerminalView's rebuild",
    );
    assert!(
        read("ui/src/components/Terminal.tsx").contains("}, [generation]);"),
        "TerminalView's setup effect must depend on `generation`, or a bump rebuilds nothing",
    );
}

#[test]
fn reload_lets_go_of_the_local_view_only() {
    let panel = read("ui/src/components/ClaudePanel.tsx");
    assert!(
        panel.contains("invoke(\"claude_stop\""),
        "the agent panes must drop the stalled local attach client (`claude_stop`), \
         so a wedged pane pressed twice does not leave two ssh children",
    );
    assert!(
        panel.contains("clearClaudeSession(sessionId)"),
        "the stale handle names an attachment inside the previous connection; \
         reattaching to it fails forever",
    );

    let deck = read("ui/src/components/WorkspaceDeck.tsx");
    assert!(
        deck.contains("invoke(\"terminal_detach\""),
        "the shell pane must detach — the command that takes neither a target nor a \
         session name, and so has no path that can end a shell",
    );
    assert!(
        deck.contains("clearLive(session.id)"),
        "the stale PTY handle must be cleared, or the next attach asks for a dead one",
    );
}

#[test]
fn a_reattach_uses_the_name_the_daemon_already_holds() {
    // Attaching to a name the daemon does not hold **creates** a session, so a
    // mis-derived name does not fail — it starts a second shell beside the one
    // being adopted and leaves the original running with nothing pointing at it.
    // `reattachName` is the one derivation that recovers an adopted session's own
    // name, and it is decided in one place rather than at each call site.
    for pane in ["ClaudeSessionPane", "PiSessionPane", "WorkspaceDeck"] {
        let source = read(&format!("ui/src/components/{pane}.tsx"));
        assert!(
            source.contains("reattachName(session)"),
            "{pane} must name its session with `reattachName`, or a reload can start a \
             second one beside the work it was meant to rescue",
        );
    }
}
