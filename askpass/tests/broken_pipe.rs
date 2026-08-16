//! The helper must survive an `ssh` that has already gone away.
//!
//! This is a regression test for a crash that presented as something else
//! entirely: **a password prompt that would not stop coming back**.
//!
//! `println!` panics when stdout is a broken pipe, and the workspace sets
//! `panic = "abort"`, so an EPIPE became SIGABRT. The helper died without
//! delivering the secret, `ssh` failed, and zmux asked again. Nothing in the
//! loop mentioned a pipe, so the symptom pointed at credentials.
//!
//! The window is ordinary: `ssh` is gone while the dialog is still open whenever
//! the app quits mid-prompt, a ControlMaster is torn down, or a credential was
//! just refused.
//!
//! Measured against the real binary — the old code exits **-6** here, the fixed
//! one exits 1 and says why.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

/// A stand-in for zmux: takes the request, then answers with a secret.
///
/// It waits before answering so the child is definitely past the point where it
/// could notice its stdout is gone — the answer has to arrive *into* a broken
/// pipe for this to test anything.
fn serve(listener: UnixListener) {
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut reader = BufReader::new(&stream);
        let mut request = String::new();
        let _ = reader.read_line(&mut request);

        std::thread::sleep(std::time::Duration::from_millis(300));

        let mut writer = &stream;
        let _ = writeln!(writer, r#"{{"answer":"hunter2"}}"#);
        let _ = writer.flush();
    });
}

#[test]
fn a_closed_stdout_is_reported_not_a_crash() {
    let dir = std::env::temp_dir().join(format!("zmux-askpass-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let socket = dir.join("askpass.sock");

    let listener = UnixListener::bind(&socket).expect("a listening socket");
    serve(listener);

    let mut child = Command::new(env!("CARGO_BIN_EXE_zmux-askpass"))
        .arg("password:")
        .env("ZMUX_ASKPASS_SOCKET", &socket)
        .env("ZMUX_ASKPASS_TOKEN", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the helper should start");

    // **This is the whole setup.** Dropping our end of the pipe closes it, so
    // the child's write to stdout gets EPIPE — exactly what a departed `ssh`
    // leaves behind.
    drop(child.stdout.take());

    let out = child.wait_with_output().expect("the helper should be waitable");
    let stderr = String::from_utf8_lossy(&out.stderr);

    // `code()` is `None` when a signal killed it — which is precisely the old
    // failure, so this assertion is the point of the test.
    let code = out.status.code();
    assert!(
        code.is_some(),
        "the helper was killed by a signal instead of returning; stderr: {stderr}"
    );

    assert!(
        !stderr.contains("panicked"),
        "the helper panicked rather than handling the closed pipe; stderr: {stderr}"
    );

    // It must also *say* what happened. A silent non-zero exit here would send
    // whoever debugs this back to the credentials, which is where the original
    // week went.
    assert!(
        stderr.contains("could not deliver the answer"),
        "the helper should name the delivery failure; stderr: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
