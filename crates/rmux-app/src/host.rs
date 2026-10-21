//! What a command needs from the shell around it.

use std::path::PathBuf;
use std::sync::Arc;

/// The environment a command runs in: the desktop app or rmux-web.
///
/// Desktop implements it over Tauri's `AppHandle`; the web server over plain
/// Linux paths. A command must reach the outside world only through this (or a
/// [`Sink`]), never by assuming which shell it is in — that is the same rule as
/// "no `if is_local` in feature code", one level up.
pub trait Host: Send + Sync + 'static {
    /// Bundled resources (the cross-compiled agents). `None` where the shell
    /// ships none, which callers already treat as "no persistence, with a reason".
    fn resource_dir(&self) -> Option<PathBuf>;

    /// Per-install data directory (app data on desktop).
    fn data_dir(&self) -> anyhow::Result<PathBuf>;

    /// Per-install cache directory.
    fn cache_dir(&self) -> anyhow::Result<PathBuf>;

    /// Broadcast an event to every UI attached to this shell (Tauri's
    /// `Emitter::emit` on desktop). Fire-and-forget: a UI that is not listening
    /// is not an error.
    fn emit(&self, event: &str, payload: serde_json::Value);

    /// The credential store, or `None` where there is none (a headless host
    /// with no keychain). A command that needs a secret must say so rather than
    /// fall back to somewhere less safe.
    fn secrets(&self) -> Option<&dyn SecretStore>;
}

/// A credential store addressed by `(service, account)` — the keychain on
/// desktop. Modelled on `keyring::Entry`.
///
/// Values are returned to Rust and never written into argv or a shell line;
/// they reach a host by stdin (`rmux-agent setenv`), per `CLAUDE.md`.
pub trait SecretStore: Send + Sync {
    /// The stored value, or `Ok(None)` when nothing is stored there. Only a
    /// store that could not be *read* is an error.
    fn get(&self, service: &str, account: &str) -> anyhow::Result<Option<String>>;

    /// Store `value`, replacing any previous one.
    fn set(&self, service: &str, account: &str, value: &str) -> anyhow::Result<()>;

    /// Remove the entry. Removing an entry that does not exist is success.
    fn delete(&self, service: &str, account: &str) -> anyhow::Result<()>;
}

/// A stream back to the UI.
///
/// Desktop resolves it to a Tauri `Channel` on the invoking webview; the web
/// server to frames on the page's WebSocket, multiplexed by channel id.
pub trait Sink: Send + Sync {
    /// Raw bytes, unencoded — terminal output. On desktop this is
    /// `InvokeResponseBody::Raw`, which reaches JS as an `ArrayBuffer`; never a
    /// JSON array of numbers (4x the size, on the hottest path in the app).
    fn send_bytes(&self, bytes: Vec<u8>) -> anyhow::Result<()>;

    /// One JSON message.
    fn send_json(&self, value: serde_json::Value) -> anyhow::Result<()>;
}

/// Turns a channel id the UI sent as an argument into a [`Sink`].
///
/// Supplied by the transport per call, because a channel belongs to the
/// webview (or WebSocket) that invoked the command.
pub trait SinkResolver: Send + Sync {
    fn resolve(&self, channel_id: &str) -> anyhow::Result<Arc<dyn Sink>>;
}
