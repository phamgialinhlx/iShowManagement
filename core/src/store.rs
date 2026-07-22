//! Persisted per-alias app state — the bits `~/.ssh/config` can't hold: a
//! `hidden` flag (to keep git/jump hosts out of the sidebar) and a stable SOCKS
//! index (assigned once, reused for the Phase 6 browser proxy). Passwords live
//! in the SecretStore, never here. JSON at `<data_dir>/state.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct ServerState {
    #[serde(default)]
    pub hidden: bool,
    #[serde(rename = "socksIndex")]
    pub socks_index: u32,
    /// Unix seconds of the last time this host was opened — drives the sidebar's
    /// recency ordering, persisted so the order survives a restart/reinstall.
    #[serde(rename = "lastAccessed", default, skip_serializing_if = "Option::is_none")]
    pub last_accessed: Option<u64>,
}

#[derive(Serialize, Deserialize, Default)]
struct StoreData {
    servers: BTreeMap<String, ServerState>,
}

pub struct Store {
    path: PathBuf,
    data: StoreData,
}

impl Store {
    pub fn load(data_dir: &std::path::Path) -> Self {
        let _ = fs::create_dir_all(data_dir);
        let path = data_dir.join("state.json");
        let data = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, data }
    }

    fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.data) {
            let tmp = self.path.with_extension("tmp");
            if fs::write(&tmp, bytes).is_ok() {
                let _ = fs::rename(&tmp, &self.path);
            }
        }
    }

    /// Ensure an entry exists for `alias`, assigning the next free SOCKS index
    /// the first time we see it. Returns the (now-guaranteed) state.
    pub fn ensure(&mut self, alias: &str) -> ServerState {
        if !self.data.servers.contains_key(alias) {
            let idx = self.next_socks_index();
            self.data.servers.insert(
                alias.to_string(),
                ServerState {
                    hidden: false,
                    socks_index: idx,
                    last_accessed: None,
                },
            );
            self.save();
        }
        self.data.servers.get(alias).cloned().unwrap_or_default()
    }

    #[allow(dead_code)] // accessor used in tests; handlers read `hidden` directly
    pub fn is_hidden(&self, alias: &str) -> bool {
        self.data.servers.get(alias).map(|s| s.hidden).unwrap_or(false)
    }

    pub fn set_hidden(&mut self, alias: &str, hidden: bool) {
        self.ensure(alias);
        if let Some(s) = self.data.servers.get_mut(alias) {
            s.hidden = hidden;
            self.save();
        }
    }

    pub fn last_accessed(&self, alias: &str) -> Option<u64> {
        self.data.servers.get(alias).and_then(|s| s.last_accessed)
    }

    /// Stamp `alias` as accessed at `now` (unix seconds) and persist.
    pub fn touch(&mut self, alias: &str, now: u64) {
        self.ensure(alias);
        if let Some(s) = self.data.servers.get_mut(alias) {
            s.last_accessed = Some(now);
            self.save();
        }
    }

    fn next_socks_index(&self) -> u32 {
        self.data
            .servers
            .values()
            .map(|s| s.socks_index)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_stable_unique_socks_indices_and_persists() {
        let dir = std::env::temp_dir().join(format!("ism-store-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let a_idx;
        let b_idx;
        {
            let mut s = Store::load(&dir);
            a_idx = s.ensure("alpha").socks_index;
            b_idx = s.ensure("beta").socks_index;
            assert_ne!(a_idx, b_idx, "indices must be unique");
            // Re-ensuring returns the same index.
            assert_eq!(s.ensure("alpha").socks_index, a_idx);
            s.set_hidden("beta", true);
        }
        // Reload from disk: index + hidden flag persisted.
        {
            let mut s = Store::load(&dir);
            assert_eq!(s.ensure("alpha").socks_index, a_idx);
            assert_eq!(s.ensure("beta").socks_index, b_idx);
            assert!(s.is_hidden("beta"));
            assert!(!s.is_hidden("alpha"));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn last_accessed_persists_across_reload() {
        let dir = std::env::temp_dir().join(format!("ism-touch-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        {
            let mut s = Store::load(&dir);
            assert_eq!(s.last_accessed("alpha"), None);
            s.touch("alpha", 1_700_000_000);
        }
        {
            let s = Store::load(&dir);
            assert_eq!(s.last_accessed("alpha"), Some(1_700_000_000));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
