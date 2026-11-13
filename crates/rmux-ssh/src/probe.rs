//! "Test connection" — one cheap `uname -s` with exactly the settings the
//! operator is about to save.
//!
//! **Its own connection, never the master's.** `ControlPath=none`, because the
//! question is whether *these* settings authenticate: riding an existing master
//! would answer yes for a host that was authenticated an hour ago with a
//! different key, which is a test that cannot fail. It also leaves nothing
//! behind — no master, no entry in the chosen-identity table — so testing a key
//! and then cancelling changes nothing.
//!
//! Prompts still work: the askpass bridge is in the environment, so a password
//! or 2FA host shows its dialog exactly as a real connection would.

use std::path::Path;
use std::time::Duration;

use rmux_transport::{NoConsoleWindow, SshHostId};

use crate::keys;

/// How long the whole probe may take, including the operator typing a password
/// into the askpass dialog. ssh's own `ConnectTimeout` bounds the network half.
const PROBE_TIMEOUT: Duration = Duration::from_secs(90);

/// The probe's argv (without the program).
///
/// `identity` is what `keys::identity_args_with` produced for these settings —
/// the chosen key, then rmux's own, each only if present — so the probe offers
/// exactly what the saved server will.
pub fn probe_args(host: &SshHostId, identity: &[String]) -> Vec<String> {
    let mut args: Vec<String> =
        ["-o", "ControlPath=none", "-o", "ConnectTimeout=8"].iter().map(|s| (*s).to_owned()).collect();
    args.extend_from_slice(identity);
    if let Some(user) = &host.user {
        args.push("-l".to_owned());
        args.push(user.clone());
    }
    if let Some(port) = host.port {
        args.push("-p".to_owned());
        args.push(port.to_string());
    }
    args.push("-T".to_owned());
    // Verbatim: an alias goes to ssh, which resolves it with the operator's own
    // config. Never `HostName` from our enumeration.
    args.push(host.alias.clone());
    args.push("--".to_owned());
    args.push("uname -s".to_owned());
    args
}

/// Run the probe. `Ok` carries a one-line description of what answered;
/// `Err` is ssh's own reason, trimmed to what a person can act on.
pub async fn test_connection(host: &SshHostId, chosen: Option<&Path>) -> Result<String, String> {
    let identity = match dirs::home_dir() {
        Some(home) => keys::identity_args_with(&home, &host.label(), chosen),
        None => Vec::new(),
    };
    let args = probe_args(host, &identity);

    let mut cmd = tokio::process::Command::new("ssh");
    cmd.no_console_window()
        .args(&args)
        .envs(crate::askpass::env_for_gui_prompts())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);

    let out = match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
        Err(_) => return Err("timed out waiting for the host".to_owned()),
        Ok(Err(e)) => return Err(format!("could not run ssh: {e}")),
        Ok(Ok(out)) => out,
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    describe(out.status.code(), &stdout, &stderr)
}

/// What a probe's outcome means, as a sentence. Pure, so every branch is tested.
fn describe(status: Option<i32>, stdout: &str, stderr: &str) -> Result<String, String> {
    match status {
        Some(0) => Ok(format!("connected · {}", if stdout.is_empty() { "unknown system" } else { stdout })),
        // 255 is ssh's own failure — auth, network, host key — never the remote
        // command's.
        Some(255) | None => Err(last_meaningful_line(stderr).unwrap_or_else(|| "ssh failed without saying why".to_owned())),
        // Authenticated, but `uname` is not there: `cmd.exe` on a Windows host.
        // rmux still works there (through Git for Windows' bash), so this is a
        // success with a note, not a failure.
        Some(_) => Ok("connected · not a POSIX shell (Windows?) — rmux will look for Git Bash".to_owned()),
    }
}

/// The line of ssh's stderr worth showing — the last one that is not a warning
/// about known_hosts being updated.
fn last_meaningful_line(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_is_its_own_connection_with_the_chosen_settings() {
        let host = SshHostId { alias: "devbox".into(), user: Some("me".into()), port: Some(2200) };
        let identity = vec!["-i".to_owned(), "/k/work".to_owned()];
        let args = probe_args(&host, &identity);
        assert!(args.windows(2).any(|w| w == ["-o", "ControlPath=none"]), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-i", "/k/work"]));
        assert!(args.windows(2).any(|w| w == ["-l", "me"]));
        assert!(args.windows(2).any(|w| w == ["-p", "2200"]));
        assert!(!args.iter().any(|a| a.contains("IdentitiesOnly")));
        let dest = args.iter().position(|a| a == "devbox").unwrap();
        assert_eq!(&args[dest + 1..], ["--", "uname -s"]);

        let bare = probe_args(&SshHostId::new("devbox"), &[]);
        assert!(!bare.iter().any(|a| a == "-i" || a == "-l" || a == "-p"), "{bare:?}");
    }

    #[test]
    fn outcomes_read_as_sentences() {
        assert_eq!(describe(Some(0), "Linux", ""), Ok("connected · Linux".to_owned()));
        assert_eq!(
            describe(Some(255), "", "Warning: Permanently added 'x' to the list of known hosts.\nme@x: Permission denied (publickey).\n"),
            Err("me@x: Permission denied (publickey).".to_owned())
        );
        assert!(describe(Some(1), "", "'uname' is not recognized").unwrap().contains("Windows"));
        assert!(describe(Some(255), "", "").is_err());
    }
}
