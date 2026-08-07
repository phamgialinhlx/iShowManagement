//! Wires the askpass bridge to the UI.
//!
//! When `ssh` needs a password or a 2FA code, the helper binary reaches the
//! server started here; this module turns that into an event the UI can render
//! and waits for the answer to come back through [`answer_prompt`].
//!
//! Secrets travel Rust → UI → Rust and are never persisted. The `Prompt` sent to
//! the UI carries only the message OpenSSH produced.

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;
use rmux_ssh::askpass::{self, AskpassServer, Prompt, server::Answerer};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

/// The event the UI listens for.
pub const PROMPT_EVENT: &str = "ssh://prompt";

/// Prompts waiting on the user.
#[derive(Default)]
pub struct PromptStore {
    pending: Mutex<HashMap<String, oneshot::Sender<Option<String>>>>,
    /// **Windows only.** See [`Memory`].
    memory: Mutex<Memory>,
}

/// What has already been answered this run, and what is on screen right now.
///
/// ## Why this exists, and only on Windows
///
/// Everywhere else a host is authenticated **once**: the `ControlMaster` holds
/// the connection open and every later command rides it, so `ssh` asks for a
/// password once per host per run and the question never comes back. Windows
/// OpenSSH has no `ControlMaster`, so every command — every file listing, every
/// two-second metrics sample, every 1.5-second status poll — is its own
/// connection and its own authentication.
///
/// Bridging prompts to the UI without this would therefore replace "a password
/// host cannot connect" with "a password dialog appears once a second forever",
/// which is not an improvement. Remembering the answer for the run is what makes
/// the two platforms behave the same; it is the same guarantee `ControlMaster`
/// already provides, implemented where there is no `ControlMaster`.
///
/// Secrets live in memory for the life of the process and are never written
/// anywhere. That is a real widening over the Unix path, which holds an answer
/// only as long as it takes to reply — and it is the narrowest version that
/// works, because the alternative is asking the operator to retype a password
/// every second.
#[derive(Default)]
struct Memory {
    /// Prompt text → the answer that satisfied it. The text names the host and
    /// account (`root@10.0.0.1's password:`), so it is already per-destination.
    answers: HashMap<String, String>,
    /// Prompts on screen now, so simultaneous connections share one dialog
    /// instead of stacking several identical ones.
    waiting: HashMap<String, Vec<oneshot::Sender<Option<String>>>>,
    /// Which `ssh` invocations have already been handed a remembered answer,
    /// as `(attempt, prompt)`. See [`Memory::decide`].
    served: VecDeque<(String, String)>,
}

/// How many `(attempt, prompt)` pairs to remember.
///
/// Only needed for as long as one `ssh` process might ask again, which is
/// milliseconds — `ssh` re-prompts the instant the helper returns a refused
/// answer. The queue is bounded because it would otherwise gain an entry every
/// couple of seconds for as long as the app is open, which is a slow leak in
/// exchange for nothing.
const SERVED_HISTORY: usize = 64;

/// Keeps the socket alive for the life of the app. Dropping it removes the
/// socket file and future prompts would fail.
pub struct AskpassHandle(#[allow(dead_code)] AskpassServer);

/// Start the bridge and point `ssh` at it.
///
/// Failure is not fatal: rmux still works for key-based hosts, which is the
/// common case. Only password and 2FA hosts become unusable, and
/// `env_for_gui_prompts` degrades to telling `ssh` not to wait for a terminal —
/// so those hosts fail fast instead of hanging.
pub async fn start(app: AppHandle) -> anyhow::Result<AskpassHandle> {
    let helper = helper_path()?;

    let answerer: Answerer = {
        let app = app.clone();
        std::sync::Arc::new(move |prompt: Prompt| {
            let app = app.clone();
            Box::pin(async move { answer(app, prompt).await })
        })
    };

    let server = AskpassServer::start(answerer).await?;

    askpass::install(helper, server.socket_path().to_path_buf(), server.token().to_owned());

    tracing::info!(socket = %server.socket_path().display(), "askpass bridge ready");
    Ok(AskpassHandle(server))
}

/// Answer a prompt, asking the operator only when there is no other way.
///
/// On every platform but Windows this is exactly `ask_the_user` — the
/// `ControlMaster` means a host is authenticated once and the question does not
/// come back, so there is nothing to remember and nothing to coalesce. See
/// [`Memory`] for why Windows needs both.
async fn answer(app: AppHandle, prompt: Prompt) -> Option<String> {
    if !cfg!(windows) {
        return ask_the_user(app, prompt).await;
    }

    // Decided under one lock, so two connections arriving together cannot both
    // conclude that they should ask.
    let decision = app
        .state::<PromptStore>()
        .memory
        .lock()
        .decide(&prompt.message, prompt.attempt.as_deref());

    match decision {
        Decision::Known(answer) => Some(answer),
        Decision::Join(rx) => rx.await.unwrap_or(None),
        Decision::Ask => {
            let answer = ask_the_user(app.clone(), prompt.clone()).await;
            app.state::<PromptStore>().memory.lock().settle(&prompt.message, answer.clone());
            answer
        }
    }
}

/// What to do about a prompt that has just arrived.
enum Decision {
    /// Answered before, this run.
    Known(String),
    /// An identical question is already on screen; take its answer.
    Join(oneshot::Receiver<Option<String>>),
    /// Nobody has asked. Put it in front of the operator.
    Ask,
}

impl Memory {
    /// What to do about `message`, asked by the `ssh` invocation `attempt`.
    ///
    /// ## Why the attempt id, rather than a timer
    ///
    /// A remembered answer that turns out to be wrong must be forgotten, or
    /// every command from then on fails with a credential the operator has no
    /// way to correct — the app would have to be restarted, with nothing on
    /// screen saying why.
    ///
    /// The signal for "wrong" is `ssh` asking the same question again. The
    /// obvious implementation is a short timer, and it does not work: the gap
    /// that says "refused" is *the same size* as the gap between two different
    /// connections prompting at once, which is precisely what happens when a
    /// session opens and a terminal, a file tree and a metrics poller all
    /// authenticate together. Any window wide enough to catch a refusal also
    /// catches those, and the operator is asked to retype a password that was
    /// perfectly correct.
    ///
    /// `RMUX_ASKPASS_ATTEMPT` removes the guess. It is minted per spawned `ssh`
    /// and travels to the helper in that process's environment, so *the same*
    /// attempt asking twice is a refusal by construction, and two connections
    /// asking together are two different attempts. Exact, rather than nearly
    /// right in the cases that matter least.
    fn decide(&mut self, message: &str, attempt: Option<&str>) -> Decision {
        let key = attempt.map(|a| (a.to_owned(), message.to_owned()));

        if let Some(key) = &key
            && self.served.contains(key)
        {
            tracing::info!("a remembered credential was refused; asking again");
            self.answers.remove(message);
            self.served.retain(|seen| seen != key);
        }

        if let Some(known) = self.answers.get(message).cloned() {
            // Without an attempt id a refusal cannot be detected, so the answer
            // is still given — `ssh` gives up after its own retries — but
            // nothing is recorded, because there is no invocation to record it
            // against. Only reachable if the helper predates the env var.
            if let Some(key) = key {
                self.served.push_back(key);
                if self.served.len() > SERVED_HISTORY {
                    self.served.pop_front();
                }
            }
            return Decision::Known(known);
        }

        match self.waiting.get_mut(message) {
            Some(waiters) => {
                let (tx, rx) = oneshot::channel();
                waiters.push(tx);
                Decision::Join(rx)
            }
            None => {
                self.waiting.insert(message.to_owned(), Vec::new());
                Decision::Ask
            }
        }
    }

    /// Record what the operator said and release anyone who joined.
    fn settle(&mut self, message: &str, answer: Option<String>) {
        // Only a real answer is worth keeping. A cancellation is "not now",
        // not a credential — remembering it would silently refuse every later
        // connection to that host.
        //
        // Nothing is added to `served` here. That list means "this invocation
        // was handed something it did not type"; an answer the operator just
        // typed is not that, and recording it would make the very next
        // connection look like a refusal.
        if let Some(answer) = &answer {
            self.answers.insert(message.to_owned(), answer.clone());
        }
        for waiter in self.waiting.remove(message).unwrap_or_default() {
            let _ = waiter.send(answer.clone());
        }
    }
}

/// Show the prompt and wait for the user.
async fn ask_the_user(app: AppHandle, prompt: Prompt) -> Option<String> {
    let (tx, rx) = oneshot::channel();

    {
        let store = app.state::<PromptStore>();
        store.pending.lock().insert(prompt.id.clone(), tx);
    }

    if app.emit(PROMPT_EVENT, &prompt).is_err() {
        // No window to ask. Treat as cancelled rather than leaving `ssh` waiting
        // on an answer that can never arrive.
        app.state::<PromptStore>().pending.lock().remove(&prompt.id);
        return None;
    }

    // A dropped sender (window closed mid-prompt) resolves to cancelled.
    rx.await.unwrap_or(None)
}

/// Deliver the user's answer, or `None` if they dismissed the dialog.
#[tauri::command]
pub async fn answer_prompt(
    store: State<'_, PromptStore>,
    id: String,
    answer: Option<String>,
) -> Result<(), String> {
    let sender = store.inner().pending.lock().remove(&id);

    match sender {
        // The receiver is gone if `ssh` already timed out; nothing to do.
        Some(sender) => sender.send(answer).map_err(|_| "prompt is no longer waiting".to_owned()),
        // Answering twice, or answering a stale prompt, is harmless.
        None => Ok(()),
    }
}

/// Locate the helper binary.
///
/// It sits beside the main executable — true for `cargo run`, and true in a
/// bundle once the helper is added as a sidecar. Resolving it at startup means a
/// missing helper is reported here rather than at the moment a host asks for a
/// password.
fn helper_path() -> anyhow::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| anyhow::anyhow!("executable has no parent directory"))?;

    let helper = dir.join(if cfg!(windows) { "rmux-askpass.exe" } else { "rmux-askpass" });
    anyhow::ensure!(helper.exists(), "askpass helper not found at {}", helper.display());

    Ok(helper)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answering_an_unknown_prompt_is_harmless() {
        // The UI may answer a prompt that already timed out or was superseded;
        // that must not error out or panic.
        let store = PromptStore::default();
        assert!(store.pending.lock().is_empty());

        // Directly exercise the same path the command takes.
        let sender = store.pending.lock().remove("no-such-id");
        assert!(sender.is_none());
    }

    /// What OpenSSH actually prints. The host and account are part of it, which
    /// is what makes the text usable as a per-destination key.
    const PROMPT: &str = "root@devbox's password:";

    #[test]
    fn an_answer_given_once_is_not_asked_for_again() {
        let mut memory = Memory::default();

        assert!(matches!(memory.decide(PROMPT, Some("ssh-1")), Decision::Ask));
        memory.settle(PROMPT, Some("hunter2".to_owned()));

        // A later connection — a metrics sample, a file listing — is answered
        // without troubling the operator. Without this they would be asked
        // roughly once a second for as long as the session stayed open.
        match memory.decide(PROMPT, Some("ssh-2")) {
            Decision::Known(answer) => assert_eq!(answer, "hunter2"),
            _ => panic!("the answer should have been remembered"),
        }
    }

    #[test]
    fn a_different_host_is_asked_separately() {
        // The prompt text names the account and the host, which is what makes it
        // a safe key. Sharing one answer across hosts would send a credential
        // somewhere it was never typed for.
        let mut memory = Memory::default();
        memory.settle("root@a's password:", Some("secret-a".to_owned()));

        assert!(matches!(memory.decide("root@b's password:", Some("ssh-1")), Decision::Ask));
    }

    #[test]
    fn a_refused_credential_is_forgotten_rather_than_retried_forever() {
        // `ssh` re-prompts immediately when a password is rejected, from the
        // same process. Without noticing that, a single typo would be kept for
        // the life of the app and every command would fail with nothing on
        // screen explaining why — only a restart would clear it.
        let mut memory = Memory::default();
        memory.settle(PROMPT, Some("wrong".to_owned()));

        assert!(matches!(memory.decide(PROMPT, Some("ssh-1")), Decision::Known(_)), "served once");
        assert!(
            matches!(memory.decide(PROMPT, Some("ssh-1")), Decision::Ask),
            "the same ssh asking again means it refused what we gave it"
        );
    }

    /// The reason this is an attempt id and not a timer.
    #[test]
    fn connections_authenticating_together_are_not_mistaken_for_a_refusal() {
        // Opening a session starts a terminal, a file tree and a metrics poller
        // at once, so three `ssh` processes prompt within milliseconds of each
        // other. Any time-based rule wide enough to catch a genuine refusal also
        // catches this, and asks the operator to retype a password that was
        // correct.
        let mut memory = Memory::default();
        memory.settle(PROMPT, Some("hunter2".to_owned()));

        for ssh in ["terminal", "files", "metrics"] {
            assert!(
                matches!(memory.decide(PROMPT, Some(ssh)), Decision::Known(_)),
                "{ssh} is a different connection, not a rejection"
            );
        }
    }

    #[test]
    fn the_served_list_cannot_grow_without_limit() {
        // One entry per authenticated command, and rmux runs one every couple of
        // seconds for as long as it is open.
        let mut memory = Memory::default();
        memory.settle(PROMPT, Some("hunter2".to_owned()));

        for i in 0..(SERVED_HISTORY * 4) {
            let _ = memory.decide(PROMPT, Some(&format!("ssh-{i}")));
        }
        assert!(memory.served.len() <= SERVED_HISTORY, "served grew to {}", memory.served.len());
    }

    #[tokio::test]
    async fn simultaneous_connections_share_one_dialog() {
        // Three identical password dialogs stacked on each other is a
        // broken-looking app.
        let mut memory = Memory::default();

        assert!(matches!(memory.decide(PROMPT, Some("ssh-1")), Decision::Ask));
        let Decision::Join(rx) = memory.decide(PROMPT, Some("ssh-2")) else {
            panic!("the second asker should have joined the first");
        };

        memory.settle(PROMPT, Some("hunter2".to_owned()));
        assert_eq!(rx.await.unwrap(), Some("hunter2".to_owned()));
    }

    #[tokio::test]
    async fn a_cancelled_prompt_is_not_remembered_as_an_answer() {
        let mut memory = Memory::default();

        assert!(matches!(memory.decide(PROMPT, Some("ssh-1")), Decision::Ask));
        let Decision::Join(rx) = memory.decide(PROMPT, Some("ssh-2")) else {
            panic!("expected a joiner")
        };

        memory.settle(PROMPT, None);

        // The joiner is released rather than left hanging...
        assert_eq!(rx.await.unwrap(), None);
        // ...and the next connection asks again rather than being refused
        // forever by a remembered "no".
        assert!(matches!(memory.decide(PROMPT, Some("ssh-3")), Decision::Ask));
    }

    #[test]
    fn a_helper_that_sends_no_attempt_is_still_answered() {
        // Only reachable if the helper predates the env var. Refusal cannot be
        // detected without one, but withholding the answer would be worse: the
        // host simply would not connect.
        let mut memory = Memory::default();
        memory.settle(PROMPT, Some("hunter2".to_owned()));

        assert!(matches!(memory.decide(PROMPT, None), Decision::Known(_)));
        assert!(memory.served.is_empty(), "nothing to record it against");
    }

    #[tokio::test]
    async fn an_answer_reaches_the_waiter() {
        let store = PromptStore::default();
        let (tx, rx) = oneshot::channel();
        store.pending.lock().insert("p1".to_owned(), tx);

        let sender = store.pending.lock().remove("p1").expect("prompt should be pending");
        sender.send(Some("hunter2".to_owned())).unwrap();

        assert_eq!(rx.await.unwrap(), Some("hunter2".to_owned()));
        // Answering consumes the prompt, so a duplicate answer finds nothing.
        assert!(store.pending.lock().remove("p1").is_none());
    }
}
