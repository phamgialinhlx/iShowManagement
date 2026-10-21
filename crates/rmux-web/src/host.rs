//! `Host` for a headless server.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmux_app::{Host, SecretStore};

use crate::conns::Conns;

/// The shell around rmux-app's commands when there is no desktop: plain
/// directories, events broadcast over every WebSocket, and **no secret store**.
///
/// `secrets()` is `None` on purpose. A headless host has no keychain, and a
/// command that needs a credential must say so (`Host::secrets` docs) rather
/// than be handed a plaintext file that looks like one.
pub struct WebHost {
    resource_dir: Option<PathBuf>,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    conns: Arc<Conns>,
}

impl WebHost {
    pub fn new(resource_dir: Option<PathBuf>, data_dir: PathBuf, cache_dir: PathBuf, conns: Arc<Conns>) -> Self {
        Self { resource_dir, data_dir, cache_dir, conns }
    }
}

impl Host for WebHost {
    fn resource_dir(&self) -> Option<PathBuf> {
        self.resource_dir.clone()
    }

    fn data_dir(&self) -> anyhow::Result<PathBuf> {
        Ok(self.data_dir.clone())
    }

    fn cache_dir(&self) -> anyhow::Result<PathBuf> {
        Ok(self.cache_dir.clone())
    }

    /// Text frame `{"event": name, "payload": value}` to every page.
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let text = serde_json::json!({ "event": event, "payload": payload }).to_string();
        self.conns.broadcast(&text);
    }

    fn secrets(&self) -> Option<&dyn SecretStore> {
        None
    }
}

/// `$XDG_DATA_HOME/rmux-web`, else `~/.local/share/rmux-web`.
pub fn default_data_dir() -> anyhow::Result<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_CACHE_HOME/rmux-web`, else `~/.cache/rmux-web`.
pub fn default_cache_dir() -> anyhow::Result<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

fn xdg_dir(var: &str, fallback: &str) -> anyhow::Result<PathBuf> {
    // The XDG spec says a relative value is invalid and must be ignored.
    let base = match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|h| h.is_absolute())
                .ok_or_else(|| anyhow::anyhow!("neither ${var} nor $HOME is set"))?;
            home.join(fallback)
        }
    };
    Ok(base.join("rmux-web"))
}

/// Create `dir` (and parents) and make the leaf `0700`. It holds what commands
/// write about the operator's machines; nobody else on the host needs to read it.
pub fn ensure_private_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("creating {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| anyhow::anyhow!("securing {}: {e}", dir.display()))?;
    }
    Ok(())
}
