//! Wires the askpass bridge to the UI.
//!
//! When `ssh` needs a password or a 2FA code, the helper binary reaches the
//! server started here; this module turns that into an event the UI can render
//! and waits for the answer to come back through [`answer_prompt`].
//!
//! Secrets travel Rust → UI → Rust and are never persisted. The `Prompt` sent to
//! the UI carries only the message OpenSSH produced.

use std::collections::HashMap;

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
}

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
            Box::pin(async move { ask_the_user(app, prompt).await })
        })
    };

    let server = AskpassServer::start(answerer).await?;

    askpass::install(helper, server.socket_path().to_path_buf(), server.token().to_owned());

    tracing::info!(socket = %server.socket_path().display(), "askpass bridge ready");
    Ok(AskpassHandle(server))
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
