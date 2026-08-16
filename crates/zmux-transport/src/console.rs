//! Stopping child processes from opening a console window on Windows.
//!
//! ## The bug this exists for
//!
//! zmux is a GUI process, and a GUI process on Windows has **no console**. When
//! it spawns a console-subsystem program — and `ssh.exe` is one — the kernel
//! allocates a fresh console for the child and shows it. The operator sees a
//! `cmd`-looking window appear and vanish.
//!
//! On its own that would be ugly. What makes it unusable is how often zmux
//! spawns `ssh`: Windows OpenSSH has no `ControlMaster` (see
//! [`crate::Platform`] and `zmux_ssh::mux`), so **every** command opens its own
//! connection, and the app polls. Host metrics run every two seconds, the status
//! watch every 1.5, the transcript every five — each one a new process, so each
//! one a window flashing over whatever the operator is doing. Reported from a
//! real machine as "it constantly spawn and close cmd window, make the app
//! unusable", and that is exactly what it is.
//!
//! `CREATE_NO_WINDOW` is the fix: the child still gets a console, so its pipes
//! and its exit code behave identically, but the console has no window.
//!
//! ## Why a trait rather than a helper function
//!
//! Both `std::process::Command` and `tokio::process::Command` are spawned in
//! this workspace, they are unrelated types, and the flag has to be set on
//! whichever one a call site happens to hold. A trait means the call site reads
//! the same either way, which is what lets `no_console_window` be applied by
//! rule — *every* `Command` we spawn — rather than case by case.
//! `src-tauri/tests/no_console_window.rs` enforces that rule over the source.
//!
//! ## What it deliberately does not cover
//!
//! Terminals do not come through here. They are spawned into a pty by
//! `portable-pty`, which on Windows means ConPTY: the child is attached to a
//! pseudoconsole and the `conhost` behind it is headless, so no window is
//! created and there is no flag for us to set.

/// Windows' `CREATE_NO_WINDOW` process creation flag.
///
/// Spelled out rather than pulled from `windows-sys` for one constant, and it is
/// stable ABI — `winbase.h` has defined it as `0x08000000` since Windows 2000.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Spawn without letting Windows put a console window on screen.
///
/// A no-op everywhere else, so call sites need no `cfg`.
pub trait NoConsoleWindow {
    fn no_console_window(&mut self) -> &mut Self;
}

impl NoConsoleWindow for std::process::Command {
    fn no_console_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

impl NoConsoleWindow for tokio::process::Command {
    fn no_console_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag must not change what a command *does* — only whether a window
    /// appears. If it did, every probe in the app would start lying.
    #[tokio::test]
    async fn output_and_status_are_unaffected() {
        let mut cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/c", "echo still-here& exit 7"]);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", "echo still-here; exit 7"]);
            c
        };

        let out = cmd.no_console_window().output().await.unwrap();
        assert_eq!(out.status.code(), Some(7));
        assert!(String::from_utf8_lossy(&out.stdout).contains("still-here"));
    }

    #[test]
    fn the_flag_is_the_documented_value() {
        // Wrong by one bit is `DETACHED_PROCESS`, which would take the child's
        // console away entirely rather than hiding it.
        #[cfg(windows)]
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    }
}
