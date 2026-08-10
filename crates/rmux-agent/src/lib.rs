//! The rmux session agent.
//!
//! A small daemon that owns terminal sessions and outlives the connections that
//! use them, so a shell survives losing the network, closing the laptop, or
//! quitting rmux.
//!
//! It is **not** a terminal multiplexer. It performs no emulation: no grid, no
//! scrollback rendering, no copy mode, no opinion about the cursor. It moves raw
//! bytes and buffers recent output for replay. Everything you can see is drawn
//! by the terminal you are actually looking at — which is why scrolling and
//! selection behave normally rather than like a program pretending to be a
//! terminal inside your terminal.
//!
//! The same binary serves local and remote sessions, so a session resumes the
//! same way either way.

pub mod alias;
pub mod attach;
pub mod daemon;
pub mod ipc;
pub mod protocol;
pub mod provision;
pub mod status;
pub mod tty;

pub use protocol::{Frame, Hello};
