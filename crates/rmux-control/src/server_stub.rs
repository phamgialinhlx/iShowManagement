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

use std::path::PathBuf;

use crate::protocol::Event;

pub trait Handler: Send + Sync + 'static {
    fn handle(
        &self,
        request: crate::protocol::Request,
    ) -> impl std::future::Future<Output = crate::protocol::Response> + Send;
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

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn publish(&self, _event: Event) {}
}
