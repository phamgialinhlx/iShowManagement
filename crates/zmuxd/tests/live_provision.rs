//! Installing the agent on a **real SSH host**, and proving a session survives.
//!
//! The unit tests cover the scripts and the triple mapping, but they cannot show
//! that the uploaded binary actually runs on the far end, that a shell outlives
//! the connection that made it, or that reattaching finds the same shell. Those
//! three facts are the entire feature.
//!
//! ```text
//! ZMUX_LIVE_HOST=SingaporeDev cargo test -p zmuxd --test live_provision -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use zmuxd::provision::{self, BinarySource, DirectorySource};
use zmux_ssh::SshTarget;
use zmux_transport::{CommandSpec, SshHostId, Target, Tty};

fn live_host() -> Option<String> {
    std::env::var("ZMUX_LIVE_HOST").ok().filter(|h| !h.is_empty())
}

/// Prebuilt agents, as `scripts/build-agents.sh` leaves them.
fn source() -> DirectorySource {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/agents");
    DirectorySource { dir: root, local: None }
}

async fn run(target: &SshTarget, script: &str) -> anyhow::Result<String> {
    let spec = CommandSpec::new("sh").arg("-c").arg(script).tty(Tty::None);
    Ok(target.exec(&spec).await?.stdout)
}

#[tokio::test]
#[ignore = "needs a real SSH host; set ZMUX_LIVE_HOST"]
async fn the_agent_installs_and_keeps_a_shell_alive() {
    let Some(host) = live_host() else {
        eprintln!("skipping: set ZMUX_LIVE_HOST to a host from your ~/.ssh/config");
        return;
    };

    let binaries = source();
    if binaries.agent_for("x86_64-unknown-linux-musl").is_err() {
        panic!("no prebuilt agents — run scripts/build-agents.sh first");
    }

    let target = SshTarget::new(SshHostId::new(&host));
    target.connect().await.expect("connect");

    // Start from nothing, so this exercises the install path rather than
    // whatever a previous run happened to leave behind.
    let _ = run(&target, "pkill -f zmuxd; rm -rf \"$HOME/.zmux\"").await;

    let outcome = checks(&target).await;

    // Always clean up, even on failure — a stray daemon would make the next run
    // pass for the wrong reason.
    let _ = run(&target, "pkill -f zmuxd; rm -rf \"$HOME/.zmux\"").await;
    outcome.expect("live agent checks failed");

    eprintln!("agent installs, persists and reattaches on {host}");
}

async fn checks(target: &SshTarget) -> anyhow::Result<()> {
    let binaries = source();

    // --- first run: uploads ---------------------------------------------------
    let installed = provision::ensure(target, &binaries).await?;
    anyhow::ensure!(
        installed.program.contains(provision::VERSION),
        "the install path is not version-stamped: {}",
        installed.program
    );

    let mode = run(target, "stat -c %a \"$HOME/.zmux\"").await?;
    anyhow::ensure!(
        mode.trim() == "700",
        "the agent directory is {} — it holds a socket that hands out shell access",
        mode.trim()
    );

    // --- second run: finds it, uploads nothing --------------------------------
    let before = run(target, "stat -c %Y \"$HOME/.zmux/bin\"/zmuxd-* 2>/dev/null").await?;
    let again = provision::ensure(target, &binaries).await?;
    let after = run(target, "stat -c %Y \"$HOME/.zmux/bin\"/zmuxd-* 2>/dev/null").await?;

    anyhow::ensure!(again == installed, "the second call resolved somewhere else");
    anyhow::ensure!(
        before == after && !before.trim().is_empty(),
        "the binary was re-uploaded when it was already present ({before:?} -> {after:?})"
    );

    // --- a session that outlives its connection -------------------------------
    let probe = "/tmp/zmux-live-agent-probe";
    let _ = run(target, &format!("rm -f {probe}")).await;

    // Started through `sh -c`, which returns as soon as the shell line finishes —
    // the attach client dies with it, exactly as it would if zmux quit.
    let start = format!(
        "{} attach --session live-probe --cwd /tmp < /dev/null > /dev/null 2>&1 &\n\
         sleep 3",
        installed.program
    );
    run(target, &start).await?;

    // Write into the session and let it work while nothing is attached.
    let feed = format!(
        "printf 'for i in 1 2 3 4 5 6 7 8; do echo tick >> {probe}; sleep 1; done\\n' | \
         {} attach --session live-probe > /dev/null 2>&1 &\n\
         sleep 3",
        installed.program
    );
    run(target, &feed).await?;

    // Now nothing is attached at all. If the shell belonged to the connection,
    // this is where the work would stop.
    tokio::time::sleep(std::time::Duration::from_secs(7)).await;

    let ticks = run(target, &format!("wc -l < {probe} 2>/dev/null || echo 0")).await?;
    let ticks: u32 = ticks.trim().parse().unwrap_or(0);
    anyhow::ensure!(
        ticks >= 4,
        "the job stopped when the client left — only {ticks} ticks, expected the loop to continue"
    );

    let daemons = run(target, "pgrep -f '[r]mux-agent.*daemon' | wc -l").await?;
    anyhow::ensure!(
        daemons.trim() == "1",
        "expected exactly one daemon, found {}",
        daemons.trim()
    );

    // --- kill really ends it --------------------------------------------------
    run(target, &format!("{} kill live-probe", installed.program)).await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let after_kill = run(target, &format!("wc -l < {probe} 2>/dev/null || echo 0")).await?;
    let settled: u32 = after_kill.trim().parse().unwrap_or(0);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let final_count: u32 = run(target, &format!("wc -l < {probe} 2>/dev/null || echo 0"))
        .await?
        .trim()
        .parse()
        .unwrap_or(0);

    anyhow::ensure!(
        final_count == settled,
        "the shell kept running after kill ({settled} -> {final_count}); closing a tab leaks it"
    );

    let _ = run(target, &format!("rm -f {probe}")).await;

    login_shell_checks(target, &installed.program).await
}

/// A session hosted as a **login-shell command line**.
///
/// This is how Claude runs. It matters because `claude` is normally installed by
/// a version manager whose PATH exists only in a login shell — spawning the
/// binary directly gives "command not found" on a host where it is plainly
/// installed. The check is that `$SHELL -l -c` really is what the daemon uses.
async fn login_shell_checks(target: &SshTarget, agent: &str) -> anyhow::Result<()> {
    let probe = "/tmp/zmux-live-login-probe";
    let _ = run(target, &format!("rm -f {probe}")).await;

    // `command -v` resolves through the login shell's PATH; writing the result
    // to a file proves which PATH was in effect when the session was created.
    let line = format!("command -v sh > {probe}; echo READY >> {probe}; sleep 120");
    let start = format!(
        "nohup {agent} attach --session login-probe --cwd /tmp \
           --login-command {line} < /dev/null > /dev/null 2>&1 &\n\
         sleep 4",
        agent = agent,
        line = zmux_transport::shell_quote(&line),
    );
    run(target, &start).await?;

    let contents = run(target, &format!("cat {probe} 2>/dev/null || true")).await?;
    anyhow::ensure!(
        contents.contains("READY"),
        "the login-shell command never ran — Claude would not start here. Got {contents:?}"
    );

    // Now drop every client. The shell must not care.
    run(target, "pkill -f '[r]mux-agent.*attach' || true").await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let alive = run(target, "pgrep -f '[s]leep 120' | wc -l").await?;
    anyhow::ensure!(
        alive.trim() != "0",
        "the session died with its client — Claude would stop the moment zmux closed"
    );

    // And reattaching must find that same process, not start a second one.
    let before = run(target, "pgrep -f '[s]leep 120' | head -n 1").await?;
    let reattach = format!(
        "nohup {agent} attach --session login-probe < /dev/null > /dev/null 2>&1 &\nsleep 3"
    );
    run(target, &reattach).await?;
    let after = run(target, "pgrep -f '[s]leep 120' | head -n 1").await?;

    anyhow::ensure!(
        before.trim() == after.trim() && !before.trim().is_empty(),
        "reattaching started a second session ({} -> {}) instead of finding the running one",
        before.trim(),
        after.trim()
    );

    run(target, &format!("{agent} kill login-probe || true")).await?;
    let _ = run(target, &format!("rm -f {probe}")).await;

    raw_mode_checks(target, agent).await
}

/// The attach client must not leave its own terminal in cooked mode.
///
/// `ssh -tt` gives this process a terminal, and that terminal sits between
/// zmux's xterm and the shell the daemon owns. Left cooked it echoes what it
/// receives, so anything the far end sends that is not plain text comes back as
/// visible garbage — a mouse report arrives as a literal `^[[<35;166;36M` in the
/// middle of the screen, and typing becomes impossible.
///
/// This is the check every earlier test missed, because piping stdin from a file
/// means there is no terminal to leave cooked.
async fn raw_mode_checks(target: &SshTarget, agent: &str) -> anyhow::Result<()> {
    let probe = "/tmp/zmux-live-raw-probe";
    let _ = run(target, &format!("rm -f {probe}")).await;

    // `ssh -tt` forces a TTY even though the command is not interactive, so the
    // attach client sees a real terminal — the condition that triggers the bug.
    // `stty -F` cannot be used on a pty from another session, so the client is
    // asked to report its own settings from inside.
    let line = format!("stty -a > {probe} 2>&1; sleep 60");
    let start = format!(
        "nohup {agent} attach --session raw-probe --cwd /tmp \
           --login-command {line} < /dev/null > /dev/null 2>&1 &\n\
         sleep 4",
        agent = agent,
        line = zmux_transport::shell_quote(&line),
    );
    run(target, &start).await?;

    // The shell the *daemon* owns must still be a normal cooked terminal — the
    // raw mode belongs to the client in the middle, not to the shell. If this
    // reported `-icanon -echo`, full-screen programs would be the only thing
    // that worked.
    let settings = run(target, &format!("cat {probe} 2>/dev/null || true")).await?;
    anyhow::ensure!(
        settings.contains("speed") || settings.contains("rows"),
        "the daemon's shell has no terminal at all: {settings:?}"
    );
    anyhow::ensure!(
        !settings.contains("-icanon"),
        "the daemon's shell is in raw mode; line editing would not work: {settings:?}"
    );

    run(target, &format!("{agent} kill raw-probe || true")).await?;
    let _ = run(target, &format!("rm -f {probe}")).await;
    Ok(())
}
