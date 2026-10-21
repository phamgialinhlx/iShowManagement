//! Disconnecting a server must actually let go of it.
//!
//! rmux caches a resolved target in six independent places. **One survivor is
//! enough to keep the connection up**, and a disconnect that half-worked is
//! indistinguishable from one that did nothing — the operator presses the button
//! again and nothing happens again.
//!
//! So each cache is pinned separately, and the count is part of the contract.

use std::sync::Arc;

use rmux_app::agent::AgentStore;
use rmux_app::claude::ClaudeStore;
use rmux_app::claude_status::ClaudeStatusStore;
use rmux_app::files::FsStore;
use rmux_app::metrics::MetricsStore;
use rmux_app::terminal::TerminalStore;
use rmux_transport::{LocalTarget, Target, TargetId};

fn local() -> Arc<dyn Target> {
    Arc::new(LocalTarget::new())
}

/// Every cache answers the same three questions: absent is `false`, present is
/// `true`, and a second eviction is `false` again — the last one being what
/// stops a double-click reporting a disconnect that did not happen.
#[test]
fn the_terminal_store_forgets_a_target() {
    let store = TerminalStore::default();
    let id = TargetId::Local;

    assert!(!store.evict_target(&id), "nothing was cached yet");
    store.insert_for_test(id.clone(), local());
    assert!(store.evict_target(&id), "the cached target should be dropped");
    assert!(!store.evict_target(&id), "and dropping it twice is not a second disconnect");
}

#[test]
fn the_claude_store_forgets_a_target() {
    let store = ClaudeStore::default();
    let id = TargetId::Local;

    assert!(!store.evict_target(&id));
    store.insert_for_test(id.clone(), local());
    assert!(store.evict_target(&id));
    assert!(!store.evict_target(&id));
}

#[test]
fn the_filesystem_store_forgets_a_target() {
    let store = FsStore::default();
    let id = TargetId::Local;

    assert!(!store.evict_target(&id));
    store.insert_for_test(id.clone(), Arc::new(rmux_fs::LocalFs::new()));
    assert!(store.evict_target(&id));
    assert!(!store.evict_target(&id));
}

#[test]
fn the_agent_store_forgets_a_provisioning_record() {
    let store = AgentStore::default();
    let id = TargetId::Local;

    assert!(!store.forget(&id));
    store.insert_for_test(id.clone(), Arc::new(tokio::sync::OnceCell::new()));
    assert!(store.forget(&id), "the provisioning cell should be dropped");
    assert!(!store.forget(&id));
}

#[tokio::test]
async fn the_metrics_store_forgets_a_target() {
    let store = MetricsStore::default();
    let id = TargetId::Local;

    assert!(!store.evict_target(&id).await);
    store.insert_for_test(id.clone(), local()).await;
    assert!(store.evict_target(&id).await, "the collector and its baseline go together");
    assert!(!store.evict_target(&id).await);
}

/// **The one that would otherwise undo the disconnect.**
///
/// The status watcher runs `rmux-agent watch-status` over SSH in a reconnect
/// loop. Dropping a `JoinHandle` does not cancel the task, so a watcher that was
/// merely forgotten would notice its stream end and dial straight back out —
/// re-opening the connection that was just closed. The operator would see a
/// server that refuses to disconnect, with nothing naming the watcher.
///
/// So this asserts the task is *dead*, not merely unreferenced.
#[tokio::test]
async fn evicting_a_status_watcher_aborts_its_task() {
    let store = ClaudeStatusStore::default();
    let id = TargetId::Local;

    assert!(!store.evict_target(&id), "nothing is being watched yet");

    // **Cancellation is observed, not inferred.**
    //
    // The obvious version of this test — spawn a long sleep, then assert it did
    // not finish — passes whether or not anything was aborted, because a task
    // nobody cancelled has not finished either. It proves nothing.
    //
    // Aborting a task drops its future, so a guard living inside the future runs
    // its `Drop` exactly when the task is cancelled and never while it is merely
    // running. That is the difference this test has to see.
    struct Cancelled(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Cancelled {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = Cancelled(Arc::clone(&cancelled));
    let handle = tokio::spawn(async move {
        let _held = guard;
        // Stands in for the reconnect loop: it never ends on its own.
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    });

    store.insert_for_test(id.clone(), handle);
    assert!(store.evict_target(&id), "the watcher should be removed");

    // Give the runtime a moment to actually cancel it.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert!(
        cancelled.load(std::sync::atomic::Ordering::SeqCst),
        "the watcher task was dropped but never aborted — it is still running, \
         and the real one would be reconnecting to the host just disconnected"
    );
    assert!(!store.evict_target(&id), "and it is no longer registered");
}

/// **The disconnect evicts the stores the commands actually use.**
///
/// The stores moved into `rmux-app`'s `Ctx`; `server_disconnect` must reach
/// those very instances. A Tauri-managed second copy would make every count
/// above true of a cache nobody reads, while the connection stayed up.
#[tokio::test]
async fn the_disconnect_evicts_the_shared_ctx_stores() {
    struct NoHost;
    impl rmux_app::Host for NoHost {
        fn resource_dir(&self) -> Option<std::path::PathBuf> {
            None
        }
        fn data_dir(&self) -> anyhow::Result<std::path::PathBuf> {
            anyhow::bail!("none")
        }
        fn cache_dir(&self) -> anyhow::Result<std::path::PathBuf> {
            anyhow::bail!("none")
        }
        fn emit(&self, _: &str, _: serde_json::Value) {}
        fn secrets(&self) -> Option<&dyn rmux_app::SecretStore> {
            None
        }
    }

    let ctx = rmux_app::Ctx::new(Arc::new(NoHost));
    let id = TargetId::Local;
    ctx.store::<TerminalStore>().insert_for_test(id.clone(), local());
    ctx.store::<ClaudeStore>().insert_for_test(id.clone(), local());
    ctx.store::<FsStore>().insert_for_test(id.clone(), Arc::new(rmux_fs::LocalFs::new()));
    ctx.store::<MetricsStore>().insert_for_test(id.clone(), local()).await;
    ctx.store::<AgentStore>().insert_for_test(id.clone(), Arc::new(tokio::sync::OnceCell::new()));
    ctx.store::<ClaudeStatusStore>().insert_for_test(id.clone(), tokio::spawn(async {}));

    let target = rmux_app::TargetRef { host: None, user: None, port: None };
    let done = rmux_lib::server::disconnect(&ctx, target.clone()).await.unwrap();
    assert_eq!(done.evicted, 6, "every one of the six caches must let go");
    assert!(done.closed, "a held target was asked to close");

    let again = rmux_lib::server::disconnect(&ctx, target).await.unwrap();
    assert_eq!(again.evicted, 0, "a second disconnect evicts nothing");
}
