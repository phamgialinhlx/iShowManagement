//! The benchmark-logging switch.
//!
//! Moved from `src-tauri/src/logs.rs` so the commands that emit `BENCH` lines
//! (metrics, today) can live here; the desktop's `logs` module re-exports these
//! three and keeps the `set_debug_logging` / `log_event` commands that drive them.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether benchmark (`BENCH …`) lines are emitted.
///
/// Off by default. The file log already runs at `rmux=debug`, so a gated
/// `tracing::info!` under this crate's target lands in the exportable log the
/// moment it is switched on — no filter reload, no restart. The switch lives in
/// Settings › Diagnostics and is synced here from the webview.
static DEBUG: AtomicBool = AtomicBool::new(false);

pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

pub fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

/// Emit one benchmark line, but only while debug logging is on.
///
/// The `BENCH ` prefix is the grep handle. The target is pinned to
/// `rmux_lib::logs` — where these lines were emitted before the move — so an
/// existing filter or a grep on the exported log keeps matching them.
pub fn bench(msg: &str) {
    if debug_enabled() {
        tracing::info!(target: "rmux_lib::logs", "BENCH {msg}");
    }
}
