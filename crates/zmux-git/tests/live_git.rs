//! Against a real host. `cargo test -p zmux-git --test live_git -- --ignored --nocapture`
//!
//! Exists because a hand-run `ssh` command proved nothing: it composed the line
//! differently from `CommandSpec`, so it passed while the app failed. Only the
//! real code path answers what the real code path does.
use zmux_ssh::SshTarget;
use zmux_transport::Target;

#[tokio::test]
#[ignore = "needs a real host"]
async fn status_against_a_real_repository() {
    let host = std::env::var("ZMUX_LIVE_HOST").unwrap_or_else(|_| "example-host".into());
    let folder = std::env::var("ZMUX_LIVE_REPO")
        .unwrap_or_else(|_| "/home/anh.nguyen/redstone-agent".into());

    let ssh = SshTarget::new(zmux_transport::SshHostId::new(host));
    ssh.connect().await.expect("connect");
    let t: &dyn Target = &ssh;

    let root = zmux_git::repo_root(t, &folder).await.expect("repo_root");
    println!("root = {root:?}");
    let root = root.expect("a git repository");
    assert!(root.starts_with('/'), "root must be a path, got {root:?}");
    assert!(!root.contains('\n'), "root carried the shell preamble: {root:?}");

    let status = zmux_git::status(t, &root).await.expect("status");
    println!("branch = {:?}, changes = {}", status.branch, status.changes.len());
    assert!(!status.branch.is_empty());

    let log = zmux_git::log(t, &root, 5).await.expect("log");
    println!("commits = {}", log.len());
}

/// Two reads at once, which is what the pane actually does.
///
/// The sequential test above passed every time while the app failed
/// intermittently — and the difference was `Promise.all` in `GitPane`, not
/// anything in this crate. A test that does not reproduce the caller's
/// concurrency cannot see the caller's bug.
#[tokio::test]
#[ignore = "needs a real host"]
async fn concurrent_reads_do_not_starve_each_other() {
    let host = std::env::var("ZMUX_LIVE_HOST").unwrap_or_else(|_| "example-host".into());
    let folder = std::env::var("ZMUX_LIVE_REPO")
        .unwrap_or_else(|_| "/home/anh.nguyen/redstone-agent".into());

    let ssh = SshTarget::new(zmux_transport::SshHostId::new(host));
    ssh.connect().await.expect("connect");
    let t: &dyn Target = &ssh;
    let root = zmux_git::repo_root(t, &folder).await.unwrap().expect("a repository");

    // Five rounds: an intermittent fault that shows once in a session needs
    // more than one attempt to be caught deliberately.
    for round in 1..=5 {
        let (s, l) = tokio::join!(
            zmux_git::status(t, &root),
            zmux_git::log(t, &root, 20),
        );
        match (&s, &l) {
            (Ok(s), Ok(l)) => println!("round {round}: {} changes, {} commits", s.changes.len(), l.len()),
            _ => panic!("round {round}: status={:?} log={:?}", s.err(), l.err()),
        }
    }
}
