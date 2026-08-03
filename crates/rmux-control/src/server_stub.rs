//! The control socket, on platforms that have no Unix domain sockets.
//!
//! Deliberately a stub and not a port. The real server's security model *is*
//! the filesystem — a `0700` directory holding a `0600` socket, plus a
//! per-run token — and Windows has neither of those primitives. A named pipe
//! can be secured properly, but through a different API with a different set of
//! mistakes available, and getting that wrong would expose an interface that
//! hands out session control.
//!
//! Nothing the workbench does needs this: it exists so a separate browser app
//! can drive rmux. So on Windows `start` reports plainly that it is unavailable,
//! the caller logs it and carries on — which is what the caller already does
//! when the socket cannot be created for any other reason.

//! ## It must mirror the real API exactly, name for name
//!
//! A stub nothing on this machine compiles is a stub that is wrong. The first
//! version of this one declared `handle` as a hand-rolled RPITIT while the real
//! trait is `#[async_trait]`, and named the event method `publish` where callers
//! say `emit` — four compile errors that no macOS or Linux build could ever
//! show, and that surfaced only after a *different* Windows fix let the build
//! get far enough to reach this crate's consumer. Change `server.rs`'s public
//! surface and you must change this file in the same commit.

use std::path::{Path, PathBuf};

use crate::protocol::{Event, Request, Response};

/// Mirrors `server::Handler`, `async_trait` and all — an `async fn` in an impl
/// desugars to a lifetime bound to `&self`, which a plain
/// `-> impl Future` declaration does not match (E0195).
#[async_trait::async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn handle(&self, request: Request) -> Response;
}

pub struct ControlServer {
    socket: PathBuf,
    token: String,
}

impl ControlServer {
    pub async fn start<H: Handler>(_handler: std::sync::Arc<H>) -> anyhow::Result<Self> {
        anyhow::bail!(
            "the rmux control socket needs Unix domain sockets, which this platform does not \
             have — everything else works; only external apps driving rmux are unavailable"
        )
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Unreachable — `start` never returns a value to call this on.
    pub fn emit(&self, _event: Event) {}
}
