//! Proves the one property the agent exists for: a shell survives losing its
//! client.
//!
//! Everything else — the framing, the session table — is machinery in service of
//! this. If a session does not outlive the connection, the agent has no reason to
//! exist and rmux may as well keep running `ssh` directly.
//!
//! These drive the **real compiled binary** as a subprocess, the same way `ssh`
//! would, rather than calling library functions in-process.

use std::io::{BufReader, Write};
use std::process::{Child, Command, Stdio};

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
            .join(format!("rmux-agent-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Self { home }
    }

    fn attach(&self, session: &str) -> Child {
        Command::new(env!("CARGO_BIN_EXE_rmux-agent"))
            .args(["attach", "--session", session, "--cols", "80", "--rows", "24"])
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to run rmux-agent")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Stop the daemon this test started, then remove its directory.
        let _ = Command::new("pkill")
            .args(["-f", "rmux-agent daemon"])
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

    // Killed outright — the moral equivalent of the network dropping or rmux
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
