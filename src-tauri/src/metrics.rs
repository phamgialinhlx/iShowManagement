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

#[tauri::command]
pub async fn metrics_sample(
    store: State<'_, MetricsStore>,
    target: TargetRef,
) -> Result<Sample, String> {
    let id = store.ensure(&target).await?;
    let mut monitored = store.inner().monitored.lock().await;

    let entry = monitored.get_mut(&id).expect("just inserted");
    // Borrow both fields at once: `sample` needs &mut the collector and & the
    // target, which a single `get_mut` on a tuple would not allow cleanly.
    let Monitored { target, collector } = entry;
    collector.sample(target.as_ref()).await.map_err(|e| e.to_string())
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
) -> Result<Vec<Process>, String> {
    let id = store.ensure(&target).await?;
    let mut monitored = store.inner().monitored.lock().await;

    let entry = monitored.get_mut(&id).expect("just inserted");
    let Monitored { target, collector } = entry;
    // Five is what the donut can label without the callouts colliding.
    collector.processes(target.as_ref(), by, 5).await.map_err(|e| e.to_string())
}
