//! Persistent sessions on a **real Windows host**.
//!
//! This is the claim that matters: a shell started through rmux on Windows keeps
//! running after the SSH connection that made it is gone, and reattaching finds
//! the same shell rather than a new one. Nothing short of a real host can show
//! it — the unit tests pin the triple mapping and the `.exe` naming, but neither
//! proves that a cross-compiled Win32 binary launches, that ConPTY gives it a
//! working pty, or that a named pipe survives the client dying.
//!
//! ```text
//! RMUX_LIVE_WINDOWS=ytai-win cargo test -p rmux-agent --test live_windows_agent -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use rmux_agent::provision::{self, DirectorySource};
use rmux_ssh::SshTarget;
use rmux_transport::{CommandSpec, Platform, SshHostId, Target, Tty};

fn live_host() -> Option<String> {
    std::env::var("RMUX_LIVE_WINDOWS").ok().filter(|h| !h.is_empty())
}

fn source() -> DirectorySource {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/agents");
    DirectorySource { dir: root, local: None }
}

async fn run(target: &SshTarget, script: &str) -> anyhow::Result<String> {
    let spec = CommandSpec::new("sh").arg("-c").arg(script).tty(Tty::None);
    Ok(target.exec(&spec).await?.stdout)
}

#[tokio::test]
#[ignore = "needs a real Windows host; set RMUX_LIVE_WINDOWS. \
            Currently red at the last step — see the note above the tick assertion."]
async fn a_windows_session_outlives_its_connection() {
    let Some(host) = live_host() else {
        eprintln!("skipping: set RMUX_LIVE_WINDOWS to a Windows host alias");
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));
    let platform = target.connect().await.expect("connect");
    assert_eq!(platform, Platform::Windows, "expected a Windows host");

    // Start clean, so this exercises the install rather than whatever a previous
    // run left behind. `taskkill`, not `pkill` — Git Bash ships no pkill.
    let _ = run(&target, "for p in \"$HOME\"/.rmux/bin/*.exe; do taskkill //F //IM \"$(basename \"$p\")\" >/dev/null 2>&1; done; rm -rf \"$HOME/.rmux\"").await;

    let outcome = checks(&target).await;

    let _ = run(&target, "for p in \"$HOME\"/.rmux/bin/*.exe; do taskkill //F //IM \"$(basename \"$p\")\" >/dev/null 2>&1; done; rm -rf \"$HOME/.rmux\"").await;
    outcome.expect("windows agent checks failed");

    eprintln!("the agent installs, persists and reattaches on {host}");
}

async fn checks(target: &SshTarget) -> anyhow::Result<()> {
    let binaries = source();

    // --- it installs, as an executable Windows can actually run ---------------
    let installed = provision::ensure(target, &binaries).await?;
    anyhow::ensure!(
        installed.program.ends_with(".exe"),
        "Windows will not execute a file without `.exe`: {}",
        installed.program
    );
    eprintln!("installed: {}", installed.program);

    // It has to *run*, not merely exist. A cross-compiled binary that will not
    // start looks identical to one that was never uploaded.
    let version = run(target, &format!("'{}' version", installed.program)).await?;
    anyhow::ensure!(
        version.trim() == provision::VERSION,
        "the uploaded agent did not run: {version:?}"
    );

    // --- second run finds it rather than re-uploading -------------------------
    let again = provision::ensure(target, &binaries).await?;
    anyhow::ensure!(again == installed, "the second call resolved somewhere else");

    // --- a session that outlives its connection -------------------------------
    //
    // **The client is killed, not abandoned.** On Unix a backgrounded attach
    // reading from `/dev/null` sees EOF and exits; a native Win32 process handed
    // MSYS's `/dev/null` blocks on it forever, so the SSH channel never closes
    // and the whole thing looks hung — on a daemon that is running perfectly.
    // Killing the client is also a better model of what rmux does: quitting the
    // app kills the far end of the pipe, it does not politely close stdin.
    let probe = "/tmp/rmux-win-probe";
    let _ = run(target, &format!("rm -f {probe}")).await;

    run(
        target,
        &format!(
            "( printf 'for i in 1 2 3 4 5 6 7 8; do echo tick >> {probe}; sleep 1; done\\n'; sleep 4 ) | \
             '{}' attach --session win-probe --cwd /tmp > /dev/null 2>&1 &\n\
             C=$!\n\
             sleep 6\n\
             kill $C 2>/dev/null\n\
             wait $C 2>/dev/null\n\
             echo disconnected",
            installed.program
        ),
    )
    .await?;

    // **This is where it currently fails, and the failure is narrow.** Measured
    // on a real host: the agent installs and runs, the daemon starts detached,
    // the session table is correct, and the shell it spawns is genuinely
    // `bash.exe` (confirmed by pid in `tasklist`) — but nothing typed into the
    // session comes back out. The client sees only ConPTY's opening `ESC[6n`
    // and no prompt, so either input is not reaching the pty or output is not
    // being read from it. Everything up to this line is verified working; this
    // is the one remaining link.
    //
    // Nothing is attached now. If the shell belonged to the connection rather
    // than to the daemon, this is where the ticking stops.
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;

    let ticks = run(target, &format!("wc -l < {probe} 2>/dev/null || echo 0")).await?;
    let ticks: usize = ticks.trim().parse().unwrap_or(0);
    anyhow::ensure!(
        ticks >= 6,
        "the session stopped when its connection went away — only {ticks} ticks"
    );
    eprintln!("kept working with nothing attached: {ticks} ticks");

    // --- and reattaching finds the *same* shell -------------------------------
    let listing = run(target, &format!("'{}' list", installed.program)).await?;
    anyhow::ensure!(
        listing.lines().filter(|l| l.starts_with("win-probe")).count() == 1,
        "expected exactly one live session, got:\n{listing}"
    );

    let _ = run(target, &format!("'{}' kill win-probe; rm -f {probe}", installed.program)).await;
    Ok(())
}
