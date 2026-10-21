//! Naming a machine across the IPC boundary.
//!
//! Moved here from `src-tauri/src/terminal.rs` (and `TargetInfo` from
//! `src-tauri/src/lib.rs`) so commands in this crate can take a target without
//! depending on the desktop's terminal module. `src-tauri` re-exports both, so
//! every existing `crate::terminal::TargetRef` keeps compiling unchanged.

use rmux_transport::{Platform, SshHostId, Target, TargetId};
use serde::{Deserialize, Serialize};

/// Which machine to open a terminal on.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetRef {
    /// `None` opens on the local machine — rmux as an ordinary IDE.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

impl TargetRef {
    pub fn id(&self) -> TargetId {
        match &self.host {
            None => TargetId::Local,
            Some(host) => TargetId::Ssh(SshHostId {
                alias: host.clone(),
                user: self.user.clone(),
                port: self.port,
            }),
        }
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
        let local = TargetRef { host: None, user: None, port: None };
        assert_eq!(local.id(), TargetId::Local);
    }

    #[test]
    fn a_host_alias_is_carried_through_unparsed() {
        // The alias may be a ~/.ssh/config Host entry; splitting or rewriting it
        // here would break ProxyJump and friends.
        let remote =
            TargetRef { host: Some("devbox".into()), user: Some("deploy".into()), port: Some(2222) };

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
        let a = TargetRef { host: Some("devbox".into()), user: None, port: None };
        let b = TargetRef { host: Some("devbox".into()), user: Some("root".into()), port: None };
        assert_ne!(a.id(), b.id());
    }
}
