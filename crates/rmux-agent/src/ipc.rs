//! Local IPC between the attach proxy and the daemon.
//!
//! Unix domain sockets on macOS and Linux, named pipes on Windows. Both are
//! kernel-local — no TCP, no loopback port — which matters because this channel
//! hands out shell access: a loopback socket is reachable by every process on
//! the machine, while these are constrained by filesystem permissions and pipe
//! ACLs respectively.
//!
//! The two platforms differ enough that they cannot share an implementation, but
//! they present the same three operations, so nothing above this module has a
//! `cfg` in it.

use std::path::PathBuf;

/// Where the daemon listens.
///
/// Version-stamped so a newer client never speaks to an older daemon it does not
/// understand — a mismatch there surfaces as corrupt terminal output rather than
/// an honest error.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Unix: the socket path. Windows: the pipe name.
    pub address: String,
    /// Unix only: the directory to create and lock down.
    pub parent: Option<PathBuf>,
}

impl Endpoint {
    pub fn for_version(version: &str) -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
            let dir = home.join(".rmux");
            Ok(Self {
                address: dir.join(format!("agent-{version}.sock")).to_string_lossy().into_owned(),
                parent: Some(dir),
            })
        }
        #[cfg(windows)]
        {
            // Pipes live in a global namespace, so the name carries the user —
            // otherwise two people on one machine would share a daemon, and with
            // it each other's shells.
            let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_owned());
            Ok(Self {
                address: format!(r"\\.\pipe\rmux-agent-{version}-{user}"),
                parent: None,
            })
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::Endpoint;

    pub type Stream = tokio::net::UnixStream;

    pub struct Listener(tokio::net::UnixListener);

    impl Listener {
        pub async fn bind(endpoint: &Endpoint) -> anyhow::Result<Self> {
            if let Some(dir) = &endpoint.parent {
                tokio::fs::create_dir_all(dir).await?;
                restrict(dir).await?;
            }

            let path = std::path::Path::new(&endpoint.address);
            // A socket file left by a crashed daemon blocks bind. If something is
            // actually listening, that daemon wins; otherwise the file is debris.
            if path.exists() {
                if tokio::net::UnixStream::connect(path).await.is_ok() {
                    anyhow::bail!("an agent is already listening on {}", path.display());
                }
                let _ = tokio::fs::remove_file(path).await;
            }

            let listener = tokio::net::UnixListener::bind(path)?;
            restrict(path).await?;
            Ok(Self(listener))
        }

        pub async fn accept(&mut self) -> anyhow::Result<Stream> {
            let (stream, _) = self.0.accept().await?;
            Ok(stream)
        }
    }

    pub async fn connect(endpoint: &Endpoint) -> anyhow::Result<Stream> {
        Ok(tokio::net::UnixStream::connect(&endpoint.address).await?)
    }

    /// Owner-only. This endpoint hands out shell access.
    async fn restrict(path: &std::path::Path) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = tokio::fs::metadata(path).await?;
        let mut perms = metadata.permissions();
        perms.set_mode(if metadata.is_dir() { 0o700 } else { 0o600 });
        tokio::fs::set_permissions(path, perms).await?;
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::Endpoint;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    pub type Stream = NamedPipeServer;

    /// A named-pipe server.
    ///
    /// Unlike a Unix listener, a pipe instance *is* the connection: accepting
    /// consumes it, so the next instance must be created before waiting again.
    /// Getting that order wrong leaves a window where a client sees
    /// "file not found" instead of connecting.
    pub struct Listener {
        address: String,
        pending: Option<NamedPipeServer>,
    }

    impl Listener {
        pub async fn bind(endpoint: &Endpoint) -> anyhow::Result<Self> {
            // `first_pipe_instance` makes a second daemon fail here rather than
            // silently sharing the name and stealing connections.
            let pending = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&endpoint.address)
                .map_err(|e| anyhow::anyhow!("an agent may already be running: {e}"))?;

            Ok(Self { address: endpoint.address.clone(), pending: Some(pending) })
        }

        pub async fn accept(&mut self) -> anyhow::Result<Stream> {
            let server = self
                .pending
                .take()
                .ok_or_else(|| anyhow::anyhow!("listener is not armed"))?;

            server.connect().await?;
            // Arm the next instance immediately, so there is no gap in which a
            // client would find nothing listening.
            self.pending = Some(ServerOptions::new().create(&self.address)?);
            Ok(server)
        }
    }

    pub async fn connect(endpoint: &Endpoint) -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
        Ok(ClientOptions::new().open(&endpoint.address)?)
    }
}

#[cfg(unix)]
pub use imp::Stream;
/// The client side of a connection. On Windows this differs from the server type.
#[cfg(windows)]
pub type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

pub use imp::{Listener, connect};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_version_stamped() {
        // A newer client talking to an older daemon would surface as corrupt
        // terminal output, so they never share an address.
        let endpoint = Endpoint::for_version("9.9.9").unwrap();
        assert!(endpoint.address.contains("9.9.9"), "got {}", endpoint.address);
    }

    #[cfg(unix)]
    #[test]
    fn the_unix_endpoint_stays_within_the_socket_path_limit() {
        // sun_path is capped near 104 bytes on macOS and exceeding it fails at
        // bind with an error that never mentions length.
        let endpoint = Endpoint::for_version(env!("CARGO_PKG_VERSION")).unwrap();
        assert!(
            endpoint.address.len() < 100,
            "socket path too long: {}",
            endpoint.address
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_endpoint_is_per_user() {
        // Pipes share one global namespace; without the user in the name, two
        // people on a machine would share a daemon and each other's shells.
        let endpoint = Endpoint::for_version("1.0.0").unwrap();
        assert!(endpoint.address.starts_with(r"\\.\pipe\"));
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_owned());
        assert!(endpoint.address.contains(&user));
    }
}
