//! The rmux side of the askpass bridge.
//!
//! Listens on a Unix socket that the helper binary connects to, hands each
//! prompt to the UI, and writes the answer back.
//!
//! Security matters here — this socket dispenses credentials the user types.
//! Three things guard it:
//!
//! 1. The socket lives in a `0700` directory, so no other user can reach it.
//! 2. Every request must carry a token generated at startup and passed to `ssh`
//!    through its environment. A local process of the *same* user could otherwise
//!    connect and phish a password dialog out of rmux.
//! 3. Answers are compared and discarded promptly; nothing is cached here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::{Prompt, classify};

/// What the helper sends us.
#[derive(Debug, Deserialize)]
struct HelperRequest {
    token: String,
    prompt: String,
    /// Absent from an older helper; see [`Prompt::attempt`].
    #[serde(default)]
    attempt: Option<String>,
}

/// Answers a prompt, or `None` if the user dismissed it.
pub type Answerer = Arc<dyn Fn(Prompt) -> super::BoxFuture<Option<String>> + Send + Sync>;

/// A listening askpass socket. Dropping it removes the socket file.
#[derive(Debug)]
pub struct AskpassServer {
    socket: PathBuf,
    token: String,
}

impl AskpassServer {
    /// Bind the socket and start serving prompts.
    pub async fn start(answerer: Answerer) -> anyhow::Result<Self> {
        let dir = runtime_dir()?;
        tokio::fs::create_dir_all(&dir).await?;
        restrict_to_owner(&dir).await?;

        // A stale socket from a crashed run would make bind fail with
        // "Address already in use".
        let socket = dir.join(format!("askpass-{}.sock", short_id()));
        let _ = tokio::fs::remove_file(&socket).await;

        let listener = UnixListener::bind(&socket)?;
        restrict_to_owner(&socket).await?;

        let token = short_id();
        let server = Self { socket, token: token.clone() };

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let answerer = Arc::clone(&answerer);
                        let token = token.clone();
                        // Each prompt is handled independently: `ssh` can ask for
                        // several in a row (passphrase, then a 2FA code), and a
                        // slow answer must not block the next request.
                        tokio::spawn(async move {
                            if let Err(e) = serve(stream, &token, answerer).await {
                                tracing::debug!(error = %e, "askpass request failed");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "askpass listener stopped");
                        break;
                    }
                }
            }
        });

        Ok(server)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for AskpassServer {
    fn drop(&mut self) {
        // Leaving the socket behind would make the next run pick a new path and
        // slowly litter the directory.
        let _ = std::fs::remove_file(&self.socket);
    }
}

async fn serve(stream: UnixStream, token: &str, answerer: Answerer) -> anyhow::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut line = String::new();
    BufReader::new(read_half).read_line(&mut line).await?;

    let request: HelperRequest = serde_json::from_str(&line)?;

    // Constant-time-ish comparison is overkill for a random 128-bit token that
    // never survives the process, but a mismatch must simply drop the connection
    // without hinting at why.
    if request.token != token {
        anyhow::bail!("rejected an askpass request with a bad token");
    }

    let prompt = Prompt {
        id: short_id(),
        kind: classify(&request.prompt),
        message: request.prompt,
        attempt: request.attempt,
    };

    let response = match answerer(prompt).await {
        Some(answer) => serde_json::json!({ "answer": answer }),
        // No `answer` field at all — the helper reads that as "cancelled" and
        // exits non-zero so ssh aborts rather than trying an empty password.
        None => serde_json::json!({ "cancelled": true }),
    };

    write_half.write_all(format!("{response}\n").as_bytes()).await?;
    write_half.flush().await?;
    Ok(())
}

/// Where to put the socket.
///
/// Kept short deliberately: a Unix socket path is capped near 104 bytes, and a
/// long `~/Library/Application Support/...` prefix overruns it and fails at bind
/// time with an error that never mentions length.
fn runtime_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".rmux"))
}

#[cfg(unix)]
async fn restrict_to_owner(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = tokio::fs::metadata(path).await?;
    let mut perms = metadata.permissions();
    // Directories need the execute bit to be traversable; sockets do not.
    perms.set_mode(if metadata.is_dir() { 0o700 } else { 0o600 });
    tokio::fs::set_permissions(path, perms).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn restrict_to_owner(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// A short random identifier — 128 bits of entropy, hex-encoded.
fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::super::PromptKind;
    use super::*;

    fn answer_with(reply: Option<&'static str>) -> Answerer {
        Arc::new(move |_prompt| Box::pin(async move { reply.map(str::to_owned) }))
    }

    #[tokio::test]
    async fn a_prompt_is_answered_over_the_socket() {
        let server = AskpassServer::start(answer_with(Some("hunter2"))).await.unwrap();

        let mut stream = UnixStream::connect(server.socket_path()).await.unwrap();
        let request =
            serde_json::json!({ "token": server.token(), "prompt": "deploy@devbox's password:" });
        stream.write_all(format!("{request}\n").as_bytes()).await.unwrap();

        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).await.unwrap();

        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["answer"], "hunter2");
    }

    #[tokio::test]
    async fn a_dismissed_prompt_reports_no_answer() {
        let server = AskpassServer::start(answer_with(None)).await.unwrap();

        let mut stream = UnixStream::connect(server.socket_path()).await.unwrap();
        let request = serde_json::json!({ "token": server.token(), "prompt": "password:" });
        stream.write_all(format!("{request}\n").as_bytes()).await.unwrap();

        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).await.unwrap();

        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        // The absence of `answer` is what makes the helper exit non-zero.
        assert!(response.get("answer").is_none());
        assert_eq!(response["cancelled"], true);
    }

    #[tokio::test]
    async fn a_request_without_the_token_gets_nothing() {
        // Any local process could otherwise phish a credential dialog out of rmux
        // and read what the user typed.
        let server = AskpassServer::start(answer_with(Some("hunter2"))).await.unwrap();

        let mut stream = UnixStream::connect(server.socket_path()).await.unwrap();
        let request = serde_json::json!({ "token": "wrong", "prompt": "password:" });
        stream.write_all(format!("{request}\n").as_bytes()).await.unwrap();

        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).await.unwrap();

        // Connection closed with no reply, rather than an error that confirms the
        // socket is rmux's.
        assert!(line.is_empty(), "an unauthorised request must receive nothing, got: {line:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_socket_is_not_reachable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;

        let server = AskpassServer::start(answer_with(Some("x"))).await.unwrap();
        let mode = std::fs::metadata(server.socket_path()).unwrap().permissions().mode();

        assert_eq!(mode & 0o077, 0, "socket is group/other accessible: {:o}", mode);
    }

    #[tokio::test]
    async fn the_socket_is_removed_when_the_server_is_dropped() {
        let server = AskpassServer::start(answer_with(Some("x"))).await.unwrap();
        let path = server.socket_path().to_path_buf();
        assert!(path.exists());

        drop(server);
        assert!(!path.exists(), "a leaked socket accumulates on every run");
    }

    #[tokio::test]
    async fn the_prompt_reaches_the_ui_classified() {
        // The UI needs to know whether to mask the field; getting this wrong
        // echoes a password in cleartext.
        let seen = Arc::new(parking_lot::Mutex::new(None));
        let captured = Arc::clone(&seen);

        let answerer: Answerer = Arc::new(move |prompt: Prompt| {
            *captured.lock() = Some(prompt);
            Box::pin(async { Some("x".to_owned()) })
        });

        let server = AskpassServer::start(answerer).await.unwrap();
        let mut stream = UnixStream::connect(server.socket_path()).await.unwrap();
        let request =
            serde_json::json!({ "token": server.token(), "prompt": "Verification code:" });
        stream.write_all(format!("{request}\n").as_bytes()).await.unwrap();

        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).await.unwrap();

        let prompt = seen.lock().clone().expect("the UI was never asked");
        assert_eq!(prompt.kind, PromptKind::Challenge);
        assert_eq!(prompt.message, "Verification code:");
    }
}
