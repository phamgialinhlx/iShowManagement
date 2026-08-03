//! The askpass bridge, on platforms that have no Unix domain sockets.
//!
//! Deliberately a stub and not a port, for the same reason as the control
//! socket — and with a sharper edge, because this socket dispenses credentials
//! the operator types.
//!
//! Two of the three guards on the real server are filesystem permissions: a
//! `0700` directory holding a `0600` socket. Windows has neither, and
//! `restrict_to_owner` there is already a no-op — so a naive port would keep the
//! *shape* of the security model while silently leaving only the token behind
//! it. That is the fail-open failure this codebase keeps writing rules against.
//! A named pipe can carry a proper DACL, but through a different API with a
//! different set of mistakes available, and it has to be got right before it
//! ships, not after.
//!
//! Nothing else is lost by leaving it out. Key-based hosts — the common case —
//! are untouched, and with no helper registered [`super::env_for_gui_prompts`]
//! tells `ssh` not to wait for a terminal it will never be given, so a
//! password or 2FA host fails immediately with a reason instead of hanging.
//! `start` failing is already the caller's expected path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Prompt;

/// Answers a prompt, or `None` if the user dismissed it.
pub type Answerer = Arc<dyn Fn(Prompt) -> super::BoxFuture<Option<String>> + Send + Sync>;

/// A listening askpass socket — never actually constructed on this platform.
#[derive(Debug)]
pub struct AskpassServer {
    socket: PathBuf,
    token: String,
}

impl AskpassServer {
    pub async fn start(_answerer: Answerer) -> anyhow::Result<Self> {
        anyhow::bail!(
            "the askpass bridge needs Unix domain sockets, which this platform does not have — \
             key-based hosts are unaffected; password and 2FA hosts will fail fast rather than hang"
        )
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}
