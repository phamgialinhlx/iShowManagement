//! Metrics IPC.
//!
//! One collector per target, kept across calls because CPU is a difference
//! between consecutive samples — a fresh collector each time could never report
//! a figure at all.

use std::collections::HashMap;
use std::sync::Arc;

use rmux_metrics::{Collector, Sample};
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

#[tauri::command]
pub async fn metrics_sample(
    store: State<'_, MetricsStore>,
    target: TargetRef,
) -> Result<Sample, String> {
    let id = target.id();
    let mut monitored = store.inner().monitored.lock().await;

    if !monitored.contains_key(&id) {
        let resolved: Arc<dyn Target> = match &id {
            TargetId::Local => Arc::new(LocalTarget::new()),
            TargetId::Ssh(host) => {
                let ssh = SshTarget::new(host.clone());
                ssh.connect().await.map_err(|e| e.to_string())?;
                Arc::new(ssh)
            }
        };
        monitored.insert(id.clone(), Monitored { target: resolved, collector: Collector::new() });
    }

    let entry = monitored.get_mut(&id).expect("just inserted");
    // Borrow both fields at once: `sample` needs &mut the collector and & the
    // target, which a single `get_mut` on a tuple would not allow cleanly.
    let Monitored { target, collector } = entry;
    collector.sample(target.as_ref()).await.map_err(|e| e.to_string())
}
