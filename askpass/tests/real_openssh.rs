//! Verifies the bridge against the **real OpenSSH binaries**.
//!
//! `tests/bridge.rs` proves our helper and server agree with each other, but both
//! sides are ours — it cannot show that OpenSSH actually honours the contract.
//! The assumptions that matter are external: that `SSH_ASKPASS_REQUIRE=force`
//! really does route prompts to the helper even with a terminal present, that the
//! prompt arrives in `argv`, and that stdout is accepted as the secret. If any of
//! those is wrong, every password and 2FA host fails and no unit test would say so.
//!
//! `ssh-keygen -y` asks for a key passphrase through exactly the same askpass
//! path `ssh` uses for passwords, so it exercises the real contract without
//! needing a server to connect to.

use std::process::Command;
use std::sync::Arc;

use rmux_ssh::askpass::{AskpassServer, Prompt, server::Answerer};

const PASSPHRASE: &str = "correct-horse-battery-staple";

fn answer_with(reply: Option<&'static str>) -> Answerer {
    Arc::new(move |_prompt: Prompt| Box::pin(async move { reply.map(str::to_owned) }))
}

/// Create an encrypted key. Returns `None` if the toolchain is unavailable.
fn make_encrypted_key(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let key = dir.join("id_ed25519");

    let status = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", PASSPHRASE, "-C", "rmux-askpass-test", "-q", "-f"])
        .arg(&key)
        .status()
        .ok()?;

    status.success().then_some(key)
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rmux-askpass-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

#[tokio::test]
async fn real_openssh_takes_the_passphrase_from_our_helper() {
    let dir = temp_dir("ok");
    let Some(key) = make_encrypted_key(&dir) else {
        eprintln!("skipping: ssh-keygen unavailable");
        return;
    };

    let server = AskpassServer::start(answer_with(Some(PASSPHRASE))).await.unwrap();
    let socket = server.socket_path().display().to_string();
    let token = server.token().to_owned();
    let helper = env!("CARGO_BIN_EXE_rmux-askpass").to_owned();

    // Exactly the environment `rmux_ssh::askpass::env_for_gui_prompts` builds.
    let output = tokio::task::spawn_blocking(move || {
        Command::new("ssh-keygen")
            .arg("-y")
            .arg("-f")
            .arg(&key)
            .env("SSH_ASKPASS", &helper)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("RMUX_ASKPASS_SOCKET", &socket)
            .env("RMUX_ASKPASS_TOKEN", &token)
            .env("DISPLAY", ":0")
            .output()
            .expect("failed to run ssh-keygen")
    })
    .await
    .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "OpenSSH rejected the passphrase our helper supplied.\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Decrypting the private key is only possible with the right passphrase, so a
    // public key here proves the secret made the whole round trip.
    assert!(stdout.starts_with("ssh-ed25519 "), "unexpected output: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn real_openssh_fails_when_the_user_dismisses_the_dialog() {
    let dir = temp_dir("cancel");
    let Some(key) = make_encrypted_key(&dir) else {
        eprintln!("skipping: ssh-keygen unavailable");
        return;
    };

    // The user hit Cancel.
    let server = AskpassServer::start(answer_with(None)).await.unwrap();
    let socket = server.socket_path().display().to_string();
    let token = server.token().to_owned();
    let helper = env!("CARGO_BIN_EXE_rmux-askpass").to_owned();

    let output = tokio::task::spawn_blocking(move || {
        Command::new("ssh-keygen")
            .arg("-y")
            .arg("-f")
            .arg(&key)
            .env("SSH_ASKPASS", &helper)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("RMUX_ASKPASS_SOCKET", &socket)
            .env("RMUX_ASKPASS_TOKEN", &token)
            .env("DISPLAY", ":0")
            .output()
            .expect("failed to run ssh-keygen")
    })
    .await
    .unwrap();

    // Cancelling must abort, not silently proceed with an empty passphrase.
    assert!(!output.status.success(), "cancelling should have failed the operation");

    let _ = std::fs::remove_dir_all(&dir);
}
