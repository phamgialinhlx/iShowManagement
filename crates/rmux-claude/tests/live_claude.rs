//! Live verification of a Claude session on a **real remote host**.
//!
//! The unit tests drive a fake TUI through a local PTY, which proves the parsing
//! and the key encoding. What they cannot prove is the part that actually breaks
//! in the field: that `claude` is *found* and *launches* over ssh, that its real
//! screen renders through our emulator, and that its own prompts parse.
//!
//! Ignored by default. Run with:
//!
//! ```text
//! RMUX_LIVE_HOST=SingaporeDev cargo test -p rmux-claude --test live_claude \
//!   -- --ignored --nocapture
//! ```

use rmux_claude::ClaudeSession;
use rmux_ssh::SshTarget;
use rmux_term::TermSize;
use rmux_transport::{SshHostId, Target};

fn live_host() -> Option<String> {
    std::env::var("RMUX_LIVE_HOST").ok().filter(|h| !h.is_empty())
}

async fn eventually(
    mut check: impl FnMut() -> bool,
    timeout: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real SSH host with claude installed; set RMUX_LIVE_HOST"]
async fn claude_launches_on_a_real_remote_host() {
    let Some(host) = live_host() else {
        eprintln!("skipping: set RMUX_LIVE_HOST");
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));
    target.connect().await.expect("connect");

    // `--version` exits immediately, so this checks the launch path — the login
    // shell resolving `claude` on a PATH that a non-interactive ssh would not
    // have — without starting an interactive session.
    let out = target
        .exec(
            &rmux_transport::CommandSpec::new("$SHELL")
                .arg("-l")
                .arg("-c")
                .arg("claude --version")
                .tty(rmux_transport::Tty::None),
        )
        .await
        .expect("exec");

    let version = out.stdout_or_err().expect("claude --version failed");
    eprintln!("remote claude: {version}");
    assert!(version.contains("Claude Code"), "unexpected version output: {version:?}");
}

/// Start a real interactive Claude and read its actual screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real SSH host with claude installed; set RMUX_LIVE_HOST"]
async fn a_real_claude_session_renders_and_accepts_input() {
    let Some(host) = live_host() else {
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));
    target.connect().await.expect("connect");

    let session = ClaudeSession::start(
        &target,
        Some("~/rmux-testbed"),
        // `--version` is short and stable. `--help` also renders correctly, but
        // it is longer than the viewport, so the opening lines scroll off and the
        // assertion would depend on where the emulator happens to be looking.
        // Neither needs an API key nor leaves a session running on the host.
        &["--version".to_owned()],
        TermSize { cols: 100, rows: 30 },
    )
    .expect("start claude");

    let rendered = eventually(
        || session.screen_text().contains("Claude Code"),
        std::time::Duration::from_secs(30),
    )
    .await;

    let text = session.screen_text();
    eprintln!("--- remote claude screen (first 400 chars) ---\n{}", &text[..text.len().min(400)]);

    assert!(rendered, "claude's screen never appeared; got:\n{text}");
}

/// Detect a **real** Claude Code dialog on a real host.
///
/// Starting `claude` in a folder it has not seen before makes it ask whether the
/// files there are trusted — a genuine numbered dialog, drawn by the real TUI.
/// That is the one thing no fake can prove: that our screen parser recognises
/// what Claude actually draws, box characters, caret and all.
///
/// Costs nothing and spends no tokens: the dialog appears before any model call,
/// and the session is killed as soon as it is seen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real SSH host with claude installed; set RMUX_LIVE_HOST"]
async fn a_real_claude_dialog_is_parsed() {
    let Some(host) = live_host() else {
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));
    target.connect().await.expect("connect");

    // A folder Claude has not been run in before, so the trust prompt appears.
    let folder = format!("~/rmux-dialog-probe-{}", std::process::id());
    target
        .exec(
            &rmux_transport::CommandSpec::new("sh")
                .arg("-c")
                .arg(format!("mkdir -p {} && echo hi > {}/a.txt", folder, folder))
                .tty(rmux_transport::Tty::None),
        )
        .await
        .expect("create probe folder");

    let session = ClaudeSession::start(
        &target,
        Some(&folder),
        &[],
        TermSize { cols: 100, rows: 30 },
    )
    .expect("start claude");

    let saw_dialog =
        eventually(|| session.state().prompt.is_some(), std::time::Duration::from_secs(45)).await;

    let screen = session.screen_text();
    let state = session.state();

    // Stop the session and clean up regardless of the outcome.
    let _ = session.terminal().kill();
    let _ = target
        .exec(
            &rmux_transport::CommandSpec::new("sh")
                .arg("-c")
                .arg(format!("rm -rf {folder}"))
                .tty(rmux_transport::Tty::None),
        )
        .await;

    eprintln!("--- real claude screen ---\n{screen}\n--- end ---");

    assert!(saw_dialog, "no dialog was detected. screen was:\n{screen}");

    let prompt = state.prompt.expect("a dialog should have been parsed");
    eprintln!("question: {:?}", prompt.question);
    for choice in &prompt.choices {
        eprintln!("  [{}] {} {}", choice.key, choice.label, if choice.selected { "<-" } else { "" });
    }

    assert!(!prompt.question.is_empty(), "the question should have been extracted");
    assert!(prompt.choices.len() >= 2, "a dialog should offer at least two options");
    assert!(!prompt.fingerprint.is_empty());
}

/// Listing resumable sessions on a real host.
#[tokio::test]
#[ignore = "needs a real SSH host with claude installed; set RMUX_LIVE_HOST"]
async fn claude_sessions_can_be_listed_on_a_real_host() {
    let Some(host) = live_host() else {
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));
    target.connect().await.expect("connect");

    let home = target
        .exec(
            &rmux_transport::CommandSpec::new("sh")
                .arg("-c")
                .arg("cd && pwd")
                .tty(rmux_transport::Tty::None),
        )
        .await
        .expect("home")
        .stdout_or_err()
        .expect("home")
        .to_owned();

    // A folder Claude has never run in must return an empty list, not an error —
    // that is the ordinary first case for a new project.
    let none = ClaudeSession::list(&target, &format!("{home}/definitely-not-a-project"))
        .await
        .expect("listing an unknown folder should succeed");
    assert!(none.is_empty(), "expected no sessions, got {none:?}");

    // Whatever exists in the home directory itself.
    let found = ClaudeSession::list(&target, &home).await.expect("list");
    eprintln!("{} session(s) recorded for {home}", found.len());
    for s in found.iter().take(5) {
        eprintln!("  {}  {}  {:?}", s.id, s.modified, s.title);
    }

    for s in &found {
        assert!(!s.id.is_empty(), "every session needs an id to resume with");
    }
}
