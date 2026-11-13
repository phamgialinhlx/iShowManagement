//! TEST CONNECTION against a real host, with an explicit key and port.
//!
//! Ignored by default; read-only (`uname -s`). Run with:
//!
//! ```sh
//! RMUX_LIVE_PROBE_HOST=dev@10.0.0.107 RMUX_LIVE_PROBE_KEY=~/.ssh/rmux_dev_10.0.0.107_ed25519 \
//!   cargo test -p rmux-ssh --test live_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use rmux_transport::SshHostId;

#[tokio::test]
#[ignore]
async fn the_probe_connects_with_the_chosen_key_and_port() {
    let dest = std::env::var("RMUX_LIVE_PROBE_HOST").expect("RMUX_LIVE_PROBE_HOST");
    let key = std::env::var("RMUX_LIVE_PROBE_KEY").expect("RMUX_LIVE_PROBE_KEY");
    let key = match key.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap().join(rest),
        None => PathBuf::from(key),
    };
    let (user, alias) = match dest.split_once('@') {
        Some((u, h)) => (Some(u.to_owned()), h.to_owned()),
        None => (None, dest.clone()),
    };
    let host = SshHostId { alias, user, port: Some(22) };

    let outcome = rmux_ssh::probe::test_connection(&host, Some(&key)).await;
    println!("probe: {outcome:?}");
    assert!(outcome.as_ref().is_ok_and(|s| s.starts_with("connected")), "{outcome:?}");

    // A wrong port must come back as ssh's reason, not a success.
    let wrong = SshHostId { port: Some(1), ..host };
    let outcome = rmux_ssh::probe::test_connection(&wrong, Some(&key)).await;
    println!("wrong port: {outcome:?}");
    assert!(outcome.is_err());
}
