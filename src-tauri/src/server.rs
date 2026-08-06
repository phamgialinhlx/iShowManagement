//! Server-level IPC: disconnecting a server (drop the SSH connection) and
//! listing sessions already running on it.

use tauri::State;

use crate::agent::AgentStore;
use crate::claude::ClaudeStore;
use crate::terminal::{TargetRef, TerminalStore};

/// Disconnect a server: drop the cached SSH target(s) so the ControlMaster
/// closes. Sessions keep running on the host. A `LocalTarget` is a no-op.
#[tauri::command]
pub async fn server_disconnect(
    terminal: State<'_, TerminalStore>,
    claude: State<'_, ClaudeStore>,
    agent: State<'_, AgentStore>,
    target: TargetRef,
) -> Result<(), String> {
    let id = target.id();
    // Forget the provisioning cache so a later use re-probes rather than
    // trusting a stale install, then drop the SSH targets from both stores.
    agent.forget(&id);
    terminal.evict_target(&id);
    claude.evict_target(&id);
    Ok(())
}
