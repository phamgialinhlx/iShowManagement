//! End-to-end test of the askpass bridge.
//!
//! Each half is unit-tested on its own, but the thing that actually has to work
//! is the whole chain: OpenSSH execs the helper with a prompt, the helper reaches
//! rmux over the socket, the user answers, and the secret comes back on stdout
//! with a zero exit status. A mistake anywhere in that chain means password and
//! 2FA hosts cannot be used at all — and the symptom is `ssh` hanging or failing
//! with nothing explaining why.
//!
//! So these tests run the **real compiled helper binary** as a subprocess against
//! a **real server**, exactly as OpenSSH would.

use std::process::Command;
use std::sync::Arc;

use rmux_ssh::askpass::{AskpassServer, Prompt, server::Answerer};

/// An answerer that always replies the same way.
fn answer_with(reply: Option<&'static str>) -> Answerer {
    Arc::new(move |_prompt: Prompt| Box::pin(async move { reply.map(str::to_owned) }))
}

/// Run the helper the way OpenSSH does: prompt as argv[1], secret on stdout.
fn run_helper(socket: &str, token: &str, prompt: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rmux-askpass"))
        .arg(prompt)
        .env("RMUX_ASKPASS_SOCKET", socket)
        .env("RMUX_ASKPASS_TOKEN", token)
        .output()
        .expect("failed to run the askpass helper")
}

#[tokio::test]
async fn openssh_receives_the_secret_the_user_typed() {
    let server = AskpassServer::start(answer_with(Some("hunter2"))).await.unwrap();
    let socket = server.socket_path().display().to_string();
    let token = server.token().to_owned();

    // Blocking subprocess, so keep it off the async worker.
    let output =
        tokio::task::spawn_blocking(move || run_helper(&socket, &token, "deploy@devbox's password:"))
            .await
            .unwrap();

    assert!(output.status.success(), "helper failed: {}", String::from_utf8_lossy(&output.stderr));
    // OpenSSH reads the first line of stdout as the secret, so the trailing
    // newline is expected and the content must be exact — a stray prefix or a
    // debug line here would be sent to the server as the password.
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hunter2\n");
}

#[tokio::test]
async fn dismissing_the_dialog_aborts_authentication() {
    let server = AskpassServer::start(answer_with(None)).await.unwrap();
    let socket = server.socket_path().display().to_string();
    let token = server.token().to_owned();

    let output = tokio::task::spawn_blocking(move || run_helper(&socket, &token, "password:"))
        .await
        .unwrap();

    // A zero exit with empty stdout would make ssh try an empty password and burn
    // an authentication attempt; non-zero makes it give up cleanly.
    assert!(!output.status.success(), "cancelling must not report success");
    assert!(output.stdout.is_empty(), "nothing may be offered as a secret when cancelled");
}

#[tokio::test]
async fn a_helper_with_the_wrong_token_gets_no_secret() {
    let server = AskpassServer::start(answer_with(Some("hunter2"))).await.unwrap();
    let socket = server.socket_path().display().to_string();

    let output =
        tokio::task::spawn_blocking(move || run_helper(&socket, "not-the-token", "password:"))
            .await
            .unwrap();

    assert!(!output.status.success());
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("hunter2"),
        "the secret leaked to an unauthorised caller"
    );
}

#[test]
fn the_helper_refuses_to_run_outside_rmux() {
    // Someone's shell may have SSH_ASKPASS pointing here from a stale export. It
    // must fail fast rather than block an ssh session waiting on a socket that
    // will never answer.
    let output = Command::new(env!("CARGO_BIN_EXE_rmux-askpass"))
        .arg("password:")
        .env_remove("RMUX_ASKPASS_SOCKET")
        .env_remove("RMUX_ASKPASS_TOKEN")
        .output()
        .expect("failed to run the askpass helper");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn the_helper_fails_fast_when_rmux_is_not_listening() {
    // rmux quit while ssh was mid-authentication.
    let output = Command::new(env!("CARGO_BIN_EXE_rmux-askpass"))
        .arg("password:")
        .env("RMUX_ASKPASS_SOCKET", "/tmp/rmux-does-not-exist.sock")
        .env("RMUX_ASKPASS_TOKEN", "irrelevant")
        .output()
        .expect("failed to run the askpass helper");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
