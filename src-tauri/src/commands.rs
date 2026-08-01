//! IPC commands.
//!
//! Errors cross the IPC boundary as strings because `anyhow::Error` is not
//! serialisable. Each handler converts at the edge so the crates underneath can
//! keep using `anyhow` internally.

use rmux_transport::{LocalTarget, Target, Tty};

use crate::{TargetInfo, describe, spec};

/// Describe the local machine.
#[tauri::command]
pub async fn local_target() -> TargetInfo {
    describe(&LocalTarget::new())
}

/// Run a one-shot command on a target and return its stdout.
///
/// Interactive work does not come through here — terminals get a PTY, which is a
/// separate channel. This is for the short probes the UI needs (`git branch`,
/// `uname`, and so on).
#[tauri::command]
pub async fn run_on_target(program: String, args: Vec<String>) -> Result<String, String> {
    let target = LocalTarget::new();
    let output = target
        .exec(&spec(&program, &args).tty(Tty::None))
        .await
        .map_err(|e| e.to_string())?;

    output.stdout_or_err().map(str::to_owned).map_err(|e| e.to_string())
}
