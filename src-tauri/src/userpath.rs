//! Giving the app the `PATH` the operator actually has.
//!
//! ## The bug this exists for
//!
//! A macOS `.app` launched from Finder does not inherit a shell's environment.
//! It gets launchd's, and unless someone has run `launchctl setenv PATH` — nobody
//! has — that is `/usr/bin:/bin:/usr/sbin:/sbin`. Homebrew's `/opt/homebrew/bin`
//! is not in it. Neither is `/usr/local/bin`, `~/.local/bin`, or wherever a
//! version manager puts things.
//!
//! rmux shells out to `ssh` and lets OpenSSH read `~/.ssh/config` verbatim —
//! which is the whole design, and it is what makes `ProxyJump`, certificates and
//! FIDO keys work for free. But a `ProxyCommand` is a *command*, and it is
//! resolved against the PATH of the process that spawned `ssh`. So a host like
//!
//! ```text
//! Host gateway
//!     ProxyCommand cloudflared access ssh --hostname %h
//! ```
//!
//! connects perfectly from Terminal and fails from the app with
//! `exit status: 255`, because `cloudflared` lives in `/opt/homebrew/bin` and the
//! app cannot see it. Measured on this machine: `launchctl getenv PATH` is empty,
//! and `cloudflared` exists at `/opt/homebrew/bin/cloudflared` and nowhere else.
//!
//! The same trap catches every other ProxyCommand helper — `boringproxy`,
//! `corkscrew`, `gh`, an `aws ssm` wrapper — and it is invisible, because the
//! command the operator would run to check it works.
//!
//! ## Why it asks the login shell instead of guessing
//!
//! Prepending a list of likely directories would fix Homebrew and miss
//! everything else, and the list would be wrong on someone's machine the day it
//! was written. The login shell already knows the answer: it is the same shell
//! that produced the PATH under which `ssh` works when typed by hand.
//!
//! This is the same reasoning that stops rmux resolving `~/.ssh/config` itself.
//! Ask the thing that knows.
//!
//! ## Why it is set on the process rather than per command
//!
//! `ssh` is spawned from several places — the control master, port forwards, and
//! the PTY behind every terminal — and a `ProxyCommand` has to resolve in all of
//! them. Setting it once, before anything is spawned, is one fix rather than one
//! per call site plus the next call site somebody adds.

use std::process::Command;

/// Read the login shell's `PATH`.
///
/// `-l` for a login shell, because that is what reads `.zprofile` /
/// `.bash_profile` where PATH is set. **Not** `-i`: an interactive shell may
/// print a banner, ask something, or take a noticeable moment on a machine with
/// a heavy `.zshrc`, and this runs during startup.
fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty())?;

    let out = Command::new(&shell)
        .arg("-l")
        .arg("-c")
        // `printf` rather than `echo`, so nothing appends a newline that then has
        // to be trimmed out of a value that may legitimately contain anything.
        .arg("printf %s \"$PATH\"")
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if path.is_empty() { None } else { Some(path) }
}

/// Merge two `PATH`s, keeping order and dropping duplicates.
///
/// The login shell's entries go first — they are the ones the operator arranged
/// — but the inherited ones are kept rather than replaced. Dropping them could
/// remove a directory the app itself was launched with, and losing `/usr/bin`
/// because a shell profile is unusual would be a far worse failure than the one
/// being fixed.
pub fn merge(login: &str, inherited: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<&str> = Vec::new();

    for entry in login.split(':').chain(inherited.split(':')) {
        if entry.is_empty() {
            continue;
        }
        if seen.insert(entry) {
            out.push(entry);
        }
    }

    out.join(":")
}

/// Adopt the login shell's `PATH` for this process and everything it spawns.
///
/// Call once, early, before any thread or child process exists — which is what
/// makes writing the process environment sound.
///
/// Returns the PATH now in effect, for the log. A failure here is not fatal: the
/// app works, and only hosts needing a `ProxyCommand` helper outside the default
/// PATH are affected — so it is reported and stepped over rather than raised.
pub fn adopt_login_path() -> Option<String> {
    let inherited = std::env::var("PATH").unwrap_or_default();
    let login = login_shell_path()?;

    let merged = merge(&login, &inherited);
    if merged == inherited {
        return None;
    }

    // SAFETY: called from `run` before any other thread is started and before any
    // child process is spawned, so nothing can be reading the environment
    // concurrently.
    unsafe { std::env::set_var("PATH", &merged) };
    Some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_path_comes_first_and_the_inherited_one_survives() {
        let merged = merge("/opt/homebrew/bin:/usr/bin", "/usr/bin:/bin:/usr/sbin");
        assert_eq!(merged, "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin");
    }

    #[test]
    fn duplicates_are_dropped_rather_than_repeated() {
        // A PATH that names the same directory five times is what you get from
        // profiles that each prepend "just in case", and it makes every command
        // lookup slower for no benefit.
        let merged = merge("/a:/b:/a", "/b:/a:/c");
        assert_eq!(merged, "/a:/b:/c");
    }

    #[test]
    fn empty_entries_do_not_become_the_current_directory() {
        // A trailing colon means "the current directory" to the shell, which is
        // a real hazard when the process later chdirs into a project.
        assert_eq!(merge("/a::/b", ":/c:"), "/a:/b:/c");
    }

    #[test]
    fn the_homebrew_case_this_was_written_for() {
        // Measured: a Finder-launched .app gets exactly this, and `cloudflared`
        // is in /opt/homebrew/bin — so a ProxyCommand using it cannot resolve.
        let launchd = "/usr/bin:/bin:/usr/sbin:/sbin";
        let merged = merge("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin", launchd);
        assert!(merged.starts_with("/opt/homebrew/bin:"), "{merged}");
        assert!(merged.split(':').any(|p| p == "/sbin"), "must not lose the inherited ones");
    }
}
