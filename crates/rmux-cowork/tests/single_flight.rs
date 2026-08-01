//! Proves the refresh is genuinely single-flighted.
//!
//! This is the one behaviour in the crate that cannot be verified by unit-testing
//! a predicate, because the bug it prevents *is* a race. The Cowork server rotates
//! the refresh token on every use: the moment a new pair is issued the old token
//! is dead. If two callers both notice an expiring token and both refresh, one of
//! them presents an already-spent token, the server rejects it, and the session
//! wedges until stored credentials are cleared by hand. The old Electron client
//! hit exactly this and had to add a shared-promise guard after the fact.
//!
//! So the test drives real HTTP against a throwaway server and counts how many
//! times `/auth/redstone/refresh` is actually hit while many callers ask for an
//! `Authorization` header at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rmux_cowork::Session;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A minimal HTTP/1.1 server that speaks just enough for these two endpoints.
///
/// Hand-rolled rather than pulled from a mock-server crate so the test has no
/// dependency that could itself serialise requests and hide the race.
async fn spawn_server(refresh_hits: Arc<AtomicUsize>) -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { continue };
            let hits = Arc::clone(&refresh_hits);

            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(n) = socket.read(&mut buf).await else { return };
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();

                let body = if req.contains("/auth/redstone/refresh") {
                    // Count before responding. Deliberately slow: a real refresh
                    // takes a network round trip, and that window is precisely
                    // when a second caller would slip in if the lock were wrong.
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                    r#"{"access_token":"renewed","refresh_token":"r2","expires_in":3600}"#
                } else {
                    // Login. `expires_in: 1` puts the token inside the 60s refresh
                    // skew immediately, so the very next caller must refresh.
                    r#"{"access_token":"initial","refresh_token":"r1","expires_in":1}"#
                };

                let res = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(res.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    Ok(format!("http://{addr}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_callers_trigger_exactly_one_refresh() -> anyhow::Result<()> {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_server(Arc::clone(&hits)).await?;

    let session = Arc::new(Session::login_sso(base, "nolan", "pw").await?);

    // Everything the UI does at once on wake — poll the leaderboard, fetch DMs,
    // list servers — each needing an Authorization header from an expiring token.
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let session = Arc::clone(&session);
        tasks.push(tokio::spawn(async move { session.authorization().await }));
    }

    let mut headers = Vec::new();
    for task in tasks {
        headers.push(task.await??);
    }

    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "refresh must happen exactly once; more than one means a rotated token was spent twice"
    );

    // Every caller must observe the renewed token — a caller that captured the
    // pre-refresh value would send a token the server has already invalidated.
    assert!(
        headers.iter().all(|h| h == "Bearer renewed"),
        "all callers should see the refreshed token, got: {:?}",
        headers.iter().collect::<std::collections::BTreeSet<_>>()
    );

    // A later call reuses the still-valid token rather than refreshing again.
    session.authorization().await?;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "a valid token must not trigger a refresh");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_session_reports_expiry_instead_of_refreshing() -> anyhow::Result<()> {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_server(Arc::clone(&hits)).await?;

    // Account tokens (`rcwa_`) carry no refresh token at all.
    let session = Session::resume(
        base,
        rmux_cowork::StoredCredentials {
            token: "rcwa_abc".into(),
            refresh_token: None,
            username: "nolan".into(),
        },
    )?;

    // Normal use must not attempt a refresh...
    assert_eq!(session.authorization().await?, "Bearer rcwa_abc");
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    // ...and a forced refresh (the post-401 recovery path) must report the session
    // as expired rather than calling an endpoint that cannot help it.
    let err = session.refresh().await.unwrap_err();
    assert!(err.requires_signin(), "expected a sign-in prompt, got: {err}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "account tokens must never hit the refresh endpoint");

    Ok(())
}
