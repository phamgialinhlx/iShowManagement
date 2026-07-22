//! Background "what's running?" views over the shared connection: overview,
//! docker (list/stats/actions), listening ports, and processes — plus a
//! pid kill shared by the ports and processes views. Each handler runs a small
//! read/action command via `ssh::exec` and parses stdout to JSON. Mirrors
//! `references/tsmanager/server/managers.js` (pm2 + screen dropped per ADR 008).

use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::{AppState, LOCAL_ID};
use crate::security::safe_name;
use crate::ssh::{self, ExecOutput, Target};

/// Resolve a server id to an exec target. `__local__` runs locally; any other
/// id must be a safe alias.
fn target(id: &str) -> Option<Target<'_>> {
    if id == LOCAL_ID {
        Some(Target::Local)
    } else if safe_name(id) {
        Some(Target::Remote(id))
    } else {
        None
    }
}

fn bad_target() -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad server id" })))
}

// ------------------------------------------------------------- overview ----

#[derive(Serialize)]
pub struct Pair {
    pub total: u64,
    pub available: u64,
}

#[derive(Serialize, Default)]
pub struct Overview {
    pub host: String,
    pub os: String,
    pub uptime: String,
    pub load: String,
    pub mem: Option<Pair>,
    pub disk: Option<Pair>,
}

const OVERVIEW_CMD: &str = concat!(
    r#"echo "@host $(hostname)"; "#,
    r#"echo "@os $(. /etc/os-release 2>/dev/null; echo $PRETTY_NAME)"; "#,
    r#"echo "@up $(uptime -p 2>/dev/null)"; "#,
    r#"echo "@load $(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null)"; "#,
    r#"free -b 2>/dev/null | awk 'NR==2{print "@mem",$2,$7}'; "#,
    r#"df -B1 / 2>/dev/null | awk 'NR==2{print "@disk",$2,$4}'"#,
);

fn parse_pair(s: &str) -> Option<Pair> {
    let nums: Vec<u64> = s.split_whitespace().filter_map(|n| n.parse().ok()).collect();
    match nums.as_slice() {
        [total, available] => Some(Pair {
            total: *total,
            available: *available,
        }),
        _ => None,
    }
}

fn parse_overview(stdout: &str) -> Overview {
    let mut ov = Overview::default();
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix('@') else {
            continue;
        };
        let (tag, val) = rest.split_once(' ').unwrap_or((rest, ""));
        let val = val.trim();
        match tag {
            "host" => ov.host = val.to_string(),
            "os" => ov.os = val.to_string(),
            "up" => ov.uptime = val.to_string(),
            "load" => ov.load = val.to_string(),
            "mem" => ov.mem = parse_pair(val),
            "disk" => ov.disk = parse_pair(val),
            _ => {}
        }
    }
    ov
}

pub async fn overview(
    State(_): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Overview>, (StatusCode, Json<Value>)> {
    let tgt = target(&id).ok_or_else(bad_target)?;
    let r = ssh::exec(tgt, OVERVIEW_CMD, Duration::from_secs(15)).await;
    Ok(Json(parse_overview(&r.stdout)))
}

// --------------------------------------------------------------- docker ----

#[derive(Serialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: String,
}

fn json_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn parse_docker_ps(stdout: &str) -> Vec<Container> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|c| Container {
            id: json_field(&c, "ID"),
            name: json_field(&c, "Names"),
            image: json_field(&c, "Image"),
            state: json_field(&c, "State"),
            status: json_field(&c, "Status"),
            ports: json_field(&c, "Ports"),
        })
        .collect()
}

fn docker_unavailable_reason(r: &ExecOutput) -> String {
    let e = r.stderr.to_lowercase();
    if e.contains("command not found") || e.contains("not found") {
        "docker is not installed on this server".into()
    } else if e.contains("permission denied") || e.contains("dial unix") {
        "permission denied — add this user to the docker group".into()
    } else {
        let msg = if r.stderr.trim().is_empty() {
            "docker failed"
        } else {
            r.stderr.trim()
        };
        msg.chars().take(300).collect()
    }
}

pub async fn docker(State(_): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let Some(tgt) = target(&id) else {
        return Json(json!({ "available": false, "reason": "bad server id" }));
    };
    let r = ssh::exec(tgt, "docker ps -a --format '{{json .}}'", Duration::from_secs(15)).await;
    if !r.ok {
        return Json(json!({ "available": false, "reason": docker_unavailable_reason(&r) }));
    }
    Json(json!({ "available": true, "containers": parse_docker_ps(&r.stdout) }))
}

#[derive(Serialize)]
pub struct Stat {
    pub name: String,
    pub cpu: String,
    pub mem: String,
}

fn parse_docker_stats(stdout: &str) -> Vec<Stat> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|s| Stat {
            name: json_field(&s, "Name"),
            cpu: json_field(&s, "CPUPerc"),
            mem: json_field(&s, "MemUsage"),
        })
        .collect()
}

pub async fn docker_stats(State(_): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let Some(tgt) = target(&id) else {
        return Json(json!({ "stats": [] }));
    };
    let r = ssh::exec(
        tgt,
        "docker stats --no-stream --format '{{json .}}'",
        Duration::from_secs(20),
    )
    .await;
    if !r.ok {
        return Json(json!({ "stats": [] }));
    }
    Json(json!({ "stats": parse_docker_stats(&r.stdout) }))
}

pub async fn docker_action(
    State(_): State<AppState>,
    Path((id, cid, action)): Path<(String, String, String)>,
) -> (StatusCode, Json<Value>) {
    let Some(tgt) = target(&id) else {
        return bad_target();
    };
    if !safe_name(&cid) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad container id" })));
    }
    if !matches!(action.as_str(), "start" | "stop" | "restart" | "rm") {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad action" })));
    }
    // cid is SAFE_NAME (no shell metacharacters) so direct interpolation is safe.
    let r = ssh::exec(tgt, &format!("docker {action} {cid}"), Duration::from_secs(30)).await;
    if r.ok {
        (StatusCode::OK, Json(json!({ "ok": true })))
    } else {
        exec_error(&r, &format!("docker {action}"))
    }
}

// ----------------------------------------------------------------- tmux ----

#[derive(Serialize)]
pub struct TmuxSession {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    pub created: String,
}

/// One tab-separated `name\twindows\tattached\tcreated` line per session.
const TMUX_LS_CMD: &str =
    "tmux list-sessions -F '#{session_name}\t#{session_windows}\t#{session_attached}\t#{session_created}'";

fn parse_tmux(stdout: &str) -> Vec<TmuxSession> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let mut it = l.split('\t');
            let name = it.next()?.to_string();
            Some(TmuxSession {
                name,
                windows: it.next().unwrap_or("0").trim().parse().unwrap_or(0),
                attached: it.next().unwrap_or("0").trim() != "0",
                created: it.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect()
}

pub async fn tmux(State(_): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let Some(tgt) = target(&id) else {
        return Json(json!({ "available": false, "reason": "bad server id" }));
    };
    let r = ssh::exec(tgt, TMUX_LS_CMD, Duration::from_secs(15)).await;
    if r.ok {
        return Json(json!({ "available": true, "sessions": parse_tmux(&r.stdout) }));
    }
    // tmux exits non-zero (with "no server running") when there are simply no
    // sessions — that's an empty list, not an error to surface.
    let e = r.stderr.to_lowercase();
    if e.contains("no server running") || e.contains("no sessions") || e.contains("failed to connect") {
        return Json(json!({ "available": true, "sessions": [] }));
    }
    if e.contains("command not found") || e.contains("not found") {
        return Json(json!({ "available": false, "reason": "tmux is not installed on this server" }));
    }
    let reason = if r.stderr.trim().is_empty() { "tmux failed" } else { r.stderr.trim() };
    Json(json!({ "available": false, "reason": reason.chars().take(300).collect::<String>() }))
}

// ---------------------------------------------------------------- ports ----

#[derive(Serialize)]
pub struct PortRow {
    pub proto: String,
    pub addr: String,
    pub port: u16,
    pub pid: Option<u32>,
    pub process: Option<String>,
    #[serde(rename = "forwardedTo", skip_serializing_if = "Option::is_none")]
    pub forwarded_to: Option<u16>,
}

fn ss_users_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"users:\(\("([^"]+)",pid=(\d+)"#).unwrap())
}
fn netstat_proc_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s(\d+)/([^\s/]+)\s*$").unwrap())
}

/// Parse `ss -tulpn` / `netstat -tulpn` output, collapsing the ipv4/ipv6 double
/// listing of the same socket, sorted by port.
fn parse_ports(stdout: &str) -> Vec<PortRow> {
    let mut rows: Vec<PortRow> = Vec::new();
    for line in stdout.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 || !(cols[0].starts_with("tcp") || cols[0].starts_with("udp")) {
            continue;
        }
        // The local address column: has a `:port` suffix, not the users:(...) col.
        let Some(local) = cols
            .iter()
            .find(|c| !c.starts_with("users:") && c.rsplit_once(':').is_some_and(|(_, p)| p.chars().all(|d| d.is_ascii_digit()) && !p.is_empty()))
        else {
            continue;
        };
        let Some((addr, port_s)) = local.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port_s.parse::<u16>() else {
            continue;
        };

        let (mut pid, mut process) = (None, None);
        if let Some(c) = ss_users_re().captures(line) {
            process = Some(c[1].to_string());
            pid = c[2].parse().ok();
        } else if let Some(c) = netstat_proc_re().captures(line) {
            pid = c[1].parse().ok();
            process = Some(c[2].to_string());
        }
        rows.push(PortRow {
            proto: cols[0].to_string(),
            addr: addr.to_string(),
            port,
            pid,
            process,
            forwarded_to: None,
        });
    }

    // Collapse v4/v6 duplicates: key on proto (sans trailing 6), port, pid|proc.
    let mut seen = std::collections::BTreeMap::new();
    for r in rows {
        let proto_base = r.proto.trim_end_matches('6');
        let ident = r
            .pid
            .map(|p| p.to_string())
            .or_else(|| r.process.clone())
            .unwrap_or_default();
        let key = format!("{proto_base}:{}:{ident}", r.port);
        seen.entry(key).or_insert(r);
    }
    let mut list: Vec<PortRow> = seen.into_values().collect();
    list.sort_by_key(|r| r.port);
    list
}

pub async fn ports(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let Some(tgt) = target(&id) else {
        return Json(json!({ "available": false, "reason": "bad server id" }));
    };
    let r = ssh::exec(
        tgt,
        "ss -tulpn 2>/dev/null || netstat -tulpn 2>/dev/null",
        Duration::from_secs(15),
    )
    .await;
    if r.stdout.trim().is_empty() {
        let reason = if r.stderr.trim().is_empty() {
            "ss/netstat unavailable"
        } else {
            r.stderr.trim()
        };
        return Json(json!({ "available": false, "reason": reason.chars().take(300).collect::<String>() }));
    }
    let mut list = parse_ports(&r.stdout);
    // Annotate any port we're currently forwarding locally.
    let forwards = state.forwards.lock().unwrap();
    for row in &mut list {
        if let Some(f) = forwards.get(&format!("{id}:{}", row.port)) {
            if !f.proc.exited() {
                row.forwarded_to = Some(f.local_port);
            }
        }
    }
    Json(json!({ "available": true, "ports": list }))
}

// ------------------------------------------------------------ processes ----

#[derive(Serialize)]
pub struct Proc {
    pub pid: u32,
    pub user: String,
    pub cpu: f64,
    pub mem: f64,
    pub time: String,
    pub cmd: String,
}

fn ps_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d+)\s+(\S+)\s+([\d.]+)\s+([\d.]+)\s+(\S+)\s+(.*)$").unwrap())
}

fn parse_processes(stdout: &str) -> Vec<Proc> {
    stdout
        .lines()
        .filter_map(|l| ps_line_re().captures(l.trim()))
        .filter_map(|c| {
            Some(Proc {
                pid: c[1].parse().ok()?,
                user: c[2].to_string(),
                cpu: c[3].parse().unwrap_or(0.0),
                mem: c[4].parse().unwrap_or(0.0),
                time: c[5].to_string(),
                cmd: c[6].chars().take(200).collect(),
            })
        })
        .collect()
}

pub async fn processes(State(_): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let Some(tgt) = target(&id) else {
        return Json(json!({ "processes": [] }));
    };
    let r = ssh::exec(
        tgt,
        "ps -eo pid,user:12,%cpu,%mem,etime,args --sort=-%cpu --no-headers | head -40",
        Duration::from_secs(15),
    )
    .await;
    Json(json!({ "processes": parse_processes(&r.stdout) }))
}

#[derive(Deserialize)]
pub struct KillReq {
    pub pid: i64,
    #[serde(default)]
    pub force: bool,
}

/// Kill a process by pid (used by both the ports and processes views).
pub async fn kill(
    State(_): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<KillReq>,
) -> (StatusCode, Json<Value>) {
    let Some(tgt) = target(&id) else {
        return bad_target();
    };
    if req.pid <= 1 {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad pid" })));
    }
    let signal = if req.force { "-9" } else { "-15" };
    let r = ssh::exec(tgt, &format!("kill {signal} {}", req.pid), Duration::from_secs(10)).await;
    if r.ok {
        (StatusCode::OK, Json(json!({ "ok": true })))
    } else {
        exec_error(&r, "kill")
    }
}

/// Turn a failed exec into a 500 with a trimmed message.
fn exec_error(r: &ExecOutput, what: &str) -> (StatusCode, Json<Value>) {
    let raw = if !r.stderr.trim().is_empty() {
        r.stderr.trim()
    } else if !r.stdout.trim().is_empty() {
        r.stdout.trim()
    } else {
        what
    };
    let msg: String = raw.chars().take(300).collect();
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": msg })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_tags_and_pairs() {
        let out = "@host web-1\n@os Ubuntu 22.04.3 LTS\n@up up 3 days\n@load 0.10 0.20 0.30\n@mem 8000 3000\n@disk 100000 40000\n";
        let ov = parse_overview(out);
        assert_eq!(ov.host, "web-1");
        assert_eq!(ov.os, "Ubuntu 22.04.3 LTS");
        assert_eq!(ov.uptime, "up 3 days");
        assert_eq!(ov.load, "0.10 0.20 0.30");
        assert_eq!(ov.mem.as_ref().unwrap().total, 8000);
        assert_eq!(ov.mem.as_ref().unwrap().available, 3000);
        assert_eq!(ov.disk.as_ref().unwrap().available, 40000);
        // Missing mem line → None.
        assert!(parse_overview("@host x\n").mem.is_none());
    }

    #[test]
    fn docker_ps_json_lines() {
        let out = "{\"ID\":\"abc123\",\"Names\":\"web\",\"Image\":\"nginx\",\"State\":\"running\",\"Status\":\"Up 2h\",\"Ports\":\"80/tcp\"}\n\
                   garbage-not-json\n\
                   {\"ID\":\"def456\",\"Names\":\"db\",\"Image\":\"postgres\",\"State\":\"exited\",\"Status\":\"Exited (0)\"}\n";
        let cs = parse_docker_ps(out);
        assert_eq!(cs.len(), 2, "bad json line dropped");
        assert_eq!(cs[0].name, "web");
        assert_eq!(cs[0].ports, "80/tcp");
        assert_eq!(cs[1].state, "exited");
        assert_eq!(cs[1].ports, ""); // missing field → empty
    }

    #[test]
    fn docker_stats_json_lines() {
        let out = "{\"Name\":\"web\",\"CPUPerc\":\"1.5%\",\"MemUsage\":\"20MiB / 2GiB\"}\n";
        let s = parse_docker_stats(out);
        assert_eq!(s[0].name, "web");
        assert_eq!(s[0].cpu, "1.5%");
    }

    #[test]
    fn tmux_sessions_parse() {
        let out = "work\t3\t1\t1700000000\nscratch\t1\t0\t1700000500\n\n";
        let s = parse_tmux(out);
        assert_eq!(s.len(), 2, "blank line dropped");
        assert_eq!(s[0].name, "work");
        assert_eq!(s[0].windows, 3);
        assert!(s[0].attached);
        assert_eq!(s[0].created, "1700000000");
        assert_eq!(s[1].name, "scratch");
        assert!(!s[1].attached);
    }

    #[test]
    fn ss_ports_parse_and_collapse_v4_v6() {
        // Real-ish `ss -tulpn` output: sshd listed on v4 and v6 (same socket).
        let out = "Netid State  Recv-Q Send-Q Local Address:Port Peer Address:Port Process\n\
tcp   LISTEN 0      128    0.0.0.0:22        0.0.0.0:*         users:((\"sshd\",pid=812,fd=3))\n\
tcp6  LISTEN 0      128    [::]:22           [::]:*           users:((\"sshd\",pid=812,fd=4))\n\
tcp   LISTEN 0      511    127.0.0.1:6379    0.0.0.0:*        users:((\"redis-server\",pid=901,fd=6))\n";
        let ports = parse_ports(out);
        // sshd v4/v6 collapse to one; redis stays → 2 rows, sorted by port.
        assert_eq!(ports.len(), 2, "v4/v6 dup collapsed");
        assert_eq!(ports[0].port, 22);
        assert_eq!(ports[0].pid, Some(812));
        assert_eq!(ports[0].process.as_deref(), Some("sshd"));
        assert_eq!(ports[1].port, 6379);
        assert_eq!(ports[1].process.as_deref(), Some("redis-server"));
    }

    #[test]
    fn netstat_ports_parse() {
        let out = "Proto Recv-Q Send-Q Local Address Foreign Address State PID/Program name\n\
tcp        0      0 0.0.0.0:8080     0.0.0.0:*      LISTEN      1234/node\n";
        let ports = parse_ports(out);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 8080);
        assert_eq!(ports[0].pid, Some(1234));
        assert_eq!(ports[0].process.as_deref(), Some("node"));
    }

    #[test]
    fn processes_parse_sorted_columns() {
        let out = "  1234 root         12.5  3.2 01:23:45 /usr/bin/python3 app.py\n\
   987 www-data      0.0  1.1 10-00:00:00 nginx: worker process\n";
        let ps = parse_processes(out);
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].pid, 1234);
        assert_eq!(ps[0].user, "root");
        assert_eq!(ps[0].cpu, 12.5);
        assert_eq!(ps[0].cmd, "/usr/bin/python3 app.py");
        assert_eq!(ps[1].user, "www-data");
    }
}
