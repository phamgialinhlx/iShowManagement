//! rmux's commands, with no GUI shell attached.
//!
//! **One command table drives both the desktop and the web** (`docs/rmux-web.md`).
//! A command lives here as an ordinary async function plus a line in its
//! module's `register`; the desktop app dispatches it from its Tauri invoke
//! handler, and rmux-web dispatches the very same entry from HTTP. Neither shell
//! re-declares it, so a command added here is live in both with no other edit.
//!
//! The four seams:
//!
//! - [`Registry`] — command name → handler over typed, `camelCase` args.
//! - [`Host`] — what a command needs from the shell around it: directories,
//!   events, secrets.
//! - [`Sink`] — a stream back to the UI (terminal bytes, JSON lifecycle).
//! - [`Ctx`] — the [`Host`] plus every module's long-lived store.
//!
//! **This crate must never depend on `tauri`.** It is cross-compiled to
//! linux-musl for a headless server; a test in the workspace (`cargo tree -p
//! rmux-app`) is the check.
//!
//! A moved command is a **move**, not a rewrite: identical behaviour, identical
//! error strings. Everything in `CLAUDE.md` holds inside this crate unchanged —
//! `shell_quote` on remote lines, no `if is_local` in feature code, secrets
//! never in argv, `no_console_window()` on every spawn.

mod ctx;
mod host;
mod registry;

pub mod agent;
pub mod bench;
pub mod children;
pub mod claude;
pub mod claude_account;
pub mod claude_status;
pub mod codex;
pub mod commands;
pub mod dsh;
pub mod files;
pub mod git;
pub mod metrics;
pub mod model_profile;
pub mod paste;
pub mod secrets;
pub mod services;
pub mod ssh_keys;
pub mod target;
pub mod terminal;
pub mod worktree;

pub use ctx::Ctx;
pub use host::{Host, SecretStore, Sink, SinkResolver};
pub use registry::{BoxFuture, Call, Command, NoArgs, Registry, Web};
pub use target::{TargetInfo, TargetRef};

/// The full command table: every module's `register`, in one place.
///
/// A module adds its commands in its own `register`; this function is the only
/// list, and it is a list of *modules*, not of commands — so adding a command
/// never means editing it.
pub fn registry() -> Registry {
    let mut r = Registry::new();
    commands::register(&mut r);
    git::register(&mut r);
    terminal::register(&mut r);
    files::register(&mut r);
    ssh_keys::register(&mut r);
    metrics::register(&mut r);
    services::register(&mut r);
    children::register(&mut r);
    claude::register(&mut r);
    claude_status::register(&mut r);
    codex::register(&mut r);
    paste::register(&mut r);
    model_profile::register(&mut r);
    claude_account::register(&mut r);
    dsh::register(&mut r);
    r
}
