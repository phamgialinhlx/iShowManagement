//! The ssh tunnels rmux owns on behalf of its clients.
//!
//! **Nothing in the app uses these any more.** They backed an in-app browser
//! tab, which is gone: rmux's webview is a single WKWebView that cannot be given
//! a per-session proxy, so every page it showed needed a forwarded port first —
//! the manual step the feature existed to remove. A separate Chromium app can
//! scope a SOCKS proxy per session, which is the arrangement that actually
//! delivers "no port forwarding", so the tunnels now serve `rmux-control`.
//!
//! They stay owned here, in one place, for the reason they always were: rmux
//! already holds the ssh connections, and two clients independently forwarding
//! the same port would collide on the bind — with the loser reporting a failure
//! it has no way to explain.

use std::sync::Arc;

use rmux_ssh::forward::Forwards;
use rmux_transport::TargetId;

#[derive(Default)]
pub struct TunnelStore {
    forwards: Arc<Forwards>,
}

impl TunnelStore {
    pub fn forwards(&self) -> Arc<Forwards> {
        Arc::clone(&self.forwards)
    }
}

/// The connection string the forward should dial — the **full `user@host`
/// label**, not the bare host.
///
/// The forward runs its own ssh (`ControlPath=none`) and offers the host's key
/// by deriving its filename from this string (`~/.ssh/rmux_<label>_ed25519`).
/// Passing the bare host would look for `rmux_192.168.1.5_ed25519` while the key
/// is `rmux_yitec_192.168.1.5_ed25519`, so the key would not be found and a
/// key-only host would reject the tunnel with `Permission denied`. This is the
/// same label the master authenticates with (`SshHostId::label`). `None` for a
/// local target, which needs no tunnel.
fn conn(target: &crate::terminal::TargetRef) -> Option<String> {
    match target.id() {
        TargetId::Ssh(host) => Some(host.label()),
        TargetId::Local => None,
    }
}

/// What the target is listening on.
///
/// Discovered rather than typed, because requiring the number up front leaves
/// the manual step this whole feature exists to remove. Ports below 1024 are
/// filtered out by `parse_listening` — they are the machine's own services, not
/// the app the operator just started.
#[tauri::command]
pub async fn ports_discover(
    claude_store: tauri::State<'_, crate::claude::ClaudeStore>,
    target: crate::terminal::TargetRef,
) -> Result<Vec<rmux_ssh::forward::ListeningPort>, String> {
    let resolved = crate::claude::resolve(&claude_store, &target).await?;

    let out = resolved
        .exec(
            &rmux_transport::CommandSpec::new("sh")
                .arg("-c")
                .arg(rmux_ssh::forward::DISCOVER_SCRIPT)
                .tty(rmux_transport::Tty::None),
        )
        .await
        .map_err(|e| e.to_string())?;

    // A host with neither `ss` nor `netstat` is not an error — it has nothing
    // to report, and a port can still be forwarded by number.
    Ok(rmux_ssh::forward::parse_listening(out.stdout_or_err().unwrap_or("")))
}

/// Open `ssh -L port:localhost:port`, so `http://localhost:<port>` reaches it.
#[tauri::command]
pub async fn port_forward(
    store: tauri::State<'_, TunnelStore>,
    target: crate::terminal::TargetRef,
    port: u16,
) -> Result<rmux_ssh::forward::Forward, String> {
    Ok(store.forwards().start(conn(&target).as_deref(), port).await)
}

#[tauri::command]
pub async fn port_unforward(
    store: tauri::State<'_, TunnelStore>,
    target: crate::terminal::TargetRef,
    port: u16,
) -> Result<(), String> {
    store.forwards().stop(conn(&target).as_deref(), port).await;
    Ok(())
}

/// Every tunnel currently open for a target.
#[tauri::command]
pub async fn ports_forwarded(
    store: tauri::State<'_, TunnelStore>,
    target: crate::terminal::TargetRef,
) -> Result<Vec<rmux_ssh::forward::Forward>, String> {
    Ok(store.forwards().list(conn(&target).as_deref()).await)
}

/// A SOCKS proxy onto the target — every port at once, and its DNS too.
///
/// Exposed to the UI so the operator can see the port and point something at
/// it. rmux's own webview deliberately does not use it: there is one of it, and
/// proxying it would route the app's own interface through the operator's
/// server. See `Forwards::socks`.
#[tauri::command]
pub async fn port_proxy(
    store: tauri::State<'_, TunnelStore>,
    target: crate::terminal::TargetRef,
) -> Result<u16, String> {
    store.forwards().socks(conn(&target).as_deref()).await
}
