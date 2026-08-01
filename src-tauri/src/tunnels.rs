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

#[derive(Default)]
pub struct TunnelStore {
    forwards: Arc<Forwards>,
}

impl TunnelStore {
    pub fn forwards(&self) -> Arc<Forwards> {
        Arc::clone(&self.forwards)
    }
}
