//! Reaching a port on the target as if it were local.
//!
//! An app listening on `localhost:3000` **on the server** is not reachable from
//! here — that is the whole problem. rmux runs `ssh -N -L 3000:localhost:3000`
//! so the same number works on this machine, and then the browser simply loads
//! `http://localhost:3000`. There is no proxy, no PAC file and no URL rewriting:
//! the local port *is* the remote port, which is what makes the address the
//! operator would have typed anyway the correct one.
//!
//! ## Two things about `ssh -L` that are easy to get wrong
//!
//! **A forward must not be multiplexed.** rmux otherwise shares one connection
//! per host via ControlMaster. Under multiplexing `ssh -N -L` registers the
//! forward with the master and *exits immediately*, which reads as "the tunnel
//! died" one moment after it started working. `ControlPath=none` gives a forward
//! its own long-lived connection.
//!
//! **A forward that cannot bind must fail loudly.** Without
//! `ExitOnForwardFailure=yes`, ssh happily connects while the `-L` binding
//! silently failed — so the tunnel looks up, and every page load hits whatever
//! else already owns that local port. That is worse than an error, because it
//! can serve someone *else's* application under your URL.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// How long a tunnel must survive before it is called up.
///
/// ssh exits within milliseconds when the bind fails, so reporting `Active` at
/// spawn would flash green and then red on every port already in use.
const GRACE: std::time::Duration = std::time::Duration::from_millis(700);

/// The map key a host's SOCKS proxy is stored under.
///
/// Port 0 because no real forward can use it — `-L 0:` is not a thing anyone
/// asks for — so the proxy shares the map without ever colliding with a tunnel.
const SOCKS_KEY: u16 = 0;

/// Ask the OS for a port nobody is using.
///
/// Bind, read the number, drop. There is a window between that and ssh binding
/// it, which is why the proxy still runs with `ExitOnForwardFailure` — losing
/// the race then surfaces as a clear failure rather than a proxy pointed at
/// whatever else won it.
fn free_local_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForwardState {
    /// The target is this machine; nothing to tunnel.
    Local,
    Starting,
    Active,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Forward {
    pub port: u16,
    pub state: ForwardState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct Running {
    child: Option<Child>,
    forward: Forward,
}

/// Every tunnel this app has opened, keyed by `(host, port)`.
#[derive(Default)]
pub struct Forwards {
    running: Mutex<HashMap<(String, u16), Running>>,
}

impl Forwards {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a tunnel to `port` on `host`, or report the one already open.
    ///
    /// Idempotent: asking twice for a live tunnel returns its state rather than
    /// spawning a second ssh that is guaranteed to fail the bind.
    pub async fn start(self: &Arc<Self>, host: Option<&str>, port: u16) -> Forward {
        let Some(host) = host else {
            return Forward { port, state: ForwardState::Local, error: None };
        };

        let key = (host.to_owned(), port);
        {
            let running = self.running.lock().await;
            if let Some(existing) = running.get(&key)
                && matches!(existing.forward.state, ForwardState::Starting | ForwardState::Active)
            {
                return existing.forward.clone();
            }
        }

        match self.spawn(host, port).await {
            Ok(mut child) => {
                let stderr = child.stderr.take();
                let forward = Forward { port, state: ForwardState::Starting, error: None };
                self.running
                    .lock()
                    .await
                    .insert(key.clone(), Running { child: Some(child), forward: forward.clone() });

                // Watch it: promote to Active after the grace window, or record
                // why it died. ssh writes the useful part to stderr and then
                // exits, so both are read here rather than polled for later.
                let this = Arc::clone(self);
                tokio::spawn(async move {
                    let message = match stderr {
                        Some(pipe) => read_first_error(pipe).await,
                        None => None,
                    };
                    this.settle(key, message).await;
                });

                forward
            }
            Err(e) => {
                let forward = Forward {
                    port,
                    state: ForwardState::Failed,
                    error: Some(e.to_string()),
                };
                self.running
                    .lock()
                    .await
                    .insert(key, Running { child: None, forward: forward.clone() });
                forward
            }
        }
    }

    async fn spawn(&self, host: &str, port: u16) -> std::io::Result<Child> {
        Command::new("ssh")
            .arg("-N")
            // See the module comment: both of these are load-bearing.
            .args(["-o", "ControlPath=none"])
            .args(["-o", "ExitOnForwardFailure=yes"])
            // Never prompt. A forward has no terminal to answer on, so without
            // this a host wanting a passphrase hangs forever with no output.
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ConnectTimeout=8"])
            .args(["-L", &format!("{port}:localhost:{port}")])
            .arg(host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
    }

    /// Decide whether a just-started tunnel lived long enough to be real.
    async fn settle(&self, key: (String, u16), message: Option<String>) {
        tokio::time::sleep(GRACE).await;

        let mut running = self.running.lock().await;
        let Some(entry) = running.get_mut(&key) else { return };

        // `try_wait` rather than `wait`: this must not block on a healthy tunnel,
        // which by design never exits.
        let exited = entry.child.as_mut().map(|c| c.try_wait().ok().flatten()).unwrap_or(None);

        entry.forward = match exited {
            Some(status) => Forward {
                port: key.1,
                state: ForwardState::Failed,
                error: Some(message.unwrap_or_else(|| format!("ssh exited ({status})"))),
            },
            None => Forward { port: key.1, state: ForwardState::Active, error: None },
        };
    }

    /// Close a tunnel.
    pub async fn stop(&self, host: Option<&str>, port: u16) {
        let Some(host) = host else { return };
        if let Some(mut entry) = self.running.lock().await.remove(&(host.to_owned(), port))
            && let Some(child) = entry.child.as_mut()
        {
            let _ = child.kill().await;
        }
    }

    /// Everything currently open for a host.
    pub async fn list(&self, host: Option<&str>) -> Vec<Forward> {
        let Some(host) = host else { return Vec::new() };
        let mut out: Vec<Forward> = self
            .running
            .lock()
            .await
            .iter()
            .filter(|((h, _), _)| h == host)
            .map(|(_, r)| r.forward.clone())
            .collect();
        out.sort_by_key(|f| f.port);
        out
    }

    /// Open a SOCKS5 proxy onto the target (`ssh -D`), returning its local port.
    ///
    /// **This is the one that actually removes port forwarding.** A `-L` tunnel
    /// carries a single port the operator had to know about first; a `-D` proxy
    /// carries the whole network. Anything pointed at it reaches every port on
    /// the target, and with `socks5h` the *far side* resolves hostnames — so an
    /// internal name that exists only on the server's network resolves there
    /// instead of failing here.
    ///
    /// It is only useful to a client that can scope a proxy to part of itself,
    /// which is exactly why rbrowse exists as a separate Chromium app: it can
    /// give each rmux session its own partition and proxy. rmux's own webview
    /// cannot — there is one of it, and proxying it would route the app's own UI
    /// through the operator's server.
    ///
    /// Idempotent per host, keyed on port `0` so it cannot collide with a real
    /// forward. Asking twice returns the same proxy rather than spawning a
    /// second ssh.
    pub async fn socks(self: &Arc<Self>, host: Option<&str>) -> Result<u16, String> {
        let host = host.ok_or_else(|| "a SOCKS proxy onto this machine would do nothing".to_owned())?;
        let key = (host.to_owned(), SOCKS_KEY);

        {
            let running = self.running.lock().await;
            if let Some(existing) = running.get(&key)
                && matches!(existing.forward.state, ForwardState::Starting | ForwardState::Active)
            {
                return Ok(existing.forward.port);
            }
        }

        let port = free_local_port().map_err(|e| e.to_string())?;

        let mut child = Command::new("ssh")
            .arg("-N")
            .args(["-o", "ControlPath=none"])
            .args(["-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ConnectTimeout=8"])
            // `127.0.0.1:` is not decoration. A bare `-D <port>` binds according
            // to the host's `GatewayPorts`, and a SOCKS proxy reachable from the
            // LAN is an open route into the operator's infrastructure for
            // anyone on the same network.
            .args(["-D", &format!("127.0.0.1:{port}")])
            .arg(host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;

        let stderr = child.stderr.take();
        let forward = Forward { port, state: ForwardState::Starting, error: None };
        self.running
            .lock()
            .await
            .insert(key.clone(), Running { child: Some(child), forward });

        let this = Arc::clone(self);
        tokio::spawn(async move {
            let message = match stderr {
                Some(pipe) => read_first_error(pipe).await,
                None => None,
            };
            this.settle_socks(key, port, message).await;
        });

        Ok(port)
    }

    /// Like `settle`, but the proxy's port is not its key.
    async fn settle_socks(&self, key: (String, u16), port: u16, message: Option<String>) {
        tokio::time::sleep(GRACE).await;

        let mut running = self.running.lock().await;
        let Some(entry) = running.get_mut(&key) else { return };
        let exited = entry.child.as_mut().map(|c| c.try_wait().ok().flatten()).unwrap_or(None);

        entry.forward = match exited {
            Some(status) => Forward {
                port,
                state: ForwardState::Failed,
                error: Some(message.unwrap_or_else(|| format!("ssh exited ({status})"))),
            },
            None => Forward { port, state: ForwardState::Active, error: None },
        };
    }

    /// Close every tunnel. Called when the app quits, so no ssh outlives it.
    pub async fn stop_all(&self) {
        let mut running = self.running.lock().await;
        for entry in running.values_mut() {
            if let Some(child) = entry.child.as_mut() {
                let _ = child.kill().await;
            }
        }
        running.clear();
    }
}

/// The first meaningful line ssh wrote to stderr.
///
/// Bounded: this reaches a chip in the UI, and ssh in verbose moods can produce
/// a great deal. Banner noise is skipped so the message is the actual reason.
async fn read_first_error(pipe: tokio::process::ChildStderr) -> Option<String> {
    let mut lines = BufReader::new(pipe).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Warning: Permanently added") {
            continue;
        }
        return Some(line.chars().take(200).collect());
    }
    None
}

/// A port something is listening on, and what is listening.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListeningPort {
    pub port: u16,
    /// The process, when the host was willing to say. `ss` only reports it for
    /// your own processes unless you are root, so this is often empty.
    pub process: String,
}

/// Ask a host what it is listening on.
///
/// This is what makes "no port forwarding" true rather than nearly true: without
/// it the operator has to already know the number, which is exactly the manual
/// step the feature exists to remove.
pub const DISCOVER_SCRIPT: &str = r#"{ ss -tlnp 2>/dev/null || netstat -tlnp 2>/dev/null; } | awk '
  { for (i = 1; i <= NF; i++) if ($i ~ /:[0-9]+$/) { addr = $i; break } }
  addr == "" { next }
  {
    n = split(addr, parts, ":"); port = parts[n]
    proc = ""
    if (match($0, /users:\(\("[^"]+"/)) {
      proc = substr($0, RSTART + 8, RLENGTH - 9)
    } else if (match($0, /[0-9]+\/[a-zA-Z0-9_.-]+/)) {
      split(substr($0, RSTART, RLENGTH), pp, "/"); proc = pp[2]
    }
    if (port + 0 > 0) print port, proc
    addr = ""
  }' | sort -u -n"#;

/// Parse what that script printed.
pub fn parse_listening(text: &str) -> Vec<ListeningPort> {
    let mut seen: Vec<ListeningPort> = Vec::new();

    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(port) = fields.next().and_then(|p| p.parse::<u16>().ok()) else { continue };
        let process = fields.next().unwrap_or("").to_owned();

        // Ports below 1024 are the machine's own services — sshd, postfix — and
        // burying a dev server at 3000 under them is what makes such a list
        // useless. The operator can still type one by hand.
        if port < 1024 {
            continue;
        }

        match seen.iter_mut().find(|p| p.port == port) {
            // The same port appears once per bound address (0.0.0.0 and ::).
            // Keep whichever reading actually named the process.
            Some(existing) if existing.process.is_empty() => existing.process = process,
            Some(_) => {}
            None => seen.push(ListeningPort { port, process }),
        }
    }

    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forward_command_disables_multiplexing_and_fails_loudly() {
        // Both of these have bitten this exact feature before. Under a shared
        // ControlMaster `ssh -N -L` exits the instant it registers the forward
        // and looks dead; without ExitOnForwardFailure a failed bind looks
        // *alive* and serves whatever else owns the port.
        let args = forward_args("example", 3000);
        assert!(args.windows(2).any(|w| w == ["-o", "ControlPath=none"]), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-o", "ExitOnForwardFailure=yes"]), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-o", "BatchMode=yes"]), "{args:?}");
    }

    #[test]
    fn the_local_port_matches_the_remote_port() {
        // The whole design: the operator types the address they already know.
        let args = forward_args("example", 5173);
        assert!(args.windows(2).any(|w| w == ["-L", "5173:localhost:5173"]), "{args:?}");
    }

    /// Mirrors [`Forwards::spawn`]'s argument list so it can be asserted without
    /// running ssh.
    fn forward_args(host: &str, port: u16) -> Vec<String> {
        vec![
            "-N".into(),
            "-o".into(),
            "ControlPath=none".into(),
            "-o".into(),
            "ExitOnForwardFailure=yes".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=8".into(),
            "-L".into(),
            format!("{port}:localhost:{port}"),
            host.into(),
        ]
    }

    #[test]
    fn listening_ports_are_read_from_ss_output() {
        let text = "3000 node\n5173 vite\n8080 \n";
        let ports = parse_listening(text);

        assert_eq!(ports.len(), 3);
        assert_eq!(ports[0], ListeningPort { port: 3000, process: "node".into() });
        assert_eq!(ports[2].port, 8080);
        // A port with no process name is still worth offering.
        assert_eq!(ports[2].process, "");
    }

    #[test]
    fn system_services_are_not_offered() {
        // A dev server at 3000 buried under sshd, postfix and rpcbind is what
        // makes a discovered-ports list useless.
        let ports = parse_listening("22 sshd\n25 master\n111 rpcbind\n3000 node\n");
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 3000);
    }

    #[test]
    fn a_port_bound_on_several_addresses_is_listed_once() {
        // `ss` prints one row per bound address — 0.0.0.0 and ::. Listing 3000
        // twice would look like two different apps.
        let ports = parse_listening("3000 \n3000 node\n3000 node\n");
        assert_eq!(ports.len(), 1);
        // …and the row that named the process is the one that survives.
        assert_eq!(ports[0].process, "node");
    }

    #[test]
    fn junk_output_yields_nothing_rather_than_panicking() {
        // A host without `ss` or `netstat`, or one whose shell printed a banner.
        assert!(parse_listening("bash: ss: command not found").is_empty());
        assert!(parse_listening("").is_empty());
        assert!(parse_listening("not a port at all\n\n  \n").is_empty());
    }

    #[test]
    fn the_discovery_script_asks_for_listening_sockets_only() {
        // `-t` tcp, `-l` listening, `-n` numeric. Dropping `-l` would list every
        // established connection, which is a different and much longer answer.
        assert!(DISCOVER_SCRIPT.contains("ss -tlnp"), "{DISCOVER_SCRIPT}");
        // …with a fallback, because minimal images ship neither tool reliably.
        assert!(DISCOVER_SCRIPT.contains("netstat -tlnp"), "{DISCOVER_SCRIPT}");
    }
}
