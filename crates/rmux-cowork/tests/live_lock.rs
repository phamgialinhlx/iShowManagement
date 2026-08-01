//! Live checks against a real Cowork server.
//!
//! `#[ignore]`d: they need a real session token and a reachable server, so they
//! cannot run in CI. Run them by hand with:
//!
//! ```sh
//! export RMUX_LIVE_SERVER=https://cowork.example.com
//! RMUX_LIVE_TOKEN=$(security find-generic-password -s ai.betterscale.rmux \
//!     -a "$RMUX_LIVE_SERVER" -w | python3 -c \
//!     'import sys,json; print(json.load(sys.stdin)["token"])') \
//!   cargo test -p rmux-cowork --test live_lock -- --ignored --nocapture
//! ```
//!
//! Both the server and the token come from the environment so that no
//! deployment of this app is named in the repository.
//!
//! **The token comes from the environment rather than from the keychain**, and
//! that is not laziness. A fresh test binary is not on the keychain item's access
//! list, so reading it directly makes macOS raise an authorisation dialog and the
//! run blocks on a prompt nobody was expecting — indefinitely, under a test
//! harness that shows no output while it waits.
//!
//! What these are for: the unit tests prove the vault round-trips *a* credential.
//! Only these prove it round-trips a **real token** and that what comes back out
//! still authenticates. That failure would present as "sign in again" after every
//! restart, with the stored session silently destroyed.

use rmux_cowork::{Session, StoredCredentials, VaultKey, lock};

fn server() -> String {
    std::env::var("RMUX_LIVE_SERVER").expect("set RMUX_LIVE_SERVER to your Cowork server URL")
}

fn stored() -> StoredCredentials {
    let token = std::env::var("RMUX_LIVE_TOKEN")
        .expect("set RMUX_LIVE_TOKEN — see the module comment for how");
    assert!(!token.is_empty(), "RMUX_LIVE_TOKEN is empty");

    StoredCredentials { token, refresh_token: None, username: "live-test".into() }
}

#[tokio::test]
#[ignore = "needs a signed-in session and a reachable server"]
async fn a_real_token_survives_sealing_and_still_authenticates() {
    let original = stored();

    // Seal and open, exactly as locking and unlocking would.
    let key = VaultKey::new("482913").expect("derive");
    let sealed = key.seal(&original, false).expect("seal");
    let reopened = key.open(&sealed).expect("open");

    assert_eq!(reopened, original, "the vault did not return the credential unchanged");

    // The real test: the token that came out of the vault still works. A
    // round-trip that compares equal but produces a token the server rejects
    // would pass every unit test and lock the user out on the next restart.
    let session = Session::resume(&server(), reopened).expect("resume");
    let account = session.me().await.expect("the unsealed token was rejected by the server");

    println!("unsealed token authenticated as {} ({})", account.label(), account.username);
    assert!(!account.username.is_empty());

    // And the vault is genuinely opaque: the live token must not be readable
    // from the sealed form.
    let json = serde_json::to_string(&sealed).unwrap();
    assert!(!json.contains(&original.token), "the sealed vault leaks the token");
}

#[tokio::test]
#[ignore = "needs a signed-in session and a reachable server"]
async fn the_wrong_pin_cannot_recover_a_real_session() {
    let original = stored();
    let sealed = lock::seal(&original, "482913", false).expect("seal");

    for guess in ["482914", "0000", "12345678"] {
        assert!(
            matches!(lock::open(&sealed, guess), Err(lock::LockError::WrongPin)),
            "{guess} opened the vault"
        );
    }
}

/// Trusting this machine is a **mutating** call — it writes a `device_trust` row
/// that the server has no endpoint to revoke. So it is separate, and left out of
/// the default ignored run on purpose.
#[tokio::test]
#[ignore = "mutates the account: writes a device_trust row that cannot be revoked"]
async fn this_machine_can_be_trusted_for_face_unlock() {
    let session = Session::resume(&server(), stored()).expect("resume");

    let account = session.me().await.expect("me");
    println!(
        "account has_face={} face_count={} has_pin={}",
        account.has_face, account.face_count, account.has_pin
    );

    let trust = session.trust_device("rmux live test").await.expect("trust_device");

    // The shape is what face login depends on; a server that returned something
    // else would fail much later, inside an unlock nobody could debug.
    assert!(trust.secret.starts_with("rcwd_"), "{}", trust.secret);
    assert_eq!(trust.secret.len(), "rcwd_".len() + 48);
    assert_eq!(trust.server_url, server());

    println!("device trusted; secret is {} chars", trust.secret.len());
}
