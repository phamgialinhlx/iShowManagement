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

use rmux_app::TargetRef;
use rmux_app::agent::AgentStore;
use rmux_app::claude::ClaudeStore;
use rmux_app::claude_status::ClaudeStatusStore;
use rmux_app::files::FsStore;
use rmux_app::metrics::MetricsStore;
use rmux_app::terminal::TerminalStore;

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
///
/// The six stores are the ones in the shared `Ctx` — the very instances the
/// commands in `rmux-app` cache into. Evicting any other copy would report a
/// disconnect while the connection stayed up.
#[tauri::command]
pub async fn server_disconnect<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    target: TargetRef,
) -> Result<Disconnected, String> {
    let ctx = crate::bridge::ctx(&app);
    disconnect(&ctx, target).await
}

/// [`server_disconnect`] against a [`rmux_app::Ctx`] directly — what the
/// command does, reachable from a test without a Tauri app.
pub async fn disconnect(ctx: &rmux_app::Ctx, target: TargetRef) -> Result<Disconnected, String> {
    let terminal = ctx.store::<TerminalStore>();
    let claude = ctx.store::<ClaudeStore>();
    let files = ctx.store::<FsStore>();
    let metrics = ctx.store::<MetricsStore>();
    let agent = ctx.store::<AgentStore>();
    let status = ctx.store::<ClaudeStatusStore>();

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
    target: TargetRef,
    key: String,
    alias: String,
) -> Result<(), String> {
    let ctx = crate::bridge::ctx(&app);
    let store = ctx.store::<ClaudeStore>();
    let resolved = rmux_app::claude::resolve(&store, &target).await?;
    let installed = rmux_app::agent::ensure_agent(&ctx, resolved.as_ref()).await?;

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
