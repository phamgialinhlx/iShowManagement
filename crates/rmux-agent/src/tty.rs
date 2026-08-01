//! The attach client's own terminal.
//!
//! `rmux-agent attach` runs with a TTY of its own — `ssh -tt` allocates one, and
//! so does a local PTY. That terminal sits *between* rmux's xterm and the shell
//! the daemon owns, and by default it is a full participant: it echoes what it
//! receives, buffers by line, and interprets control characters.
//!
//! That is wrong for every byte here. This process is a pipe, not a terminal —
//! the real terminal is xterm.js at one end and the daemon's PTY at the other,
//! and both already do the interpreting. Leaving the middle one in cooked mode
//! shows up as input being echoed back as literal escape sequences: typing at
//! Claude produces `^[[<35;166;36M` on screen, because a mouse report round-trips
//! as text instead of passing through as data.
//!
//! So the client puts its terminal in raw mode for as long as it is attached, and
//! puts it back afterwards.

#[cfg(unix)]
mod imp {
    use std::os::fd::AsRawFd;

    /// Restores the terminal when dropped.
    ///
    /// A guard rather than a pair of calls: this process exits down several paths
    /// — the shell exits, the connection drops, an error bubbles up — and a
    /// terminal left in raw mode makes the *user's* shell unusable afterwards,
    /// with no echo and no line editing.
    pub struct RawMode {
        fd: i32,
        saved: libc::termios,
    }

    impl RawMode {
        /// Enter raw mode, if stdin is a terminal.
        ///
        /// `None` when it is not — a piped stdin has no line discipline to
        /// disable, which is the case in tests and in scripted use.
        pub fn enter() -> Option<Self> {
            let fd = std::io::stdin().as_raw_fd();
            // SAFETY: `fd` is stdin, valid for the life of the process.
            if unsafe { libc::isatty(fd) } != 1 {
                return None;
            }

            let mut saved: libc::termios = unsafe { std::mem::zeroed() };
            // SAFETY: `saved` is a valid, writable termios.
            if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
                return None;
            }

            let mut raw = saved;
            // SAFETY: `raw` is a valid termios.
            unsafe { libc::cfmakeraw(&mut raw) };
            // Block until at least one byte, with no inter-byte timer: this is a
            // relay, so it should wake per keystroke rather than poll.
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;

            // TCSANOW, not TCSAFLUSH: flushing would discard input the user has
            // already typed but which has not been read yet.
            // SAFETY: `raw` is a valid termios and `fd` is a terminal.
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
                return None;
            }

            Some(Self { fd, saved })
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // SAFETY: `self.saved` is what `tcgetattr` gave us for this fd.
            unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
        }
    }

    /// The terminal's current size, if it has one.
    pub fn window_size() -> Option<(u16, u16)> {
        let fd = std::io::stdin().as_raw_fd();
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: `size` is a valid winsize and `fd` is stdin.
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } != 0 {
            return None;
        }
        if size.ws_col == 0 || size.ws_row == 0 {
            return None;
        }
        Some((size.ws_col, size.ws_row))
    }
}

#[cfg(not(unix))]
mod imp {
    /// Windows console modes are set by the PTY layer, so there is nothing to do.
    pub struct RawMode;

    impl RawMode {
        pub fn enter() -> Option<Self> {
            None
        }
    }

    pub fn window_size() -> Option<(u16, u16)> {
        None
    }
}

pub use imp::{RawMode, window_size};
