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
    agent: State<'_, AgentStore>,
    target: TargetRef,
) -> Result<(), String> {
    let id = target.id();
    // Forget the provisioning cache so a later use re-probes rather than
    // trusting a stale install, then drop the SSH targets from both stores.
    agent.forget(&id);
    terminal.evict_target(&id);
    claude.evict_target(&id);
    Ok(())
}

/// A session the agent is currently running on a host.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningSession {
    pub name: String,
    pub pid: Option<u32>,
    pub age_seconds: u64,
    pub attached: bool,
    pub command: Option<String>,
}

/// Parse `agent list` output — one **tab-separated** line per session
/// (`name\tpid\tage\tattached|detached\tcommand`), as printed by
/// `crates/rmux-agent/src/main.rs:102-122`. Session names are ours and cannot
/// contain tabs or newlines, so splitting on `\t` is safe.
pub fn parse_list(out: &str) -> Vec<RunningSession> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let name = parts.next()?;
            if name.trim().is_empty() {
                return None;
            }
            let pid = parts
                .next()
                .and_then(|p| if p == "-" { None } else { p.parse().ok() });
            let age = parts.next().and_then(|a| a.parse().ok()).unwrap_or(0);
            let attached = parts.next().map(|a| a == "attached").unwrap_or(false);
            let command = parts.next().map(|c| c.to_string()).filter(|c| !c.is_empty());
            Some(RunningSession {
                name: name.to_string(),
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
