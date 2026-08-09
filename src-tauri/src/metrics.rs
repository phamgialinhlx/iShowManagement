//! Metrics IPC.
//!
//! One collector per target, kept across calls because CPU is a difference
//! between consecutive samples — a fresh collector each time could never report
//! a figure at all.

use std::collections::HashMap;
use std::sync::Arc;

use rmux_metrics::{Collector, Process, Sample, SortBy};
use rmux_ssh::SshTarget;
use rmux_transport::{LocalTarget, Target, TargetId};
use tauri::State;
use tokio::sync::Mutex;

use crate::terminal::TargetRef;

/// A target and the CPU baseline taken from it.
struct Monitored {
    target: Arc<dyn Target>,
    collector: Collector,
}

#[derive(Default)]
pub struct MetricsStore {
    monitored: Mutex<HashMap<TargetId, Monitored>>,
}

impl MetricsStore {
    /// Resolve a target once and keep it, so the CPU baseline survives.
    async fn ensure(&self, target: &TargetRef) -> Result<TargetId, String> {
        let id = target.id();
        let mut monitored = self.monitored.lock().await;

        if !monitored.contains_key(&id) {
            let resolved: Arc<dyn Target> = match &id {
                TargetId::Local => Arc::new(LocalTarget::new()),
                TargetId::Ssh(host) => {
                    let ssh = SshTarget::new(host.clone());
                    ssh.connect().await.map_err(|e| e.to_string())?;
                    Arc::new(ssh)
                }
            };
            monitored
                .insert(id.clone(), Monitored { target: resolved, collector: Collector::new() });
        }

        Ok(id)
    }
}

/// A short host label for benchmark lines (never a connection target).
fn target_label(id: &TargetId) -> String {
    match id {
        TargetId::Local => "local".to_owned(),
        TargetId::Ssh(host) => host.alias.clone(),
    }
}

/// Keep a polled target's persistent transport warm, off the store mutex.
///
/// The collector `sample`/`processes` calls already run under `monitored`'s
/// lock, so re-asserting the master there too would serialise a rare full
/// master revive — up to the SSH connect timeout — in front of *every* other
/// host's poll. Clone the `Arc` out, release the lock, then keep it warm.
/// Best-effort: a sample still succeeds without it, just via a fresh handshake.
async fn warm_master(store: &MetricsStore, id: &TargetId) {
    let target = {
        let monitored = store.monitored.lock().await;
        monitored.get(id).map(|m| m.target.clone())
    };
    let Some(target) = target else { return };
    let t0 = std::time::Instant::now();
    let outcome = target.ensure_ready().await;
    if let Err(e) = &outcome {
        tracing::debug!(error = %e, "could not warm the metrics control master");
    }
    // A warm ride is tens of ms (a local `ssh -O check`); a revive is hundreds+
    // (a fresh handshake). The duration is the tell, so no warm/cold flag needs
    // threading through the trait.
    if crate::logs::debug_enabled() {
        crate::logs::bench(&format!(
            "warm host={} dur_ms={} ok={}",
            target_label(id),
            t0.elapsed().as_millis(),
            outcome.is_ok()
        ));
    }
}

#[tauri::command]
pub async fn metrics_sample(
    store: State<'_, MetricsStore>,
    target: TargetRef,
) -> Result<Sample, String> {
    let id = store.ensure(&target).await?;
    warm_master(store.inner(), &id).await;

    let t0 = std::time::Instant::now();
    let result = {
        let mut monitored = store.inner().monitored.lock().await;
        let entry = monitored.get_mut(&id).expect("just inserted");
        // Borrow both fields at once: `sample` needs &mut the collector and & the
        // target, which a single `get_mut` on a tuple would not allow cleanly.
        let Monitored { target, collector } = entry;
        collector.sample(target.as_ref()).await.map_err(|e| e.to_string())
    };
    if crate::logs::debug_enabled() {
        crate::logs::bench(&format!(
            "metrics kind=sample host={} dur_ms={} ok={}",
            target_label(&id),
            t0.elapsed().as_millis(),
            result.is_ok()
        ));
    }
    result
}

/// The heaviest processes on a host.
///
/// Polled only while the process widget is open — `ps` over every process is far
/// more output than the status rows need, and it runs on the operator's server.
#[tauri::command]
pub async fn metrics_processes(
    store: State<'_, MetricsStore>,
    target: TargetRef,
    by: SortBy,
    // How many to return. The rail's donut labels five without its callouts
    // colliding; the management view asks for many more.
    limit: Option<usize>,
) -> Result<Vec<Process>, String> {
    let id = store.ensure(&target).await?;
    warm_master(store.inner(), &id).await;

    let t0 = std::time::Instant::now();
    let result = {
        let mut monitored = store.inner().monitored.lock().await;
        let entry = monitored.get_mut(&id).expect("just inserted");
        let Monitored { target, collector } = entry;
        // Five is what the donut can label without the callouts colliding.
        collector.processes(target.as_ref(), by, limit.unwrap_or(5)).await.map_err(|e| e.to_string())
    };
    if crate::logs::debug_enabled() {
        crate::logs::bench(&format!(
            "metrics kind=processes host={} dur_ms={} ok={}",
            target_label(&id),
            t0.elapsed().as_millis(),
            result.is_ok()
        ));
    }
    result
}

/// Signal a process on the target.
///
/// `hard` picks `KILL` over `TERM`. Separate rather than a retry, because the
/// difference matters on a dev host: `KILL` gives the process no chance to
/// flush or clean up, which routinely means a corrupted build or a stale lock.
#[tauri::command]
pub async fn metrics_kill(
    store: State<'_, MetricsStore>,
    target: TargetRef,
    pid: u32,
    hard: bool,
) -> Result<(), String> {
    let id = store.ensure(&target).await?;
    let mut monitored = store.inner().monitored.lock().await;

    let entry = monitored.get_mut(&id).expect("just inserted");
    let Monitored { target, collector } = entry;
    collector.kill(target.as_ref(), pid, hard).await.map_err(|e| e.to_string())
}
