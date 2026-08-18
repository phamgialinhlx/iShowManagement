//! Claude Code's interface, as data: where its records are, how you launch it,
//! and which keystrokes drive it.
//!
//! Split out of `rmux-claude` because there are now two consumers and only one
//! of them can afford that crate. `rmux-claude` *drives* a Claude process — it
//! holds a PTY, scrapes the screen with `alacritty_terminal`, and calls
//! Anthropic's usage API through `reqwest`. **`rmux-agent` does none of that**,
//! and it is a static musl binary uploaded to every host rmux touches, where
//! several megabytes of unreachable TLS and terminal-emulator code would be paid
//! for on the wire by every operator on every version bump.
//!
//! What is left here needs nothing but `serde` and `shell_quote`, and it is the
//! half the two genuinely share. The agent uses it to serve Redstone from a
//! host; the desktop app uses it over ssh. **One definition, so the two cannot
//! disagree** — about what a conversation contains, about how Claude is
//! launched, or about how a message is submitted. Each of those has already cost
//! a real bug when it existed twice.
//!
//! Four rules govern everything in here, all learned the expensive way and each
//! restated on the item it applies to:
//!
//! 1. **Only ever read the tail.** A real transcript has been measured at
//!    **228 MB** on a working server, and the widget that reads it polls.
//! 2. **Never bind to Claude's schema.** Fields are picked out one at a time and
//!    unknown record types are skipped; a strict deserialise turns every Claude
//!    Code release into "the transcript is empty".
//! 3. **Claude's project-directory name is not computable.** The scheme changed
//!    between versions, so every spelling is tried and then the `cwd` each
//!    transcript records is used as the authority.
//! 4. **Send text and Enter as separate writes.** Appending `\r` to a message
//!    makes the TUI treat the whole thing as one bracketed chunk and swallow the
//!    newline, so the message sits in the composer, typed but never sent.

pub mod keys;
pub mod launch;
pub mod pi;
pub mod sessions;
pub mod transcript;

pub use launch::{launch_line, Rendering};
pub use sessions::SessionInfo;
pub use transcript::{Entry, Speaker, Status, Transcript, Usage};
