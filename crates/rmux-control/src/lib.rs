//! rmux as a backend.
//!
//! rmux owns the expensive, stateful things: ssh connections and their
//! multiplexed masters, PTYs that outlive the app, port forwards, Claude
//! sessions, credentials. Every one of those is something a second app must not
//! duplicate — two apps independently forwarding port 3000 collide on the bind,
//! and two apps each holding their own ssh master to the same host doubles the
//! connections for no gain.
//!
//! So other apps in the ecosystem do not reimplement any of it. They connect
//! here, follow rmux's session state, and ask rmux to do the work.
//!
//! ## Why a unix socket and not a localhost port
//!
//! A TCP listener on localhost is reachable by **every process on the machine**,
//! including a web page's `fetch` in some configurations. A unix socket is a
//! filesystem object with an owner and a mode, so `0600` in a `0700` directory
//! means the OS refuses other users before rmux sees a byte. The same reasoning
//! as the askpass bridge, and for the same reason: this socket can open ssh
//! tunnels into the operator's infrastructure.
//!
//! The token on top of that is not belt-and-braces. On macOS the temporary
//! directory is per-user but a *sandboxed* process of the same user could still
//! reach it, and the token means a client must have been able to read a
//! `0600` file to say anything at all.

pub mod protocol;
pub mod server;

pub use protocol::{
    ConsoleEntry, Event, Message, Report, Request, Response, SessionInfo, Viewport, VERSION,
};
pub use server::{ControlServer, Handler};

use std::path::PathBuf;

/// Where clients look for the socket and its token.
///
/// A fixed, predictable path — a client that has to be *told* where to connect
/// needs configuration, and configuration for something both halves already
/// know is friction with no benefit.
pub fn runtime_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::runtime_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(base.join(".rmux"))
}

pub fn socket_path() -> anyhow::Result<PathBuf> {
    Ok(runtime_dir()?.join("control.sock"))
}

/// The handshake file: `{"socket": "...", "token": "...", "version": 1}`.
///
/// Separate from the socket because a client needs the token *before* it can
/// connect, and a socket cannot hand out a secret to an unauthenticated peer
/// without defeating the point of having one.
pub fn handshake_path() -> anyhow::Result<PathBuf> {
    Ok(runtime_dir()?.join("control.json"))
}
