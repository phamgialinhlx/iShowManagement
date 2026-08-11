//! Installing a key on a **real** host, end to end.
//!
//! `#[ignore]` because it needs a reachable machine and writes to its
//! `authorized_keys`. Run deliberately:
//!
//! ```sh
//! RMUX_LIVE_HOST=example-host cargo test -p rmux-ssh --test live_keys -- --ignored --nocapture
//! ```
//!
//! What a unit test cannot prove and this can: that the remote script actually
//! creates `~/.ssh` with the right modes, that a second run is recognised as
//! already-present rather than appending a duplicate line, and that what lands
//! in the file is what was sent.
//!
//! This exists because the offer shipped **broken from the first release** — the
//! UI sent the wrong argument shape and every attempt died at the IPC boundary,
//! so nothing downstream had ever run once in anger.

use rmux_ssh::keys::{install_key, Installed};
use rmux_ssh::SshTarget;
use rmux_transport::{CommandSpec, SshHostId, Target};

/// **These tests share one `authorized_keys`, so they run one at a time.**
///
/// Both write a line tagged `rmux-live-test` and both count those lines, so run
/// in parallel each sees the other's key and the count assertion fails — while
/// the code under test is fine. Measured: `got "2"` where one was expected.
/// `persistence.rs` holds a mutex for the same reason, one directory over.
///
/// A **tokio** mutex rather than `std`: these tests await across the whole
/// critical section, and a `std` guard held over an await blocks the runtime
/// thread — which clippy rightly refuses.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Run a script and return **only what it printed**.
///
/// A login shell prints its MOTD and job-control notices to stdout ahead of the
/// command's own output, so `stdout.trim()` is the banner plus the answer. The
/// first version of this test compared that against `"1"` and reported a failure
/// on a host where the key had in fact been installed correctly — the same trap
/// `rmux-git` documents, arrived at independently.
///
/// A marker printed first, then split on, is the fix `rmux-git` settled on.
async fn run(target: &dyn Target, script: &str) -> String {
    const START: &str = "__RMUX_TEST_BEGIN__";
    // The trailing `;` is load-bearing: `{ cmd }` is a syntax error in POSIX sh
    // without it, so the whole line fails and only the banner comes back — which
    // is exactly what the first attempt saw.
    let line = format!("printf '%s' {START}; {{ {script}; }}");
    let spec = CommandSpec::login_shell().arg("-c").arg(line);
    let out = target.exec(&spec).await.expect("command").stdout;
    match out.rsplit_once(START) {
        Some((_banner, answer)) => answer.to_owned(),
        None => panic!("the marker never appeared — got: {out:?}"),
    }
}

/// Remove every line this test has ever written, without touching any other.
///
/// Run both before and after: before so a previous failure cannot make this one
/// fail, after so nothing is left on the host.
async fn purge(target: &dyn Target) {
    run(
        target,
        "f=\"$HOME/.ssh/authorized_keys\"; [ -f \"$f\" ] || exit 0; t=$(mktemp); \
         grep -v rmux-live-test \"$f\" > \"$t\" || true; chmod 600 \"$t\"; mv \"$t\" \"$f\"",
    )
    .await;
}

#[tokio::test]
#[ignore = "writes to a real host's authorized_keys"]
async fn a_key_is_installed_once_and_recognised_thereafter() {
    let Ok(alias) = std::env::var("RMUX_LIVE_HOST") else {
        eprintln!("set RMUX_LIVE_HOST to run this");
        return;
    };

    // Held for the whole test. A tokio mutex has no poisoning, which suits a
    // test: a panicking sibling should not also fail the next one for a reason
    // unrelated to what it is checking.
    let _serial = SERIAL.lock().await;

    let ssh = SshTarget::new(SshHostId { alias, user: None, port: None });
    ssh.connect().await.expect("connect");

    // **Start from a known state.** A run that fails before its cleanup leaves a
    // key behind, and the next run then counts two — so the *test* fails on the
    // wreckage of the previous one while the code under test is fine. Measured:
    // five leftover lines after five interrupted runs, reported as "exactly one
    // line should have been written". A test whose failures are cumulative
    // cannot be trusted to say anything about the code.
    purge(&ssh as &dyn Target).await;

    // A throwaway key, so the test never touches the operator's real one and
    // anything left behind by a failed run is obviously identifiable.
    let marker = format!("rmux-live-test-{}", std::process::id());
    let key = format!("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiveTestKeyDoNotTrust {marker}");

    let first = install_key(&ssh as &dyn Target, &key).await.expect("install");
    assert_eq!(first, Installed::Added, "a key not present must be added");

    // The *file* is what matters, not the return value.
    let count = run(&ssh as &dyn Target, "grep -c rmux-live-test \"$HOME/.ssh/authorized_keys\" || true").await;
    assert_eq!(count.trim(), "1", "exactly one line should have been written, got {count:?}");

    let modes = run(
        &ssh as &dyn Target,
        "stat -c '%a %a' \"$HOME/.ssh\" \"$HOME/.ssh/authorized_keys\" 2>/dev/null \
         || stat -f '%Lp %Lp' \"$HOME/.ssh\" \"$HOME/.ssh/authorized_keys\"",
    )
    .await;
    assert!(modes.contains("700"), "~/.ssh must be 700, got: {modes}");
    assert!(modes.contains("600"), "authorized_keys must be 600, got: {modes}");

    // **The repeat is the point.** Without the grep guard every offer would
    // append another copy and the file would grow for the life of the host.
    let second = install_key(&ssh as &dyn Target, &key).await.expect("install again");
    assert_eq!(second, Installed::AlreadyPresent, "a repeat must not append");
    let after = run(&ssh as &dyn Target, "grep -c rmux-live-test \"$HOME/.ssh/authorized_keys\" || true").await;
    assert_eq!(after.trim(), "1", "a second install must not duplicate the line");

    purge(&ssh as &dyn Target).await;
    let cleaned = run(&ssh as &dyn Target, "grep -c rmux-live-test \"$HOME/.ssh/authorized_keys\" || true").await;
    assert_eq!(cleaned.trim(), "0", "the test key must be removed again");
}

/// **The whole point: after installing, the key is actually used.**
///
/// Everything else can pass while the operator still types a password on every
/// connection — which is what shipped. rmux writes its key to a filename
/// OpenSSH does not try on its own, so the public half was installed, the offer
/// said "key added", and nothing changed.
///
/// This asks `ssh` itself, with passwords disabled: if it authenticates, the key
/// is being offered and accepted. Nothing else can make that connection succeed.
#[tokio::test]
#[ignore = "writes to a real host's authorized_keys"]
async fn the_installed_key_is_what_authenticates() {
    let Ok(alias) = std::env::var("RMUX_LIVE_HOST") else {
        eprintln!("set RMUX_LIVE_HOST to run this");
        return;
    };

    let _serial = SERIAL.lock().await;

    let home = dirs::home_dir().expect("home");
    let ssh = SshTarget::new(SshHostId { alias: alias.clone(), user: None, port: None });
    ssh.connect().await.expect("connect");
    purge(&ssh as &dyn Target).await;

    // A real keypair, generated the way the offer generates it.
    let marker = format!("rmux-live-test-{}", std::process::id());
    let key_file = rmux_ssh::keys::key_path(&home, &format!("livetest-{marker}"));
    let public = rmux_ssh::keys::ensure_local_key(&key_file, &marker).expect("generate");
    rmux_ssh::keys::install_key(&ssh as &dyn Target, &public).await.expect("install");

    // Ask ssh directly, with every other method refused. `BatchMode` forbids
    // prompting and `PasswordAuthentication=no` removes the fallback, so success
    // means the identity was offered and accepted.
    let out = std::process::Command::new("ssh")
        .arg("-i").arg(&key_file)
        .arg("-o").arg("IdentitiesOnly=yes")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("PasswordAuthentication=no")
        .arg("-o").arg("KbdInteractiveAuthentication=no")
        .arg("-o").arg("ControlPath=none")
        .arg("-o").arg("ConnectTimeout=15")
        .arg(&alias)
        .arg("echo authenticated-by-key")
        .output()
        .expect("spawn ssh");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Clean up before asserting, so a failure does not leave the host altered.
    purge(&ssh as &dyn Target).await;
    let _ = std::fs::remove_file(&key_file);
    let _ = std::fs::remove_file(key_file.with_extension("pub"));

    assert!(
        stdout.contains("authenticated-by-key"),
        "the installed key did not authenticate — the password prompt would keep coming.\nstderr: {stderr}"
    );
}
