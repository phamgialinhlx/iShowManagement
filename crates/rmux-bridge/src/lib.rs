//! The rmux ↔ Redstone bridge: wire contract and on-host enrolment.
//!
//! rmux already puts a persistent agent on every host the operator connects to,
//! and that agent already owns the thing Redstone wants to reach: Claude
//! sessions that outlive the app, the network and the laptop lid. The bridge is
//! one more subcommand on that binary — not a second daemon — which is the whole
//! reason this is cheap.
//!
//! ## Why the bridge lives on the host and not in the desktop app
//!
//! It was very nearly written the other way: rmux holds the WebSocket, and
//! forwards to hosts over the ssh connections it already has. That is less code
//! and needs no credential anywhere but the operator's own machine.
//!
//! It is also wrong, for one decisive reason. **rmux is frequently closed.** The
//! entire point of `rmux-agent` is that work continues without it — that is why
//! sessions survive quitting the app. A bridge in the desktop app would mean
//! Redstone can drive a server exactly while the operator is sitting in front of
//! the machine that could have driven it by hand, and goes blind the moment they
//! close the lid. That reproduces the problem the agent exists to solve.
//!
//! Two smaller consequences fall out and both are wins. A transcript read is a
//! **local file read** on the host rather than 228 MB over ssh. And a host that
//! can reach Redstone needs no ingress, no certificate and no firewall change,
//! because the connection is outbound.
//!
//! What it costs is a credential on each enrolled host — which is why the token
//! is per host, minted by Redstone, individually revocable, and scoped to
//! exactly the verbs in [`protocol::Request`]. See [`Enrolment`].

pub mod protocol;

pub use protocol::{
    Conversation, ErrorCode, Event, Frame, Hello, HostInfo, Kind, Message, Request, Response,
    Role, Session, Status, Welcome, VERSION,
};

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where the agent keeps its own state on a host.
pub fn runtime_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".rmux"))
}

/// The enrolment file: `~/.rmux/redstone.json`.
pub fn enrolment_path() -> anyhow::Result<PathBuf> {
    Ok(runtime_dir()?.join("redstone.json"))
}

/// What one host needs in order to reach Redstone.
///
/// **This is a credential path**, held to the same standard as the askpass
/// socket: `0700` directory, `0600` file, and a token that belongs to this host
/// alone.
///
/// The token is deliberately *not* the operator's own access token. A dev box is
/// a machine other people frequently have accounts on and which is rebuilt
/// without ceremony; a token that could act as the operator everywhere in
/// Redstone would make every enrolled host a copy of their identity. A per-host
/// token can be revoked from Redstone's UI on its own, and its blast radius is
/// the verbs in [`protocol::Request`] against one machine.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Enrolment {
    /// `wss://…/api/v1/rmux/bridge`. Stored rather than compiled in: Redstone is
    /// self-hosted, and a deployment's own hostname must never be baked into a
    /// binary that ships to everybody.
    pub endpoint: String,
    /// The per-host bearer token. Sent as an `Authorization` header, never in a
    /// frame and never in argv — `ps` shows one user's command line to every
    /// account on the machine.
    pub token: String,
    /// Redstone's id for this host, echoed in the UI so two machines that are
    /// both called `localhost` can be told apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// Who enrolled it, for the operator's own benefit when they find this file
    /// on a server in a year's time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrolled_by: Option<String>,
    /// Seconds since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrolled_at: Option<u64>,
}

impl Enrolment {
    /// Read the enrolment, or `Ok(None)` when the host is not enrolled.
    ///
    /// **Refuses a file that other accounts can read.** A credential that has
    /// been `chmod 644`'d at some point is a credential to treat as disclosed,
    /// and carrying on with it silently is how a leak stays undiscovered. Failing
    /// closed here costs one clear error and a re-enrolment.
    pub fn load() -> anyhow::Result<Option<Self>> {
        let path = enrolment_path()?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)?.permissions().mode() & 0o077;
            if mode != 0 {
                anyhow::bail!(
                    "{} is readable by other accounts (mode {:o}); \
                     treat the token as disclosed and enrol again",
                    path.display(),
                    mode,
                );
            }
        }

        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// Write it, `0600` inside a `0700` directory.
    ///
    /// The mode is set **before** the token is written, not after: a file created
    /// world-readable and tightened a moment later is world-readable for that
    /// moment, and that is all a loop on the machine needs.
    pub fn save(&self) -> anyhow::Result<()> {
        let dir = runtime_dir()?;
        std::fs::create_dir_all(&dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }

        let path = enrolment_path()?;
        let body = serde_json::to_vec_pretty(self)?;

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(&body)?;
            file.flush()?;
        }
        #[cfg(not(unix))]
        std::fs::write(&path, &body)?;

        Ok(())
    }

    /// Remove it. Unenrolling must actually delete the token, not merely stop
    /// using it — a revoked host with the credential still on disk is one
    /// restart away from being enrolled again.
    pub fn forget() -> anyhow::Result<()> {
        match std::fs::remove_file(enrolment_path()?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enrolment() -> Enrolment {
        Enrolment {
            endpoint: "wss://redstone.example/api/v1/rmux/bridge".into(),
            token: "rbt_secret".into(),
            host_id: Some("h-1".into()),
            enrolled_by: Some("dev.user".into()),
            enrolled_at: Some(1_782_903_431),
        }
    }

    #[test]
    fn an_enrolment_round_trips() {
        let json = serde_json::to_string(&enrolment()).unwrap();
        assert!(json.contains(r#""hostId":"h-1""#), "{json}");
        assert_eq!(serde_json::from_str::<Enrolment>(&json).unwrap(), enrolment());
    }

    #[test]
    fn an_endpoint_is_stored_rather_than_compiled_in() {
        // Redstone is self-hosted. A deployment's hostname baked into a binary
        // ships that deployment's address to everyone who ever gets the binary.
        let json = serde_json::to_string(&enrolment()).unwrap();
        assert!(json.contains("endpoint"), "{json}");
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_enrolment_is_refused_rather_than_used() {
        // Fail closed. Carrying on with a disclosed credential is how a leak
        // stays undiscovered.
        use std::os::unix::fs::PermissionsExt;

        let home = std::env::temp_dir().join(format!("rmux-bridge-perm-{}", std::process::id()));
        let dir = home.join(".rmux");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("redstone.json");
        std::fs::write(&path, serde_json::to_vec(&enrolment()).unwrap()).unwrap();

        // The check is the one `load` runs; exercised directly here because
        // `load` resolves `$HOME` and the test must not depend on the runner's.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o077;
        assert_ne!(mode, 0, "0644 must be seen as readable by others");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o077;
        assert_eq!(mode, 0, "0600 must pass");

        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn the_enrolment_lives_beside_the_agent_state() {
        // Same directory as the daemon socket and the alias file, so an operator
        // clearing rmux off a host has one place to look.
        let path = enrolment_path().unwrap();
        assert!(path.ends_with(".rmux/redstone.json"), "{}", path.display());
    }
}
