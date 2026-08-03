//! Connection multiplexing via OpenSSH's `ControlMaster`.
//!
//! One master `ssh` process authenticates and holds the transport open; every
//! later `ssh` to the same host connects to its Unix socket instead of doing a
//! fresh handshake. Opening a fifth terminal on a host therefore costs a process
//! spawn and nothing else — no key exchange, no re-auth, no 2FA prompt.
//!
//! We deliberately use `ControlPersist=no` so the master dies with rmux rather
//! than lingering in the user's session after the app quits.

use std::path::PathBuf;
use std::process::Stdio;

use parking_lot::Mutex;
use rmux_transport::SshHostId;
use tokio::process::{Child, Command};

/// Where the master stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MasterState {
    Stopped,
    Starting,
    Running,
    /// Multiplexing is unavailable on this platform; each command dials its own
    /// connection.
    Unsupported,
}

#[derive(Debug)]
pub struct ControlMaster {
    host: SshHostId,
    socket: PathBuf,
    state: Mutex<MasterState>,
    child: Mutex<Option<Child>>,
}

impl ControlMaster {
    pub fn new(host: SshHostId) -> Self {
        let socket = control_path_for(&host);
        let state =
            if Self::is_supported() { MasterState::Stopped } else { MasterState::Unsupported };
        Self { host, socket, state: Mutex::new(state), child: Mutex::new(None) }
    }

    /// Whether OpenSSH connection multiplexing works here.
    ///
    /// `ControlMaster` requires Unix domain sockets, which Windows OpenSSH does
    /// not provide — it fails with "getsockname failed". On Windows every command
    /// opens its own connection until the `russh` path lands.
    pub const fn is_supported() -> bool {
        !cfg!(windows)
    }

    pub fn state(&self) -> MasterState {
        *self.state.lock()
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket
    }

    /// `ssh` options every client invocation needs so it finds the master.
    pub fn client_options(&self) -> Vec<String> {
        if !Self::is_supported() {
            return Vec::new();
        }
        vec![
            "-o".to_owned(),
            format!("ControlPath={}", self.socket.display()),
            // Never start a master implicitly from a client command — that would
            // race several terminals opened at once into competing masters.
            "-o".to_owned(),
            "ControlMaster=no".to_owned(),
        ]
    }

    /// Start the master if it is not already up.
    pub async fn ensure_started(&self) -> anyhow::Result<MasterState> {
        if !Self::is_supported() {
            return Ok(MasterState::Unsupported);
        }

        match self.state() {
            MasterState::Running => return Ok(MasterState::Running),
            MasterState::Starting => {
                anyhow::bail!("control master for {} is already starting", self.host.alias)
            }
            _ => {}
        }

        // Piggyback on a master the user already has open (from their own shell,
        // or a previous rmux run) rather than starting a competing one.
        if self.check_alive().await {
            *self.state.lock() = MasterState::Running;
            return Ok(MasterState::Running);
        }

        // The socket exists but nothing answered, so it is debris from a run that
        // died without cleaning up — a crash, a kill -9, a machine that slept.
        //
        // This must be removed before starting a master, and the failure mode if
        // it is not is nasty: OpenSSH will not bind over an existing socket file.
        // It prints "ControlSocket ... already exists, disabling multiplexing"
        // and then connects *anyway* without multiplexing, so the poll below
        // never sees a live master and spins for the full timeout before failing.
        // The symptom is every host hanging for a minute after any unclean exit.
        self.clear_stale_socket().await;

        *self.state.lock() = MasterState::Starting;

        if let Some(dir) = self.socket.parent() {
            tokio::fs::create_dir_all(dir).await.ok();
        }

        let mut cmd = Command::new("ssh");
        cmd.arg("-N") // no remote command; this process only holds the transport
            .arg("-o")
            .arg(format!("ControlPath={}", self.socket.display()))
            .arg("-o")
            .arg("ControlMaster=yes")
            // Die with rmux instead of outliving it in the user's session.
            .arg("-o")
            .arg("ControlPersist=no")
            // Notice a dead link in ~45s rather than hanging on a half-open socket.
            .arg("-o")
            .arg("ServerAliveInterval=15")
            .arg("-o")
            .arg("ServerAliveCountMax=3");

        if let Some(user) = &self.host.user {
            cmd.arg("-l").arg(user);
        }
        if let Some(port) = self.host.port {
            cmd.arg("-p").arg(port.to_string());
        }
        cmd.arg(&self.host.alias);

        for (k, v) in super::askpass::env_for_gui_prompts() {
            cmd.env(k, v);
        }

        // stdin must stay open: closing it makes OpenSSH exit immediately when a
        // ProxyJump is in play.
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());

        let child = cmd.spawn()?;
        *self.child.lock() = Some(child);

        // The socket appears asynchronously, after authentication completes.
        // Poll rather than sleeping a fixed amount so 2FA has room to finish.
        for _ in 0..600 {
            if self.socket.exists() && self.check_alive().await {
                *self.state.lock() = MasterState::Running;
                return Ok(MasterState::Running);
            }
            if let Some(child) = self.child.lock().as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                *self.state.lock() = MasterState::Stopped;
                anyhow::bail!("ssh master for {} exited early ({status})", self.host.alias);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        *self.state.lock() = MasterState::Stopped;
        anyhow::bail!("timed out waiting for ssh master to {}", self.host.alias)
    }

    /// Ask OpenSSH whether the master is live (`ssh -O check`).
    pub async fn check_alive(&self) -> bool {
        if !Self::is_supported() || !self.socket.exists() {
            return false;
        }
        Command::new("ssh")
            .arg("-O")
            .arg("check")
            .arg("-o")
            .arg(format!("ControlPath={}", self.socket.display()))
            .arg(&self.host.alias)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Remove a socket file left behind by a dead master.
    ///
    /// Only ever called once [`check_alive`] has reported nothing listening, so
    /// this cannot delete a socket another process is still using.
    ///
    /// [`check_alive`]: ControlMaster::check_alive
    pub(crate) async fn clear_stale_socket(&self) -> bool {
        if !self.socket.exists() {
            return false;
        }
        match tokio::fs::remove_file(&self.socket).await {
            Ok(()) => {
                tracing::debug!(socket = %self.socket.display(), "removed a stale control socket");
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not remove a stale control socket");
                false
            }
        }
    }

    /// Tear the master down.
    pub async fn stop(&self) {
        if Self::is_supported() && self.socket.exists() {
            let _ = Command::new("ssh")
                .arg("-O")
                .arg("exit")
                .arg("-o")
                .arg(format!("ControlPath={}", self.socket.display()))
                .arg(&self.host.alias)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.start_kill();
        }
        *self.state.lock() = MasterState::Stopped;
    }
}

impl Drop for ControlMaster {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.start_kill();
            // Killing the master leaves its socket file behind, and that debris
            // is what makes the *next* connection hang. Remove it here as well
            // as on the way in, so an ordinary quit leaves nothing to trip over.
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

/// Socket path for a host.
///
/// Kept short on purpose: a Unix socket path is capped near 104 bytes on macOS,
/// and OpenSSH's own `%C` hash exists for exactly this reason. A long
/// `~/Library/Application Support/...` prefix plus a descriptive host name blows
/// that limit and fails with a confusing bind error.
fn control_path_for(host: &SshHostId) -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
    dir.push(".rmux");
    dir.push("mux");
    dir.push(format!("{}.sock", short_hash(host)));
    dir
}

/// Short stable hash of the full destination (FNV-1a, 64-bit).
fn short_hash(host: &SshHostId) -> String {
    let key = format!(
        "{}|{}|{}",
        host.alias,
        host.user.as_deref().unwrap_or(""),
        host.port.map(|p| p.to_string()).unwrap_or_default()
    );

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_stays_within_the_unix_socket_length_limit() {
        // macOS caps sun_path near 104 bytes; exceeding it fails at bind time
        // with an error that does not mention length.
        let host = SshHostId::new("a-very-long-descriptive-production-hostname.internal.example.com");
        let path = control_path_for(&host);
        assert!(path.to_string_lossy().len() < 100, "control path too long: {}", path.display());
    }

    #[test]
    fn distinct_destinations_get_distinct_sockets() {
        let a = control_path_for(&SshHostId::new("host-a"));
        let b = control_path_for(&SshHostId::new("host-b"));
        assert_ne!(a, b);

        // Same alias but a different user is a different authenticated session
        // and must not share a master.
        let root = control_path_for(&SshHostId {
            alias: "host-a".to_owned(),
            user: Some("root".to_owned()),
            port: None,
        });
        assert_ne!(a, root);
    }

    #[test]
    fn socket_path_is_stable_across_calls() {
        let host = SshHostId::new("devbox");
        assert_eq!(control_path_for(&host), control_path_for(&host));
    }

    #[test]
    fn client_options_never_start_a_competing_master() {
        let master = ControlMaster::new(SshHostId::new("devbox"));
        let opts = master.client_options();
        if ControlMaster::is_supported() {
            assert!(opts.iter().any(|o| o == "ControlMaster=no"));
            assert!(opts.iter().any(|o| o.starts_with("ControlPath=")));
        } else {
            assert!(opts.is_empty());
        }
    }

    // Binds a Unix socket to produce the debris, so it can only run where there
    // are Unix sockets. ControlMaster is a POSIX-only feature anyway.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_socket_is_cleared_before_starting_a_master() {
        // Reproduces the debris a crashed run leaves: binding a listener creates
        // the socket file, and dropping the listener does NOT remove it.
        let master = ControlMaster::new(SshHostId::new("stale-socket-test"));
        let socket = master.socket_path().clone();

        if let Some(dir) = socket.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        drop(listener);

        assert!(socket.exists(), "dropping a listener should leave the file behind");
        assert!(!master.check_alive().await, "nothing is listening on it");

        // Left in place, OpenSSH would refuse to bind and quietly disable
        // multiplexing, making every connection hang until the timeout.
        assert!(master.clear_stale_socket().await);
        assert!(!socket.exists(), "the stale socket should be gone");

        // Clearing again is harmless — the path is simply absent.
        assert!(!master.clear_stale_socket().await);
    }

    #[tokio::test]
    async fn unsupported_platforms_degrade_instead_of_failing() {
        let master = ControlMaster::new(SshHostId::new("devbox"));
        if !ControlMaster::is_supported() {
            assert_eq!(master.state(), MasterState::Unsupported);
            assert_eq!(master.ensure_started().await.unwrap(), MasterState::Unsupported);
        }
    }
}
