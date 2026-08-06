//! `evict_target` / `forget` must drop a cached target so the ControlMaster
//! closes and the next resolve reconnects. Tested against the store maps.

use std::sync::Arc;
use rmux_transport::TargetId;
use rmux_transport::local::LocalTarget;
use tokio::sync::OnceCell;

// This is an integration test in its own crate, so the store comes from the
// `rmux_lib` library crate. The `TerminalStore` / `ClaudeStore` / `AgentStore`
// types are `pub`; the accessors must be `pub` too for the test to reach them.
use rmux_lib::agent::AgentStore;
use rmux_lib::claude::ClaudeStore;
use rmux_lib::terminal::TerminalStore;

// LocalTarget is a cheap real target that needs no network to construct, and
// evicting it proves the map entry is gone without an SSH round trip.

#[test]
fn terminal_evict_target_removes_entry() {
    // The store starts empty: evicting an absent id is a no-op.
    let store = TerminalStore::default();
    let id = TargetId::Local;
    assert!(!store.evict_target(&id));

    // Insert a target, evict it, and assert the entry is gone.
    store.insert_for_test(TargetId::Local, Arc::new(LocalTarget::new()));
    assert!(store.evict_target(&id));
    assert!(!store.evict_target(&id)); // already gone — no-op
}

#[test]
fn claude_evict_target_removes_entry() {
    let store = ClaudeStore::default();
    let id = TargetId::Local;
    assert!(!store.evict_target(&id));

    store.insert_for_test(TargetId::Local, Arc::new(LocalTarget::new()));
    assert!(store.evict_target(&id));
    assert!(!store.evict_target(&id));
}

#[test]
fn agent_forget_removes_entry() {
    let store = AgentStore::default();
    let id = TargetId::Local;
    assert!(!store.forget(&id));

    store.insert_for_test(TargetId::Local, Arc::new(OnceCell::new()));
    assert!(store.forget(&id));
    assert!(!store.forget(&id));
}
