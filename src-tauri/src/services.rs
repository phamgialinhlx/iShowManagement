//! What is *running* on the host, beyond bare processes.
//!
//! The process list answers "what is using the CPU". It does not answer the two
//! questions an operator actually arrives with — **which containers are up** and
//! **which Claude sessions are still going** — because both are collections of
//! processes whose names say nothing useful (`node`, `python3`, and a login
//! shell) and whose identity lives in a daemon rather than in `/proc`.
//!
//! ## Everything here is read with one command per refresh
//!
//! Each of these is an SSH round trip. Listing containers and then asking for
//! each one's stats separately would be N+1 trips on a timer, which on a host
//! with twenty containers is unusable. `docker stats --no-stream` reports all of
//! them at once and is joined here.
//!
//! ## Absence is reported, never faked
//!
//! A host without Docker is the ordinary case, not an error: the tab says so
//! rather than showing an empty list, which would read as "no containers
//! running" — a different and wrong claim.

use rmux_transport::{shell_quote, CommandSpec, Target};
use serde::Serialize;

/// A container, joined with its live resource use.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// Docker's own words (`Up 3 hours`, `Exited (0) 2 days ago`) — not parsed
    /// into a boolean, because "restarting" and "paused" are real states that a
    /// running/stopped flag would have to lie about.
    pub status: String,
    pub running: bool,
    /// Percent of one host CPU, as Docker reports it. `None` when stopped.
    pub cpu: Option<f32>,
    /// Bytes in use. Reported in bytes so the UI decides the unit.
    pub memory: Option<u64>,
    pub memory_limit: Option<u64>,
    pub ports: String,
}

/// A Claude or shell session held by `rmux-agent` on the host.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    pub name: String,
    /// A display name set on the host, if one was. Shown instead of `name` —
    /// the person who renamed it may have been at a different computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub pid: Option<u32>,
    pub age_seconds: u64,
    /// Whether a client is currently attached — the difference between a
    /// session someone is using and one left behind.
    pub attached: bool,
    pub command: String,
    /// Resident memory of the session's process tree, in bytes.
    pub memory: Option<u64>,
    pub cpu: Option<f32>,
}

/// `docker ps` joined with `docker stats`, or a reason there is nothing to show.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DockerReport {
    /// Docker is not installed, or this account cannot talk to the daemon.
    Unavailable { reason: String },
    Containers(Vec<Container>),
}

/// Fields are tab-separated and rows newline-separated.
///
/// A container *name* cannot contain either, and neither can an id or an image
/// reference — which is what makes this parseable at all. The same reasoning as
/// `rmux-agent list`; unlike a filename, these identifiers are constrained.
const PS_FORMAT: &str = "{{.ID}}\\t{{.Names}}\\t{{.Image}}\\t{{.Status}}\\t{{.Ports}}";
const STATS_FORMAT: &str = "{{.ID}}\\t{{.CPUPerc}}\\t{{.MemUsage}}";

pub async fn docker(target: &dyn Target) -> anyhow::Result<DockerReport> {
    // `-a` so stopped containers are listed too: the operator wants to *start*
    // one at least as often as stop it, and a container you cannot see is one
    // you cannot start.
    let script = format!(
        "command -v docker >/dev/null 2>&1 || {{ echo __NO_DOCKER__; exit 0; }}; \
         docker ps -a --format '{PS_FORMAT}' 2>&1 || echo __DOCKER_ERR__; \
         echo __STATS__; \
         docker stats --no-stream --format '{STATS_FORMAT}' 2>/dev/null || true"
    );
    let out = target.exec(&CommandSpec::login_shell().arg("-c").arg(script)).await?;
    // Logged because the shape of this output is the thing that has been wrong
    // before, and it cannot be read off the screen: a row that fails to parse
    // and a container with genuinely no stats look identical in the UI.
    let report = parse_docker(&out.stdout);
    if let DockerReport::Containers(list) = &report {
        tracing::debug!(
            bytes = out.stdout.len(),
            containers = list.len(),
            with_stats = list.iter().filter(|c| c.cpu.is_some()).count(),
            "read docker containers"
        );
    }
    Ok(report)
}

/// The parsing, separated from the round trip so it can be tested against real
/// captured output rather than against what I assumed Docker prints.
pub fn parse_docker(text: &str) -> DockerReport {
    if text.contains("__NO_DOCKER__") {
        return DockerReport::Unavailable { reason: "docker is not installed on this host".into() };
    }

    let (ps, stats) = text.split_once("__STATS__").unwrap_or((text, ""));

    // A daemon that is not running, or an account not in the `docker` group,
    // both surface here — and both are worth saying out loud, because the fix
    // is different and neither is "no containers".
    if ps.contains("__DOCKER_ERR__") || ps.contains("permission denied") || ps.contains("Cannot connect") {
        let reason = ps
            .lines()
            .find(|l| l.contains("permission denied") || l.contains("Cannot connect"))
            .unwrap_or("docker is installed but did not answer")
            .trim()
            .to_string();
        return DockerReport::Unavailable { reason };
    }

    let mut containers = Vec::new();
    for line in ps.lines().filter(|l| !l.trim().is_empty() && !l.contains("__")) {
        let mut f = line.split('\t');
        let (Some(id), Some(name), Some(image), Some(status)) = (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        containers.push(Container {
            id: id.trim().to_string(),
            name: name.trim().to_string(),
            image: image.trim().to_string(),
            running: status.trim_start().starts_with("Up"),
            status: status.trim().to_string(),
            ports: f.next().unwrap_or_default().trim().to_string(),
            ..Container::default()
        });
    }

    for line in stats.lines().filter(|l| !l.trim().is_empty()) {
        let mut f = line.split('\t');
        let (Some(id), Some(cpu), Some(mem)) = (f.next(), f.next(), f.next()) else { continue };
        let id = id.trim();
        // `docker stats` prints short ids; `docker ps` may print either, so match
        // on prefix in whichever direction is longer.
        if let Some(c) = containers
            .iter_mut()
            .find(|c| c.id.starts_with(id) || id.starts_with(c.id.as_str()))
        {
            c.cpu = cpu.trim().trim_end_matches('%').parse().ok();
            let (used, limit) = mem.split_once('/').unwrap_or((mem, ""));
            c.memory = parse_size(used.trim());
            c.memory_limit = parse_size(limit.trim());
        }
    }

    DockerReport::Containers(containers)
}

/// `1.5GiB`, `934MiB`, `12.3kB` → bytes.
///
/// Docker mixes SI and binary units in the same field depending on version and
/// platform, so both are handled. Returning `None` on anything unrecognised is
/// deliberate: a memory figure that is silently wrong by 1024× is worse than a
/// missing one, because it is believable.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let cut = s.find(|c: char| c.is_alphabetic())?;
    let value: f64 = s[..cut].trim().parse().ok()?;
    let unit = s[cut..].trim().to_ascii_lowercase();
    let scale: f64 = match unit.as_str() {
        "b" => 1.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        "kib" => 1_024.0,
        "mib" => 1_024.0 * 1_024.0,
        "gib" => 1_024.0 * 1_024.0 * 1_024.0,
        "tib" => 1_024.0 * 1_024.0 * 1_024.0 * 1_024.0,
        _ => return None,
    };
    Some((value * scale) as u64)
}

/// What a container action may be.
///
/// An enum rather than a string, and it is the security property: the value
/// crossing the IPC bridge cannot become an arbitrary `docker` subcommand, let
/// alone a shell fragment. The id is still quoted — belt and braces, because it
/// comes from the far side.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
}

impl ContainerAction {
    fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

pub async fn container_action(
    target: &dyn Target,
    id: &str,
    action: ContainerAction,
) -> anyhow::Result<String> {
    anyhow::ensure!(!id.trim().is_empty(), "no container given");
    let line = format!("docker {} {} 2>&1", action.verb(), shell_quote(id));
    let out = target.exec(&CommandSpec::login_shell().arg("-c").arg(line)).await?;
    // Docker explains its refusals on stdout and exits non-zero. Reading only
    // the text would report success for every failure — the same mistake the
    // process kill made, and it is pinned there for the same reason.
    anyhow::ensure!(out.ok(), "{}", out.stdout.trim().lines().next().unwrap_or("docker refused"));
    Ok(out.stdout.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_sizes_in_both_unit_systems() {
        assert_eq!(parse_size("1.5GiB"), Some(1_610_612_736));
        assert_eq!(parse_size("934MiB"), Some(979_369_984));
        assert_eq!(parse_size("12.3kB"), Some(12_300));
        assert_eq!(parse_size("2GB"), Some(2_000_000_000));
    }

    #[test]
    fn an_unknown_unit_is_absent_rather_than_wrong() {
        // A figure wrong by 1024x is worse than a missing one: it is believable.
        assert_eq!(parse_size("12 parsecs"), None);
        assert_eq!(parse_size("--"), None);
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn an_action_is_a_verb_not_a_string() {
        // The point of the enum: nothing the webview sends can widen this set.
        assert_eq!(ContainerAction::Start.verb(), "start");
        assert_eq!(ContainerAction::Stop.verb(), "stop");
        assert_eq!(ContainerAction::Restart.verb(), "restart");
    }

    #[test]
    fn a_hostile_container_id_is_quoted() {
        let line = format!("docker {} {} 2>&1", ContainerAction::Stop.verb(), shell_quote("a'; rm -rf /; echo '"));
        // The id comes from the far side, so it is treated as untrusted even
        // though Docker's own ids are hex.
        assert!(line.contains("'a'\\''; rm -rf /; echo '\\'''"), "{line}");
    }
}

// ── IPC ──────────────────────────────────────────────────────────────────────

use crate::terminal::TargetRef;
use rmux_ssh::SshTarget;
use rmux_transport::{LocalTarget, TargetId};

/// Resolve a target for a one-shot read.
///
/// Not cached, unlike `MetricsStore`: nothing here is a *difference* between
/// samples, so there is no baseline to preserve and a fresh target costs only
/// the ControlMaster socket that is already open.
async fn resolved(target: &TargetRef) -> Result<Box<dyn Target>, String> {
    match target.id() {
        TargetId::Local => Ok(Box::new(LocalTarget::new())),
        TargetId::Ssh(host) => {
            let ssh = SshTarget::new(host);
            ssh.connect().await.map_err(|e| e.to_string())?;
            Ok(Box::new(ssh))
        }
    }
}

#[tauri::command]
pub async fn docker_containers(target: TargetRef) -> Result<DockerReport, String> {
    let t = resolved(&target).await?;
    docker(t.as_ref()).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn docker_action(
    target: TargetRef,
    id: String,
    action: ContainerAction,
) -> Result<String, String> {
    let t = resolved(&target).await?;
    container_action(t.as_ref(), &id, action).await.map_err(|e| e.to_string())
}

/// One row of `rmux-agent list`, tolerant of both column counts.
///
/// The agent gained an alias column, and the host may be running **either**
/// build: a rebuilt client talks to whichever daemon still owns a live session,
/// and old daemons deliberately keep serving until their sessions end. Reading
/// the new format from an old daemon would take the pid as the alias and the age
/// as the pid — every row silently wrong rather than absent, which is the worse
/// failure.
///
/// The two are told apart by whether the second field parses as a number: an
/// alias never does, because a numeric one would be refused as ambiguous with a
/// pid at exactly this point.
fn parse_agent_row(row: &str) -> Option<AgentSession> {
    let f: Vec<&str> = row.split('\t').collect();
    let name = f.first()?.trim();
    if name.is_empty() {
        return None;
    }

    // `-` is the agent's "absent" marker in every column.
    let dash = |v: &str| {
        let v = v.trim();
        (!v.is_empty() && v != "-").then(|| v.to_owned())
    };

    // Six columns: name, alias, pid, age, attached, command.
    // Five columns: name, pid, age, attached, command.
    let six = f.len() >= 6 && f.get(1).is_some_and(|v| v.trim().parse::<u32>().is_err());
    let (alias, rest) = if six { (dash(f[1]), &f[2..]) } else { (None, &f[1..]) };

    Some(AgentSession {
        name: name.to_owned(),
        alias,
        pid: rest.first().and_then(|v| v.trim().parse::<u32>().ok()),
        age_seconds: rest.get(1).and_then(|v| v.trim().parse().ok()).unwrap_or(0),
        attached: rest.get(2).is_some_and(|v| v.trim() == "attached"),
        command: rest.get(3).unwrap_or(&"").trim().to_owned(),
        memory: None,
        cpu: None,
    })
}

/// Sessions `rmux-agent` is holding on the host, with what they are consuming.
///
/// The list comes from the daemon rather than from `ps`, because that is the
/// only thing that knows a session's *name* — from the outside a Claude session
/// is a login shell, indistinguishable from any other. `ps` is then asked for
/// the resource figures in the same round trip.
///
/// Dead sessions are already excluded by `rmux-agent list`: their pid may have
/// been reused, and reporting one sends the operator to kill something else.
pub async fn agent_sessions(target: &dyn Target, program: &str) -> anyhow::Result<Vec<AgentSession>> {
    let line = format!(
        "{} list 2>/dev/null || true; echo __PS__; ps -eo pid=,rss=,pcpu= 2>/dev/null || true",
        shell_quote(program)
    );
    let out = target.exec(&CommandSpec::login_shell().arg("-c").arg(line)).await?;
    let (listing, ps) = out.stdout.split_once("__PS__").unwrap_or((out.stdout.as_str(), ""));

    // pid → (resident bytes, percent of one CPU)
    let mut usage = std::collections::HashMap::new();
    for row in ps.lines() {
        let mut f = row.split_whitespace();
        if let (Some(pid), Some(rss), Some(cpu)) = (f.next(), f.next(), f.next())
            && let (Ok(pid), Ok(rss)) = (pid.parse::<u32>(), rss.parse::<u64>())
        {
            // `ps` reports RSS in kilobytes; the UI is given bytes like every
            // other figure here, so one place decides the unit.
            usage.insert(pid, (rss * 1024, cpu.parse::<f32>().unwrap_or(0.0)));
        }
    }

    let mut sessions = Vec::new();
    for row in listing.lines().filter(|l| !l.trim().is_empty()) {
        let Some(parsed) = parse_agent_row(row) else { continue };
        let (memory, cpu) = parsed.pid.and_then(|p| usage.get(&p)).copied().unzip();
        sessions.push(AgentSession { memory, cpu, ..parsed });
    }
    Ok(sessions)
}

#[tauri::command]
pub async fn host_sessions<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    target: TargetRef,
) -> Result<Vec<AgentSession>, String> {
    let t = resolved(&target).await?;
    // The agent has to be there to be asked. On a host where it cannot run
    // (Windows), this reports the reason rather than an empty list — which
    // would read as "nothing running".
    let installed = crate::agent::ensure_agent(&app, t.as_ref()).await?;
    agent_sessions(t.as_ref(), &installed.program).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod real_output {
    use super::*;

    /// The exact shape a live host produced — tabs, trailing empty field,
    /// mixed units and all — with the names and images replaced.
    ///
    /// The structure is what the test is for; a real deployment's container
    /// inventory is not something a shared repository should carry.
    ///
    /// Written down because every earlier assumption about this format was
    /// checked against what I expected Docker to print rather than what it
    /// does. The two lines with no ports end in a trailing tab; one container is
    /// stopped; `docker stats` lists only the running ones and its memory field
    /// carries a limit after a slash.
    const CAPTURE: &str = "61a4cba3d3ca\tweb-cache\tredis:7\tUp 25 hours\t\n\
214dfb757607\tdb-tools\tpostgres:16-alpine\tExited (0) 35 hours ago\t\n\
fc4a0701b555\tweb\texample/web:1.2\tUp 35 hours\t0.0.0.0:8080->80/tcp, [::]:8080->80/tcp\n\
__STATS__\n\
61a4cba3d3ca\t0.00%\t27.98MiB / 62.79GiB\n\
fc4a0701b555\t0.01%\t204.3MiB / 62.79GiB\n";

    fn containers(report: DockerReport) -> Vec<Container> {
        match report {
            DockerReport::Containers(c) => c,
            DockerReport::Unavailable { reason } => panic!("unavailable: {reason}"),
        }
    }

    #[test]
    fn a_real_capture_yields_containers_with_their_usage() {
        let list = containers(parse_docker(CAPTURE));
        assert_eq!(list.len(), 3, "every row is a container");

        let web = list.iter().find(|c| c.name == "web").expect("web");
        assert!(web.running);
        // The figures are the point of the tab: without them the rows are just
        // names, which `docker ps` already gives in a terminal.
        assert_eq!(web.cpu, Some(0.01));
        assert_eq!(web.memory, Some(214_224_076));
        assert_eq!(web.memory_limit, Some(67_420_249_128));
        assert_eq!(web.ports, "0.0.0.0:8080->80/tcp, [::]:8080->80/tcp");
    }

    #[test]
    fn a_stopped_container_is_listed_without_usage() {
        let list = containers(parse_docker(CAPTURE));
        let stopped = list.iter().find(|c| c.name == "db-tools").expect("db-tools");
        // Listed, because starting one is half the reason for the tab — and
        // with no figures, because `docker stats` does not report the stopped.
        assert!(!stopped.running);
        assert_eq!(stopped.cpu, None);
        assert_eq!(stopped.memory, None);
    }

    #[test]
    fn a_container_with_no_ports_still_parses() {
        let list = containers(parse_docker(CAPTURE));
        let c = list.iter().find(|c| c.name == "web-cache").expect("web-cache");
        // The row ends in a trailing tab, so the last field is empty rather
        // than absent — a split that dropped it would take the *status* as the
        // ports and leave the row without a state.
        assert_eq!(c.ports, "");
        assert_eq!(c.status, "Up 25 hours");
        assert_eq!(c.cpu, Some(0.0));
    }
}

#[cfg(test)]
mod agent_list_parsing {
    use super::parse_agent_row;

    /// The new six-column form, from an agent that knows about aliases.
    #[test]
    fn a_renamed_session_reports_its_alias() {
        let row = parse_agent_row("term-1\twebapp\t4131\t900\tattached\tbash").expect("a row");
        assert_eq!(row.name, "term-1", "the key is what reattaches");
        assert_eq!(row.alias.as_deref(), Some("webapp"));
        assert_eq!(row.pid, Some(4131));
        assert_eq!(row.age_seconds, 900);
        assert!(row.attached);
        assert_eq!(row.command, "bash");
    }

    #[test]
    fn an_unnamed_session_has_no_alias() {
        let row = parse_agent_row("term-1\t-\t4131\t900\tdetached\tbash").expect("a row");
        assert_eq!(row.alias, None, "`-` is absent, not a name");
        assert!(!row.attached);
    }

    /// **The compatibility case.** An old daemon still serving its live sessions
    /// prints five columns. Read as six, its pid would become the alias and its
    /// age the pid — every row wrong in a way that still renders.
    #[test]
    fn an_older_daemons_five_columns_still_parse() {
        let row = parse_agent_row("term-1\t4131\t900\tattached\tbash").expect("a row");
        assert_eq!(row.alias, None);
        assert_eq!(row.pid, Some(4131), "the pid must not be read as an alias");
        assert_eq!(row.age_seconds, 900);
        assert_eq!(row.command, "bash");
    }

    #[test]
    fn a_dead_session_has_no_pid() {
        let row = parse_agent_row("term-1\t-\t-\t10\tdetached\t").expect("a row");
        assert_eq!(row.pid, None);
        assert_eq!(row.command, "");
    }

    #[test]
    fn a_row_with_no_name_is_skipped() {
        assert!(parse_agent_row("").is_none());
        assert!(parse_agent_row("\t-\t1\t2\tattached\tbash").is_none());
    }
}
