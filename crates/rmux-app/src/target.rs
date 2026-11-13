//! Naming a machine across the IPC boundary.
//!
//! Moved here from `src-tauri/src/terminal.rs` (and `TargetInfo` from
//! `src-tauri/src/lib.rs`) so commands in this crate can take a target without
//! depending on the desktop's terminal module. `src-tauri` re-exports both, so
//! every existing `crate::terminal::TargetRef` keeps compiling unchanged.

use rmux_transport::{Platform, SshHostId, Target, TargetId};
use serde::{Deserialize, Serialize};

/// Which machine to open a terminal on.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetRef {
    /// `None` opens on the local machine — rmux as an ordinary IDE.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    /// Absolute local path of a private key the operator chose for this server
    /// (Connect a server › KEY FILE). Offered as one more `-i`, additively.
    ///
    /// **Not part of the target's identity.** [`TargetRef::id`] ignores it, so
    /// `TargetId`, the target caches, the ControlMaster socket and the UI's
    /// `serverId` are all unchanged by picking a key — existing projects and
    /// sessions keep their ids. `#[serde(default)]`, so every `TargetRef` stored
    /// before this field existed still parses.
    #[serde(default)]
    pub identity: Option<String>,
}

impl TargetRef {
    /// Which machine this is. Host, user and port — never the key.
    ///
    /// **Also where a chosen key is registered**, because every command that
    /// reaches a host goes through here first: doing it at each of the dozen
    /// resolve sites would leave one that forgot, and that one would be the
    /// command that asked for a password. Only a `Some` registers; clearing is
    /// the server-list sync's job (`servers_sync`), which replaces the whole
    /// table, because a `TargetRef` built somewhere without the field must not
    /// be able to un-choose a key.
    pub fn id(&self) -> TargetId {
        match &self.host {
            None => TargetId::Local,
            Some(host) => {
                let id = SshHostId { alias: host.clone(), user: self.user.clone(), port: self.port };
                if let Some(path) = self.identity_path() {
                    rmux_ssh::keys::choose_identity(&id.label(), &path);
                }
                TargetId::Ssh(id)
            }
        }
    }

    /// The chosen key as a path — `None` when absent, empty or not absolute.
    pub fn identity_path(&self) -> Option<std::path::PathBuf> {
        let raw = self.identity.as_deref()?.trim();
        let path = std::path::PathBuf::from(raw);
        (!raw.is_empty() && path.is_absolute()).then_some(path)
    }
}

/// A target as the UI sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub id: TargetId,
    pub label: String,
    pub platform: Option<Platform>,
}

pub fn describe(target: &dyn Target) -> TargetInfo {
    TargetInfo {
        id: target.id().clone(),
        label: target.id().label(),
        platform: target.platform(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_host_means_the_local_machine() {
        let local = TargetRef::default();
        assert_eq!(local.id(), TargetId::Local);
    }

    #[test]
    fn a_host_alias_is_carried_through_unparsed() {
        // The alias may be a ~/.ssh/config Host entry; splitting or rewriting it
        // here would break ProxyJump and friends.
        let remote =
            TargetRef { host: Some("devbox".into()), user: Some("deploy".into()), port: Some(2222), identity: None };

        match remote.id() {
            TargetId::Ssh(host) => {
                assert_eq!(host.alias, "devbox");
                assert_eq!(host.user.as_deref(), Some("deploy"));
                assert_eq!(host.port, Some(2222));
            }
            other => panic!("expected an ssh target, got {other:?}"),
        }
    }

    #[test]
    fn targets_are_cached_per_destination() {
        // Same alias with a different user is a different authenticated session
        // and must not reuse the first one's connection.
        let a = TargetRef { host: Some("devbox".into()), ..Default::default() };
        let b = TargetRef { host: Some("devbox".into()), user: Some("root".into()), ..Default::default() };
        assert_ne!(a.id(), b.id());
    }

    /// **A key is how rmux authenticates, not which machine it is.** Picking one
    /// must not re-key the server, or every project and session on it would be
    /// orphaned under a new id.
    #[test]
    fn the_identity_does_not_change_the_target_id() {
        let plain = TargetRef { host: Some("idtest-host".into()), user: Some("me".into()), port: Some(22), identity: None };
        let keyed = TargetRef { identity: Some("/Users/me/.ssh/work_ed25519".into()), ..plain.clone() };
        assert_eq!(plain.id(), keyed.id());
    }

    /// Everything stored before the field existed has no `identity` key at all.
    #[test]
    fn an_old_target_without_identity_still_parses() {
        let old: TargetRef = serde_json::from_str(r#"{"host":"devbox","user":"me","port":2222}"#).unwrap();
        assert_eq!(old.identity, None);
        assert_eq!(old.port, Some(2222));
        let new: TargetRef =
            serde_json::from_str(r#"{"host":"devbox","identity":"/Users/me/.ssh/k"}"#).unwrap();
        assert_eq!(new.identity_path().as_deref(), Some(std::path::Path::new("/Users/me/.ssh/k")));
        // Relative or blank is not a choice anyone made.
        let rel: TargetRef = serde_json::from_str(r#"{"host":"devbox","identity":"k"}"#).unwrap();
        assert_eq!(rel.identity_path(), None);
        let blank: TargetRef = serde_json::from_str(r#"{"host":"devbox","identity":"  "}"#).unwrap();
        assert_eq!(blank.identity_path(), None);
    }
}
