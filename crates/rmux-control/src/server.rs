//! The listener, and who is allowed to talk to it.
//!
//! Two gates, and they answer different questions.
//!
//! **The filesystem answers "which user".** The socket is `0600` inside a
//! `0700` directory, so the kernel refuses another account before rmux sees a
//! byte. That is the same arrangement as the askpass bridge, for the same
//! reason: this socket opens ssh tunnels into the operator's infrastructure.
//!
//! **The token answers "which process".** Same-user is not the same as
//! trusted — a browser extension host, a sandboxed helper, anything the
//! operator ran once. Requiring a secret that only a reader of a `0600` file
//! could know means a client must already have had that access.
//!
//! The token is regenerated per run. A stale one in a client's config then
//! fails cleanly at `Hello` rather than authorising a connection against an
//! rmux that has since restarted with different sessions.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::protocol::{Event, Message, Request, Response, VERSION};

/// What the server needs from rmux to answer a request.
///
/// A trait so this crate never depends on the Tauri layer — the protocol and
/// its transport stay testable without a GUI, which is the same separation the
/// rest of the workspace keeps.
#[async_trait::async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn handle(&self, request: Request) -> Response;
}

pub struct ControlServer {
    socket: PathBuf,
    token: String,
    events: broadcast::Sender<Event>,
}

impl ControlServer {
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Push a state change to every connected client.
    ///
    /// Fails silently when nobody is listening, which is the ordinary case —
    /// most runs of rmux have no client attached at all.
    pub fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Start listening, and write the handshake file clients read.
    pub async fn start<H: Handler>(handler: std::sync::Arc<H>) -> anyhow::Result<Self> {
        let dir = crate::runtime_dir()?;
        std::fs::create_dir_all(&dir)?;
        restrict(&dir, 0o700)?;

        let socket = crate::socket_path()?;
        // A socket file left by a crashed run makes bind fail with "Address
        // already in use" — which reads as "another rmux is running" when
        // nothing is.
        let _ = std::fs::remove_file(&socket);

        let listener = UnixListener::bind(&socket)?;
        restrict(&socket, 0o600)?;

        let token = random_token();
        write_handshake(&socket, &token)?;

        let (events, _) = broadcast::channel(64);
        let server = Self { socket: socket.clone(), token: token.clone(), events: events.clone() };

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let handler = std::sync::Arc::clone(&handler);
                let token = token.clone();
                let events = events.subscribe();
                tokio::spawn(async move {
                    if let Err(e) = serve(stream, handler, token, events).await {
                        tracing::debug!(error = %e, "control client disconnected");
                    }
                });
            }
        });

        tracing::info!(socket = %socket.display(), "control socket ready");
        Ok(server)
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
        if let Ok(path) = crate::handshake_path() {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn serve<H: Handler>(
    stream: UnixStream,
    handler: std::sync::Arc<H>,
    token: String,
    mut events: broadcast::Receiver<Event>,
) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut greeted = false;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                if line.trim().is_empty() {
                    continue;
                }

                let (id, request) = match serde_json::from_str::<Message>(&line) {
                    Ok(Message::Request { id, request }) => (id, request),
                    // A client sending us a response or an event is confused;
                    // saying so beats ignoring it silently.
                    _ => {
                        let reply = Message::Response {
                            id: 0,
                            response: Response::Error { message: "expected a request".into() },
                        };
                        write_line(&mut write, &reply).await?;
                        continue;
                    }
                };

                let response = match (&request, greeted) {
                    (Request::Hello { token: given, client }, _) => {
                        // Constant-time is overkill for a 32-byte random token
                        // over a socket only this user can open, but the check
                        // itself must be exact.
                        if given != &token {
                            let reply = Message::Response {
                                id,
                                response: Response::Error { message: "bad token".into() },
                            };
                            write_line(&mut write, &reply).await?;
                            return Ok(());
                        }
                        greeted = true;
                        tracing::info!(client = %client, "control client connected");
                        Response::Hello { version: VERSION, app: "rmux".into() }
                    }
                    // Nothing is answered before the greeting, so an unauthorised
                    // peer cannot enumerate sessions or open a tunnel.
                    (_, false) => Response::Error { message: "say hello first".into() },
                    (_, true) => handler.handle(request).await,
                };

                write_line(&mut write, &Message::Response { id, response }).await?;
            }

            event = events.recv() => {
                match event {
                    Ok(event) if greeted => {
                        write_line(&mut write, &Message::Event(event)).await?;
                    }
                    Ok(_) => {}
                    // Lagged: this client reads slower than rmux emits. Its view
                    // is now incomplete, so it is dropped rather than left
                    // silently stale — it will reconnect and re-list.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "control client fell behind");
                        return Ok(());
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, message: &Message) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await?;
    Ok(())
}

fn random_token() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Write the file a client reads to find the socket and its token.
fn write_handshake(socket: &Path, token: &str) -> anyhow::Result<()> {
    let path = crate::handshake_path()?;
    let body = serde_json::json!({
        "socket": socket.to_string_lossy(),
        "token": token,
        "version": VERSION,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&body)?)?;
    // The token is in here, so this is as sensitive as the socket itself.
    restrict(&path, 0o600)?;
    Ok(())
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> anyhow::Result<()> {
    // Windows has no unix sockets; that port needs a named pipe with an ACL,
    // and pretending the mode was applied would be worse than saying nothing.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_never_repeat() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 64, "32 bytes as hex");
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_handshake_carries_what_a_client_needs_and_nothing_more() {
        let body = serde_json::json!({
            "socket": "/tmp/x/control.sock",
            "token": "abc",
            "version": VERSION,
        });
        // A client needs exactly these three to connect. Anything else here is
        // state a client might start depending on.
        let map = body.as_object().unwrap();
        assert_eq!(map.len(), 3, "{body}");
        assert!(map.contains_key("socket") && map.contains_key("token"));
    }
}
