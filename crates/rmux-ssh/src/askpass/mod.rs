//! Routing OpenSSH credential prompts into the GUI.
//!
//! `ssh` normally reads passwords and 2FA responses straight from the controlling
//! terminal. rmux has no terminal to offer, so without intervention a
//! password-auth or keyboard-interactive host simply hangs — which is exactly the
//! reason wrappers around the `ssh` binary are often described as supporting
//! "password-less authentication only".
//!
//! OpenSSH's escape hatch is `SSH_ASKPASS`: point it at a program and set
//! `SSH_ASKPASS_REQUIRE=force`, and every prompt is handed to that program on the
//! command line, with the answer read from its stdout. Our helper binary
//! (`askpass/`) forwards the prompt to the running rmux instance over a Unix
//! socket, the UI asks the user, and the answer travels back.
//!
//! `SSH_ASKPASS_REQUIRE=force` matters: without it OpenSSH only consults the
//! helper when `DISPLAY` is set and no TTY is present, which is not reliably true
//! for a desktop app launched from Finder or from a shell.

use std::collections::BTreeMap;
use std::path::PathBuf;

use parking_lot::RwLock;
use serde::Serialize;

// **One bridge, two transports.** A Unix socket where there are Unix sockets,
// a named pipe on Windows — and the pipe carries an explicit owner-only DACL,
// which is what makes it an equivalent guard rather than a weaker one. It was a
// stub until a real password host proved the stub's premise wrong: `ssh` given
// neither a terminal nor a helper does not "fail fast with a reason", it burns
// its retries against nothing and reports `Permission denied`, on every command,
// forever. See `server_windows.rs`.
//
// Anything that is neither is still stubbed out.
#[cfg(unix)]
pub mod server;

#[cfg(windows)]
#[path = "server_windows.rs"]
pub mod server;

#[cfg(not(any(unix, windows)))]
#[path = "server_stub.rs"]
pub mod server;

pub use server::AskpassServer;

/// A boxed future, spelled out so this crate needn't depend on `futures`.
pub type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

static ASKPASS: RwLock<Option<AskpassConfig>> = RwLock::new(None);

#[derive(Clone, Debug)]
struct AskpassConfig {
    helper: PathBuf,
    socket: PathBuf,
    /// Proves to the server that a prompt really came from an `ssh` we launched.
    token: String,
}

/// Register the helper binary, the socket it calls back on, and the token that
/// authenticates it.
///
/// Called once at startup, after the socket is listening.
pub fn install(helper: PathBuf, socket: PathBuf, token: String) {
    *ASKPASS.write() = Some(AskpassConfig { helper, socket, token });
}

/// Forget the helper — prompts fall back to failing fast rather than hanging.
pub fn uninstall() {
    *ASKPASS.write() = None;
}

/// Whether GUI prompting is wired up.
pub fn is_installed() -> bool {
    ASKPASS.read().is_some()
}

/// Environment for a spawned `ssh` so its prompts reach the UI.
pub fn env_for_gui_prompts() -> BTreeMap<String, String> {
    env_for(ASKPASS.read().as_ref())
}

/// The pure half of [`env_for_gui_prompts`].
///
/// Split out so it can be tested without touching the process-wide `ASKPASS`
/// registration. Tests that mutated that global raced each other under the
/// default parallel test runner — one installing a helper while another asserted
/// that none was installed — which is a flake that only shows up sometimes.
fn env_for(config: Option<&AskpassConfig>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();

    let Some(config) = config else {
        // Nothing registered. Tell ssh not to try a terminal it will never get —
        // failing immediately is far better than a terminal that hangs forever
        // with no visible reason.
        //
        // A `BatchMode` entry used to sit here too, and it never did anything:
        // `BatchMode` is an **ssh_config option, not an environment variable**,
        // so `ssh` has never read a variable by that name. Removed rather than
        // converted to a `-o` flag — measured against a real password host, `ssh`
        // given a pipe for stdin already fails immediately with "Permission
        // denied" rather than waiting for anything, so the flag would buy no
        // behaviour while changing what macOS and Linux do.
        env.insert("SSH_ASKPASS_REQUIRE".to_owned(), "never".to_owned());
        return env;
    };

    env.insert("SSH_ASKPASS".to_owned(), config.helper.display().to_string());
    // "force" is required: the default heuristics skip the helper when a TTY is
    // present or DISPLAY is unset, neither of which we can rely on.
    env.insert("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned());
    env.insert("RMUX_ASKPASS_SOCKET".to_owned(), config.socket.display().to_string());
    // **Which `ssh` is asking.** Minted per invocation and never reused, so a
    // second prompt carrying the same value is the *same* `ssh` asking again —
    // which is what it does when an answer is refused. Nothing here acts on it;
    // it exists so the app can tell "that credential was wrong" apart from "a
    // different connection is authenticating too", which are otherwise
    // indistinguishable and happen milliseconds apart. See `Memory::decide` in
    // `src-tauri/src/askpass.rs`.
    env.insert("RMUX_ASKPASS_ATTEMPT".to_owned(), uuid::Uuid::new_v4().simple().to_string());
    // Passed explicitly rather than left to be inherited from our own
    // environment: the helper is a grandchild process (ssh spawns it), and
    // depending on two levels of implicit inheritance for a security check is
    // the kind of thing that breaks silently and fails open.
    env.insert("RMUX_ASKPASS_TOKEN".to_owned(), config.token.clone());

    // Some OpenSSH builds still gate on DISPLAY being non-empty.
    if std::env::var_os("DISPLAY").is_none() {
        env.insert("DISPLAY".to_owned(), ":0".to_owned());
    }

    env
}

/// A credential request on its way to the UI.
///
/// Lives here rather than beside the server because it is plain data with no
/// platform in it, and the server it travels through is compiled out on
/// Windows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    /// Correlates the answer with the request.
    pub id: String,
    /// The text OpenSSH gave us, shown verbatim — it names the host and the
    /// account, which is what tells the user whether to trust it.
    pub message: String,
    pub kind: PromptKind,
    /// Which `ssh` invocation asked, from `RMUX_ASKPASS_ATTEMPT`.
    ///
    /// **Not sent to the UI.** It is bookkeeping for deciding whether a
    /// remembered credential was just refused, and the webview has no use for
    /// it — see the env var's comment in [`env_for`]. `None` if the helper did
    /// not supply one.
    #[serde(skip)]
    pub attempt: Option<String>,
}

/// What kind of answer a prompt wants — drives which UI is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PromptKind {
    /// Account or key passphrase; input must be masked.
    Password,
    /// A yes/no question, most often unknown-host-key confirmation.
    Confirm,
    /// A 2FA code or other keyboard-interactive challenge.
    Challenge,
}

/// Classify a prompt from its text alone.
///
/// OpenSSH gives the helper no structured type, only the human-readable string,
/// so this is unavoidably heuristic. The stakes are asymmetric: showing a masked
/// field for a yes/no question is a small annoyance, whereas echoing a password
/// in cleartext is a real leak — so anything unrecognised is treated as a secret.
pub fn classify(prompt: &str) -> PromptKind {
    let lower = prompt.to_ascii_lowercase();

    if lower.contains("(yes/no")
        || lower.contains("yes/no/[fingerprint]")
        || lower.contains("are you sure you want to continue connecting")
    {
        return PromptKind::Confirm;
    }
    if lower.contains("password") || lower.contains("passphrase") {
        return PromptKind::Password;
    }
    if lower.contains("verification code")
        || lower.contains("one-time")
        || lower.contains("otp")
        || lower.contains("token")
        || lower.contains("duo")
    {
        return PromptKind::Challenge;
    }

    PromptKind::Password
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_helper_ssh_is_told_not_to_wait_for_a_terminal() {
        let env = env_for(None);
        // A hang with no explanation is the worst outcome; fail fast instead.
        assert_eq!(env.get("SSH_ASKPASS_REQUIRE").map(String::as_str), Some("never"));
        assert!(!env.contains_key("SSH_ASKPASS"));
    }

    #[test]
    fn installing_forces_ssh_to_use_the_helper() {
        let config = AskpassConfig {
            helper: PathBuf::from("/opt/rmux/askpass"),
            socket: PathBuf::from("/tmp/rmux.sock"),
            token: "tok-123".to_owned(),
        };
        let env = env_for(Some(&config));

        assert_eq!(env.get("SSH_ASKPASS").map(String::as_str), Some("/opt/rmux/askpass"));
        // Anything weaker than "force" lets OpenSSH silently skip the helper.
        assert_eq!(env.get("SSH_ASKPASS_REQUIRE").map(String::as_str), Some("force"));
        assert_eq!(env.get("RMUX_ASKPASS_SOCKET").map(String::as_str), Some("/tmp/rmux.sock"));
        // Without this the helper cannot prove it is ours and gets no answer.
        assert_eq!(env.get("RMUX_ASKPASS_TOKEN").map(String::as_str), Some("tok-123"));
    }

    #[test]
    fn registering_a_helper_is_visible_to_the_whole_process() {
        // The global registration itself still needs covering; this is the only
        // test that touches it, so nothing can race with it.
        assert!(!is_installed());
        install(
            PathBuf::from("/opt/rmux/askpass"),
            PathBuf::from("/tmp/rmux.sock"),
            "tok-123".to_owned(),
        );
        assert!(is_installed());
        assert_eq!(
            env_for_gui_prompts().get("SSH_ASKPASS").map(String::as_str),
            Some("/opt/rmux/askpass")
        );
        uninstall();
        assert!(!is_installed());
    }

    #[test]
    fn host_key_questions_are_confirmations_not_secrets() {
        assert_eq!(
            classify("The authenticity of host 'x' can't be established.\nAre you sure you want to continue connecting (yes/no/[fingerprint])?"),
            PromptKind::Confirm
        );
    }

    #[test]
    fn passwords_and_passphrases_are_masked() {
        assert_eq!(classify("deploy@devbox's password:"), PromptKind::Password);
        assert_eq!(classify("Enter passphrase for key '/home/me/.ssh/id_ed25519':"), PromptKind::Password);
    }

    #[test]
    fn second_factor_challenges_are_recognised() {
        assert_eq!(classify("Verification code:"), PromptKind::Challenge);
        assert_eq!(classify("Duo two-factor login"), PromptKind::Challenge);
    }

    #[test]
    fn unknown_prompts_default_to_masked_input() {
        // Erring toward masking: echoing a secret is worse than hiding a non-secret.
        assert_eq!(classify("Something unfamiliar:"), PromptKind::Password);
    }
}
