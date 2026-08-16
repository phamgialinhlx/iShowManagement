//! `~/.zmux/state.json` — which servers were connected and which session names
//! were adopted, so the rail repopulates on relaunch.
//!
//! This is not the JSON settings store (that engine stays deferred). Layout/tab
//! restoration is also deferred: only the server list and session names persist,
//! enough that relaunch reconnects and shows the sessions that are still running
//! (plus greyed "gone" entries for names the host no longer reports — clicking
//! one re-attaches, and the daemon's `open_or_attach` revives it under the same
//! name). A slim versioned port of the old `PersistedV3` server/session shape.

use std::io::Write;
use std::path::PathBuf;

use zmux_transport::TargetId;
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;

/// A session kind. Shells are `zmuxd` sessions running a login shell;
/// Claude sessions run `claude`. Persisted so a dead session's row keeps its
/// icon after the host stops reporting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Shell,
    Claude,
}

/// A session name this app has adopted for a server. The name is the agent
/// session key — attach is always by verbatim name, never a derived one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedSession {
    pub name: String,
    pub kind: SessionKind,
    /// Last known working directory, for project grouping when the session is
    /// dead and the host no longer reports a cwd. `None` groups under "(other)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// One connected server and the session names adopted under it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedServer {
    #[serde(flatten)]
    pub target: TargetId,
    #[serde(default)]
    pub sessions: Vec<PersistedSession>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub servers: Vec<PersistedServer>,
}

impl Default for State {
    fn default() -> Self {
        Self { version: VERSION, servers: Vec::new() }
    }
}

fn path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".zmux").join("state.json"))
}

impl State {
    /// Load from disk, or an empty state if the file is missing or unreadable.
    /// A corrupt file is logged and replaced rather than fatal — the rail can
    /// still repopulate from the live host list.
    pub fn load() -> Self {
        let Some(path) = path() else { return Self::default() };
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(state) => state,
                Err(e) => {
                    log::warn!(target: "zmux", "state.json unreadable, starting fresh: {e}");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Atomic write: tmp file in the same dir, then rename. Best-effort — a
    /// failure is logged, not fatal, since the rail's live state is the source
    /// of truth and the next successful change rewrites the file.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        let result = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if let Err(e) = result {
            log::warn!(target: "zmux", "state.json save failed: {e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn server(&self, target: &TargetId) -> Option<&PersistedServer> {
        self.servers.iter().find(|s| &s.target == target)
    }

    pub fn server_mut(&mut self, target: &TargetId) -> Option<&mut PersistedServer> {
        self.servers.iter_mut().find(|s| &s.target == target)
    }

    /// Ensure a server entry exists (connected). Idempotent.
    pub fn add_server(&mut self, target: TargetId) {
        if self.server(&target).is_none() {
            self.servers.push(PersistedServer { target, sessions: Vec::new() });
            self.save();
        }
    }

    pub fn remove_server(&mut self, target: &TargetId) {
        if self.servers.iter().any(|s| &s.target == target) {
            self.servers.retain(|s| &s.target != target);
            self.save();
        }
    }

    /// Record an adopted session name under a server. Idempotent; updates folder
    /// if the session already exists.
    pub fn add_session(&mut self, target: &TargetId, session: PersistedSession) {
        self.add_server(target.clone());
        let server = self.server_mut(target).unwrap();
        if let Some(existing) = server.sessions.iter_mut().find(|s| s.name == session.name) {
            existing.kind = session.kind;
            if session.folder.is_some() {
                existing.folder = session.folder;
            }
        } else {
            server.sessions.push(session);
        }
        self.save();
    }

    pub fn remove_session(&mut self, target: &TargetId, name: &str) {
        if let Some(server) = self.server_mut(target) {
            let before = server.sessions.len();
            server.sessions.retain(|s| s.name != name);
            if server.sessions.len() != before {
                self.save();
            }
        }
    }
}
