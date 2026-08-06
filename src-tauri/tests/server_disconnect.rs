//! `evict_target` / `forget` must drop a cached target so the ControlMaster
//! closes and the next resolve reconnects. Tested against the store maps.

use std::sync::Arc;
use rmux_transport::TargetId;
use rmux_transport::local::LocalTarget;

// This is an integration test in its own crate, so the store comes from the
// `rmux` library crate. The `TerminalStore` / `ClaudeStore` / `AgentStore`
// types are `pub`; the accessors must be `pub` too for the test to reach them
// (see the note below — this forces option 1).
use rmux_lib::terminal::TerminalStore;

// LocalTarget is a cheap real target that needs no network to construct, and
// evicting it proves the map entry is gone without an SSH round trip.

#[test]
fn evict_target_removes_entry() {
    // The store starts empty: evicting an absent id is a no-op (returns false).
    let store = TerminalStore::default();
    let id = TargetId::Local;
    assert_eq!(store.evict_target(&id), false);

    // Insert a target, evict it, and assert the entry is gone.
    store.insert_for_test(TargetId::Local, Arc::new(LocalTarget::new()));
    assert_eq!(store.evict_target(&id), true);
    assert_eq!(store.evict_target(&id), false); // already gone — no-op
}
