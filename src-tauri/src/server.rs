//! Dropping a server's connection without ending its work.
//!
//! rmux caches a resolved `Arc<dyn Target>` per host in six separate places, and
//! until now nothing ever removed one. The SSH ControlMaster therefore lived for
//! the whole run of the app: connect to a host once and you were connected to it
//! until you quit, whether or not anything was still using it.
//!
//! That matters more than tidiness. A laptop that has moved networks, a VPN that
//! dropped, a jump host that was rebooted — each leaves a master that is up as
//! far as rmux is concerned and dead as far as the network is concerned, and
//! every command through it hangs until its own timeout.
//!
//! **Disconnecting is not closing sessions.** The shells and Claude runs belong
//! to `rmux-agent` on the far side; they keep going with nobody attached, which
//! is the whole point of the agent. This closes the pipe, not the work.

use tauri::State;

use crate::agent::AgentStore;
use crate::claude::ClaudeStore;
use crate::claude_status::ClaudeStatusStore;
use crate::files::FsStore;
use crate::metrics::MetricsStore;
use crate::terminal::{TargetRef, TerminalStore};

/// What a disconnect actually did.
///
/// Reported rather than assumed. "Disconnect" that silently no-ops on a host
/// nothing was connected to reads as a broken button, and the operator's next
/// move is to press it again.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Disconnected {
    /// How many of the six caches were holding this host.
    pub evicted: usize,
    /// Whether a transport was asked to close. False for a local target, which
    /// has none.
    pub closed: bool,
}

/// Close a server's connection and forget it, leaving its sessions running.
///
/// ## Order matters
///
/// The target is taken **before** anything is evicted, because after eviction
/// there is nothing left to ask to disconnect. Then every cache is dropped, and
/// only then is the transport closed — so nothing can hand out the connection
/// between the close and the forget.
///
/// ## Why all six, and why the count is returned
///
/// Each store caches independently, and one survivor is enough to keep the
/// master alive. A disconnect that half-worked looks exactly like one that did
/// nothing, so the count is surfaced rather than discarded.
#[tauri::command]
pub async fn server_disconnect(
    terminal: State<'_, TerminalStore>,
    claude: State<'_, ClaudeStore>,
    files: State<'_, FsStore>,
    metrics: State<'_, MetricsStore>,
    agent: State<'_, AgentStore>,
    status: State<'_, ClaudeStatusStore>,
    target: TargetRef,
) -> Result<Disconnected, String> {
    let id = target.id();

    // Any store that has it will do — they all hold the same resolved target.
    let held = claude.cached_target(&id).or_else(|| terminal.cached_target(&id));

    let mut evicted = 0;
    // The status watcher goes first and is *aborted*, not merely dropped. It
    // reconnects on its own when its stream ends, so leaving it running would
    // re-open the connection the rest of this function is closing.
    if status.evict_target(&id) {
        evicted += 1;
    }
    if terminal.evict_target(&id) {
        evicted += 1;
    }
    if claude.evict_target(&id) {
        evicted += 1;
    }
    if files.evict_target(&id) {
        evicted += 1;
    }
    if metrics.evict_target(&id).await {
        evicted += 1;
    }
    if agent.forget(&id) {
        evicted += 1;
    }

    let closed = match held {
        Some(target) => {
            // `ssh -O exit`, not a hopeful drop. See `Target::disconnect`.
            target.disconnect().await;
            true
        }
        None => false,
    };

    Ok(Disconnected { evicted, closed })
}

/// Rename a session on the host.
///
/// The name a *rail row* shows is local to this machine, which is fine until two
/// people — or one person and their other laptop — look at the same host and see
/// `term-01K9…` for something that was named hours ago. This writes the name
/// where every client can read it.
///
/// It is an **alias**, never a rename of the key: the key is how the daemon holds
/// the session and how every client reattaches, so changing it would orphan
/// running work.
#[tauri::command]
pub async fn server_alias<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, ClaudeStore>,
    target: TargetRef,
    key: String,
    alias: String,
) -> Result<(), String> {
    let resolved = crate::claude::resolve(store.inner(), &target).await?;
    let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;

    let spec = rmux_transport::CommandSpec::new(&installed.program)
        .arg("alias")
        .arg(&key)
        .arg(&alias)
        .tty(rmux_transport::Tty::None);

    let out = resolved.exec(&spec).await.map_err(|e| e.to_string())?;
    if out.status != 0 {
        // The agent explains itself on stderr — "already running", "no running
        // session". Passing that through beats inventing a message here, since
        // the rules live over there.
        let reason = out.stderr.trim();
        return Err(if reason.is_empty() {
            format!("rename failed (status {})", out.status)
        } else {
            reason.to_owned()
        });
    }
    Ok(())
}
