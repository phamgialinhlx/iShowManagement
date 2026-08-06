//! Making sure the persistent-session agent is available on a target.
//!
//! Provisioning is memoised per target: installing is a probe plus, on the first
//! run, a ~1MB upload, and doing that for every terminal tab would add a
//! round trip to something that should feel instant. The result is cached for the
//! life of the app, so only the first terminal on a host pays anything at all.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_agent::provision::{self, Installed};
use rmux_transport::{Target, TargetId};
use tauri::Manager;
use tokio::sync::OnceCell;

impl AgentStore {
    /// Forget a host's provisioning result so the next use re-probes the remote
    /// rather than trusting a stale install.
    pub fn forget(&self, id: &TargetId) -> bool {
        self.by_target.lock().remove(id).is_some()
    }
}

/// Provisioning results, one per target.
#[derive(Default)]
pub struct AgentStore {
    /// `OnceCell` rather than a plain map: two terminals opened at once on a cold
    /// host would otherwise both start an upload, and the second would overwrite
    /// the first's temporary file mid-write.
    by_target: Mutex<HashMap<TargetId, Arc<OnceCell<Installed>>>>,
}

/// Ensure the agent is installed on `target`, uploading it if this is the first
/// time rmux has seen this host.
pub async fn ensure_agent<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    target: &dyn Target,
) -> Result<Installed, String> {
    // **Persistence on Windows is gated off, deliberately and provisionally.**
    // The agent itself is built, ships, installs and runs there — verified on a
    // real host, down to the daemon spawning a genuine `bash.exe`. What is not
    // yet working is the last link: input written into a session does not come
    // back out of the pty (see `tests/live_windows_agent.rs`, which is committed
    // red at exactly that assertion).
    //
    // Letting callers through would take the agent path and hand the operator a
    // terminal that accepts keystrokes and shows nothing — strictly worse than
    // the non-persistent shell they get by falling back, on a platform where
    // files, search, metrics and Claude all work. Remove this the moment that
    // test goes green; nothing else has to change.
    if target.platform() == Some(rmux_transport::Platform::Windows) {
        return Err(
            "session persistence is not enabled on Windows yet — this shell will end when \
             the connection does"
                .to_owned(),
        );
    }

    let store = app.state::<AgentStore>();
    let cell = {
        let mut guard = store.by_target.lock();
        Arc::clone(guard.entry(target.id().clone()).or_default())
    };

    let source = source_for(app);
    cell.get_or_try_init(|| async { provision::ensure(target, &source).await })
        .await
        .cloned()
        .map_err(|e| e.to_string())
}

/// Where the prebuilt agents live for this build.
///
/// A bundled app carries them as resources; `cargo run` finds them next to the
/// executable in `target/debug`. Both are checked, newest-build-wins, so a
/// developer never debugs against a stale bundled copy.
fn source_for<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> provision::DirectorySource {
    let resource_dir = app.path().resource_dir().ok();
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));

    provision::default_source(resource_dir.as_deref(), exe_dir.as_deref())
}
