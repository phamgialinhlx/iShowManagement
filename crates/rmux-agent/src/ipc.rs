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
/// **Stamped with the build, not just the version — and that distinction cost a
/// day.** The socket used to be `agent-<version>.sock`, which is wrong for the
/// one case that happens constantly: two builds sharing a version. Every dev
/// build is `0.1.0`.
///
/// What that produced, on a real host: the fixed agent installed correctly under
/// its own fingerprinted path and ran as the *client*, then connected to a
/// daemon from seventeen hours earlier that was still holding the socket. The
/// daemon is what spawns the shell, so it kept using its own compiled-in code —
/// the version without `-i` — and `claude` stayed "command not found" through
/// three rebuilds. Nothing looked wrong anywhere: the binary on the host was new,
/// its fingerprint was right, and the fix was genuinely in it. It was simply
/// never the process doing the work.
///
/// So the socket now carries the same content fingerprint the install path
/// already carried. A changed agent gets a changed socket, starts its own
/// daemon, and cannot inherit an older one. The old daemon keeps serving the
/// sessions already attached to it until they end, which is the right outcome —
/// upgrading must not kill work in progress.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Unix: the socket path. Windows: the pipe name.
    pub address: String,
    /// Unix only: the directory to create and lock down.
    pub parent: Option<PathBuf>,
}

/// The socket's basename, from the version and the running binary's own name.
///
/// Pure so the rule can be tested without installing anything. `provision`
/// installs as `rmux-agent-<version>-<fingerprint>`, so the stem *is* the
/// identity — no hashing at startup, and the client and the daemon agree by
/// construction because `spawn_daemon` launches `current_exe`.
fn socket_stem(version: &str, exe_stem: Option<&str>) -> String {
    match exe_stem {
        // An installed, fingerprinted agent. The `rmux-` prefix is dropped: the
        // socket already lives in `~/.rmux`, and those five characters are not
        // free. `sun_path` caps a Unix socket near 104 bytes, the sandbox in
        // `tests/persistence.rs` sits inside a long `/var/folders/...` temp
        // path, and keeping the prefix pushed it over — `bind` then fails with
        // an error that never mentions length, and the shell simply never
        // starts. Caught by that test going red.
        Some(stem) if stem.starts_with("rmux-agent-") => {
            stem.strip_prefix("rmux-").unwrap_or(stem).to_owned()
        }
        // A bare `rmux-agent` — a dev build run straight out of `target/`, where
        // there is no fingerprint to use and sharing one daemon is what you
        // want anyway.
        _ => format!("agent-{version}"),
    }
}

/// This process's own file name, if it can be read.
///
/// **`file_name`, never `file_stem`.** The installed name is
/// `rmux-agent-0.1.0-<fingerprint>`, and `file_stem` reads `.0-<fingerprint>` as
/// an extension and throws it away — leaving `rmux-agent-0.1`, which is the same
/// for every build of 0.1.x and so reintroduces the collision this whole change
/// exists to remove. Caught on a real host: the socket came out as
/// `rmux-agent-0.1.sock`. It differed from the old name by luck, not by design.
///
/// Only a literal `.exe` is stripped, for Windows.
fn current_stem() -> Option<String> {
    let path = std::env::current_exe().ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(name.strip_suffix(".exe").unwrap_or(&name).to_owned())
}

impl Endpoint {
    /// The endpoint for the binary that is running right now.
    pub fn current(version: &str) -> anyhow::Result<Self> {
        Self::named(&socket_stem(version, current_stem().as_deref()))
    }

    fn named(stem: &str) -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
            let dir = home.join(".rmux");
            Ok(Self {
                address: dir.join(format!("{stem}.sock")).to_string_lossy().into_owned(),
                parent: Some(dir),
            })
        }
        #[cfg(windows)]
        {
            // Pipes live in a global namespace, so the name carries the user —
            // otherwise two people on one machine would share a daemon, and with
            // it each other's shells.
            let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_owned());
            Ok(Self { address: format!(r"\\.\pipe\{stem}-{user}"), parent: None })
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
    fn two_builds_of_one_version_do_not_share_a_daemon() {
        // **The bug this exists for.** Every dev build is 0.1.0, so a
        // version-only socket meant a rebuilt agent installed under a new
        // fingerprinted path, ran as the client, and then connected to the
        // *previous* build's daemon — which is the process that actually spawns
        // the shell. A fix could therefore be present on the host, in the right
        // binary, and still never run. Verified on a real host: the daemon was
        // seventeen hours older than the client attached to it.
        let old = socket_stem("0.1.0", Some("rmux-agent-0.1.0-16fdf898f1317da9"));
        let new = socket_stem("0.1.0", Some("rmux-agent-0.1.0-efe045f9a5aa5db0"));
        assert_ne!(old, new, "a changed build must not inherit the old daemon");
        assert!(old.contains("16fdf898f1317da9"), "{old}");
    }

    #[test]
    fn the_same_build_still_shares_one_daemon() {
        // The other half: reattaching must find the daemon it left, or every
        // reconnect would strand the previous one holding live shells.
        assert_eq!(
            socket_stem("0.1.0", Some("rmux-agent-0.1.0-abc123")),
            socket_stem("0.1.0", Some("rmux-agent-0.1.0-abc123")),
        );
    }

    #[test]
    fn a_dotted_version_does_not_swallow_the_fingerprint() {
        // `file_stem` would read `.0-<fingerprint>` as an extension and drop it,
        // collapsing every 0.1.x build back onto one socket — the collision this
        // change removes. Observed on a real host as `rmux-agent-0.1.sock`.
        let stem = socket_stem("0.1.0", Some("rmux-agent-0.1.0-bab16edd2b66591e"));
        assert!(stem.ends_with("bab16edd2b66591e"), "fingerprint lost: {stem}");
    }

    #[test]
    fn a_dev_build_falls_back_to_the_version() {
        // Run straight out of `target/` there is no fingerprint in the name, and
        // sharing one daemon is the wanted behaviour there.
        assert_eq!(socket_stem("0.1.0", Some("rmux-agent")), "agent-0.1.0");
        assert_eq!(socket_stem("0.1.0", None), "agent-0.1.0");
    }

    #[test]
    fn the_endpoint_is_still_version_stamped() {
        // A newer client talking to an older daemon would surface as corrupt
        // terminal output, so they never share an address.
        let stem = socket_stem("9.9.9", None);
        assert!(stem.contains("9.9.9"), "got {stem}");
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_name_leaves_room_for_a_long_home() {
        // `sun_path` is capped near 104 bytes *including the directory*, and the
        // test sandbox lives under `/var/folders/…/T/rmux-agent-test-…`, which
        // is already ~90 of them. The name itself therefore has to stay short —
        // this is the budget, checked directly, because the failure mode is a
        // `bind` error that never mentions length.
        let stem = socket_stem("0.1.0", Some("rmux-agent-0.1.0-565cb104276563fc"));
        assert!(stem.len() + ".sock".len() <= 34, "socket name too long: {stem}");
    }

    #[cfg(unix)]
    #[test]
    fn the_unix_endpoint_stays_within_the_socket_path_limit() {
        // sun_path is capped near 104 bytes on macOS and exceeding it fails at
        // bind with an error that never mentions length. The fingerprint made
        // this name meaningfully longer, so it is checked against the real
        // installed shape rather than the short dev one.
        let stem = socket_stem(env!("CARGO_PKG_VERSION"), Some("rmux-agent-0.1.0-efe045f9a5aa5db0"));
        let endpoint = Endpoint::named(&stem).unwrap();
        assert!(endpoint.address.len() < 100, "socket path too long: {}", endpoint.address);
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_endpoint_is_per_user() {
        // Pipes share one global namespace; without the user in the name, two
        // people on a machine would share a daemon and each other's shells.
        let endpoint = Endpoint::named(&socket_stem("1.0.0", None)).unwrap();
        assert!(endpoint.address.starts_with(r"\\.\pipe\"));
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_owned());
        assert!(endpoint.address.contains(&user));
    }
}
