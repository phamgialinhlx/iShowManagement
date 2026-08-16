//! Proves the one property the agent exists for: a shell survives losing its
//! client.
//!
//! Everything else — the framing, the session table — is machinery in service of
//! this. If a session does not outlive the connection, the agent has no reason to
//! exist and zmux may as well keep running `ssh` directly.
//!
//! These drive the **real compiled binary** as a subprocess, the same way `ssh`
//! would, rather than calling library functions in-process.

use std::io::{BufReader, Write};
use std::process::{Child, Command, Stdio};

/// Serialises the tests in this file.
///
/// Both of them spawn real daemons and tear them down with `pkill`, which is a
/// process-wide instrument — run in parallel, each one's cleanup lands in the
/// middle of the other's run, and they pass alone while failing together. That
/// is the worst way for a test to be wrong, and it is why the file used to hold
/// exactly one test. A lock is the smaller price than a single thousand-line
/// test.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A private HOME, so a real agent on this machine is never disturbed.
///
/// Cleanup kills daemons by name, which is process-wide — so everything here
/// lives in **one** test. Split across several, each one's teardown killed the
/// others' daemons mid-run: they passed alone and failed together, which is the
/// worst way for a test to be wrong.
struct Sandbox {
    home: std::path::PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let home = std::env::temp_dir()
            .join(format!("zmuxd-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Self { home }
    }

    fn attach(&self, session: &str) -> Child {
        Command::new(env!("CARGO_BIN_EXE_zmuxd"))
            .args(["attach", "--session", session, "--cols", "80", "--rows", "24"])
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to run zmuxd")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Stop the daemon this test started, then remove its directory.
        let _ = Command::new("pkill")
            .args(["-f", "zmuxd daemon"])
            .env("HOME", &self.home)
            .status();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Read from a child's stdout until `needle` appears, or give up.
fn read_until(child: &mut Child, needle: &str, timeout: std::time::Duration) -> String {
    let stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut seen = String::new();
        let mut byte = [0u8; 1];
        loop {
            use std::io::Read;
            match reader.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    seen.push(byte[0] as char);
                    if tx.send(seen.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let deadline = std::time::Instant::now() + timeout;
    let mut latest = String::new();
    while std::time::Instant::now() < deadline {
        if let Ok(seen) = rx.recv_timeout(std::time::Duration::from_millis(100)) {
            latest = seen;
            if latest.contains(needle) {
                return latest;
            }
        }
    }
    latest
}

#[test]
fn sessions_persist_across_disconnection() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sandbox = Sandbox::new("persist");

    // --- a shell survives its client being killed ---------------------------
    let mut first = sandbox.attach("work");
    {
        let stdin = first.stdin.as_mut().expect("stdin was piped");
        // A shell variable only this shell knows. If the session is really the
        // same one later, it is still set.
        stdin.write_all(b"MARKER=survived-the-disconnect\n").unwrap();
        stdin.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }

    // Killed outright — the moral equivalent of the network dropping or zmux
    // being force-quit. No clean shutdown, no goodbye.
    first.kill().expect("failed to kill the client");
    let _ = first.wait();
    std::thread::sleep(std::time::Duration::from_millis(600));

    let mut second = sandbox.attach("work");
    {
        let stdin = second.stdin.as_mut().expect("stdin was piped");
        stdin.write_all(b"echo RECALLED:$MARKER\n").unwrap();
        stdin.flush().unwrap();
    }

    let seen = read_until(
        &mut second,
        "RECALLED:survived-the-disconnect",
        std::time::Duration::from_secs(10),
    );
    let _ = second.kill();
    let _ = second.wait();

    assert!(
        seen.contains("RECALLED:survived-the-disconnect"),
        "the shell did not survive the client being killed. saw:\n{seen}"
    );

    // --- reattaching replays what was missed --------------------------------
    let mut third = sandbox.attach("history");
    {
        let stdin = third.stdin.as_mut().unwrap();
        stdin.write_all(b"echo EARLIER-OUTPUT\n").unwrap();
        stdin.flush().unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let _ = third.kill();
    let _ = third.wait();
    std::thread::sleep(std::time::Duration::from_millis(400));

    // Without replay, reattaching lands you in a blank window with no idea what
    // the shell has been doing.
    let mut fourth = sandbox.attach("history");
    let replayed = read_until(&mut fourth, "EARLIER-OUTPUT", std::time::Duration::from_secs(10));
    let _ = fourth.kill();
    let _ = fourth.wait();

    assert!(
        replayed.contains("EARLIER-OUTPUT"),
        "scrollback was not replayed. saw:\n{replayed}"
    );

    // --- distinct names are distinct shells ---------------------------------
    let mut alpha = sandbox.attach("alpha");
    {
        let stdin = alpha.stdin.as_mut().unwrap();
        stdin.write_all(b"WHICH=alpha\n").unwrap();
        stdin.flush().unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(1000));

    let mut beta = sandbox.attach("beta");
    {
        let stdin = beta.stdin.as_mut().unwrap();
        stdin.write_all(b"echo LEAKED:[$WHICH]\n").unwrap();
        stdin.flush().unwrap();
    }
    let leak = read_until(&mut beta, "LEAKED:", std::time::Duration::from_secs(10));

    let _ = alpha.kill();
    let _ = beta.kill();
    let _ = alpha.wait();
    let _ = beta.wait();

    assert!(leak.contains("LEAKED:"), "the second shell never answered. saw:\n{leak}");
    assert!(
        !leak.contains("LEAKED:[alpha]"),
        "state leaked between two differently-named sessions:\n{leak}"
    );
}

/// Two builds must not each start their own copy of the same session.
///
/// **This is the bug that cost real work.** The daemon socket carries the
/// binary's content fingerprint, so a rebuilt agent deliberately starts its own
/// daemon — upgrading must not kill a run in progress. What was missing is the
/// other half: the new client could not *see* the sessions the old daemon still
/// held, so it created a second Claude under the same name while the first kept
/// running, orphaned and unreachable by anything. Measured on a real host, one
/// session name existed three times across three daemons, the oldest 27 hours
/// old and still detached.
///
/// The two "builds" here are two copies of the same binary under different
/// installed names, which is exactly what distinguishes two builds at runtime:
/// the socket is derived from the file name, which `provision` stamps with the
/// fingerprint.
#[test]
fn an_upgraded_agent_adopts_the_old_build_s_session() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    // **A short home, not `temp_dir()`.** These socket names carry a build
    // fingerprint, which is longer than the plain `agent-<version>` the other
    // test produces — and macOS's `temp_dir()` is already a deep
    // `/var/folders/…` path. Together they cross the ~104-byte `sun_path` limit,
    // and `bind` then fails with an error that never mentions length: the shell
    // simply never starts and the test sees empty output. That is the exact trap
    // documented in `ipc::socket_stem`, met again from the other direction.
    let tag = std::process::id();
    let home = std::path::PathBuf::from(format!("/tmp/zmux-h{tag}"));
    let _ = std::fs::remove_dir_all(&home);
    let bin = home.join(".zmux/bin");
    std::fs::create_dir_all(&bin).unwrap();

    struct ShortHome(std::path::PathBuf, u32);
    impl Drop for ShortHome {
        fn drop(&mut self) {
            // Only this test's daemons, matched by its unique tag.
            for name in [format!("old{}", self.1), format!("new{}", self.1)] {
                let _ = Command::new("pkill").args(["-f", &name]).status();
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = ShortHome(home.clone(), tag);
    let old = bin.join(format!("zmuxd-0.1.0-old{tag}"));
    let new = bin.join(format!("zmuxd-0.1.0-new{tag}"));
    for path in [&old, &new] {
        std::fs::copy(env!("CARGO_BIN_EXE_zmuxd"), path).unwrap();
    }

    let run = |exe: &std::path::Path, args: &[&str]| {
        Command::new(exe)
            .args(args)
            .env("HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to run the agent")
    };

    let session = "claude-handoff-1";

    // --- the "old build" creates the session -------------------------------
    let mut first = run(&old, &["attach", "--session", session, "--cols", "80", "--rows", "24"]);
    first
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"echo ready-one\n")
        .unwrap();
    let seen = read_until(&mut first, "ready-one", std::time::Duration::from_secs(15));
    assert!(seen.contains("ready-one"), "the first build never started a shell: {seen:?}");

    // Disconnect, leaving the session running under the old daemon.
    let _ = first.kill();
    let _ = first.wait();
    std::thread::sleep(std::time::Duration::from_millis(400));

    // --- the "new build" attaches to the same name -------------------------
    let mut second = run(&new, &["attach", "--session", session, "--cols", "80", "--rows", "24"]);
    second
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"echo ready-two\n")
        .unwrap();
    let seen = read_until(&mut second, "ready-two", std::time::Duration::from_secs(15));
    assert!(seen.contains("ready-two"), "the second build never got a shell: {seen:?}");

    let _ = second.kill();
    let _ = second.wait();
    std::thread::sleep(std::time::Duration::from_millis(400));

    // --- exactly one daemon may hold this name -----------------------------
    //
    // The assertion that fails without the handoff: each build answers for its
    // own daemon, and before the fix the new one had created a session of its
    // own, so the name existed twice and there were two shells.
    let listing = |exe: &std::path::Path| -> String {
        let out = Command::new(exe)
            .arg("list")
            .env("HOME", &home)
            .output()
            .expect("list failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let from_new = listing(&new);
    let copies = from_new.lines().filter(|l| l.starts_with(session)).count();
    assert_eq!(copies, 1, "the session exists {copies} times, not once:\n{from_new}");

    // And the new build can find it at all — a listing that only asked its own
    // daemon would report nothing here, which is how the orphans stayed hidden.
    assert!(from_new.contains(session), "the upgraded build cannot see the session:\n{from_new}");

}
