//! Server-level IPC: disconnecting a server (drop the SSH connection) and
//! listing sessions already running on it.

use tauri::State;

use crate::agent::{ensure_agent, AgentStore};
use crate::claude::ClaudeStore;
use crate::terminal::{TargetRef, TerminalStore};

/// Disconnect a server: drop the cached SSH target(s) so the ControlMaster
/// closes. Sessions keep running on the host. A `LocalTarget` is a no-op.
#[tauri::command]
pub async fn server_disconnect(
    terminal: State<'_, TerminalStore>,
    claude: State<'_, ClaudeStore>,
    files: State<'_, crate::files::FsStore>,
    metrics: State<'_, crate::metrics::MetricsStore>,
    agent: State<'_, AgentStore>,
    target: TargetRef,
) -> Result<(), String> {
    let id = target.id();
    // Forget the provisioning cache so a later use re-probes rather than
    // trusting a stale install, then drop the SSH targets from all stores.
    agent.forget(&id);
    terminal.evict_target(&id);
    claude.evict_target(&id);
    files.evict_target(&id);
    metrics.evict_target(&id).await;
    Ok(())
}

/// A session the agent is currently running on a host.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningSession {
    pub name: String,
    /// A display alias mapped to this session by a rename, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub pid: Option<u32>,
    pub age_seconds: u64,
    pub attached: bool,
    pub command: Option<String>,
}

/// Parse `agent list` output — one **tab-separated** line per session
/// (`name\talias\tpid\tage\tattached|detached\tcommand`), as printed by
/// `crates/rmux-agent/src/main.rs`. The alias column is a dash when unset.
/// Session names are ours and cannot contain tabs or newlines, so splitting on
/// `\t` is safe.
pub fn parse_list(out: &str) -> Vec<RunningSession> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let name = parts.next()?;
            if name.trim().is_empty() {
                return None;
            }
            let alias = parts
                .next()
                .filter(|a| *a != "-" && !a.is_empty())
                .map(|a| a.to_string());
            let pid = parts
                .next()
                .and_then(|p| if p == "-" { None } else { p.parse().ok() });
            let age = parts.next().and_then(|a| a.parse().ok()).unwrap_or(0);
            let attached = parts.next().map(|a| a == "attached").unwrap_or(false);
            let command = parts.next().map(|c| c.to_string()).filter(|c| !c.is_empty());
            Some(RunningSession {
                name: name.to_string(),
                alias,
                pid,
                age_seconds: age,
                attached,
                command,
            })
        })
        .collect()
}

/// List sessions the agent is running on `target` — including ones another PC
/// started. Mirrors `claude_end_session`: resolve the target, ensure the agent,
/// run `agent list` over the SSH connection, parse the rows.
#[tauri::command]
pub async fn server_sessions<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, ClaudeStore>,
    target: TargetRef,
) -> Result<Vec<RunningSession>, String> {
    let resolved = crate::claude::resolve(store.inner(), &target).await?;
    let installed = ensure_agent(&app, resolved.as_ref()).await?;
    let spec = rmux_transport::CommandSpec::new(&installed.program)
        .arg("list")
        .tty(rmux_transport::Tty::None);
    let out = resolved.exec(&spec).await.map_err(|e| e.to_string())?;
    Ok(parse_list(&out.stdout))
}

/// Map a display alias to a running terminal session on `target`, so
/// `agent list` shows the friendly name and reattach-by-alias works.
/// Terminals only — Claude resume identity is load-bearing and never aliased.
#[tauri::command]
pub async fn server_alias<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, ClaudeStore>,
    target: TargetRef,
    key: String,
    alias: String,
) -> Result<(), String> {
    let resolved = crate::claude::resolve(store.inner(), &target).await?;
    let installed = ensure_agent(&app, resolved.as_ref()).await?;
    let spec = rmux_transport::CommandSpec::new(&installed.program)
        .arg("alias")
        .arg(&key)
        .arg(&alias)
        .tty(rmux_transport::Tty::None);
    let out = resolved.exec(&spec).await.map_err(|e| e.to_string())?;
    if !out.ok() {
        let msg = out.stderr.trim();
        return Err(if msg.is_empty() {
            format!("alias failed (status {})", out.status)
        } else {
            msg.to_owned()
        });
    }
    Ok(())
}
