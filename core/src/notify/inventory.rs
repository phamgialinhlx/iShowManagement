//! The Claude/tmux inventory tree. Correlates Claude's own live session-status
//! files with the host's process table and tmux panes to produce the nested
//! sidebar tree (Claude instances grouped by tmux session). Split out of
//! `notify.rs`: it shares only the private `guard` helper (via `super::guard`)
//! and its pure core, `build_inventory`, is SSH-free — which is what the tests
//! exercise.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::AppState;
use crate::ssh::{self, Target};

use super::guard;

/// `GET /api/servers/{id}/tmux/claude` — Claude instances grouped by tmux
/// session, for the nested sidebar tree. State comes from Claude's own live
/// status file, `~/.claude/sessions/<pid>.json` (`status`): `working` (`busy` or
/// `shell` — generating or running a tool), `waiting` (a blocking HITL prompt,
/// with `waitingFor` detail), or `idle` (at the prompt — the frontend splits idle
/// into read/unread against `statusUpdatedAt`). Each file is correlated to a pane
/// via the Claude pid's tty (matching `#{pane_tty}`) or its parent chain reaching
/// a `#{pane_pid}`; a Claude in a plain shell (no tmux) has no pane and is absent.
/// The UI merges this with the full session list from `/tmux`.
pub async fn claude_inventory(
    State(_): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = guard(&id) {
        return e;
    }
    // One round trip: Claude's session-status files (state), the live process
    // table (liveness + tty for mapping), then the live pane table (location).
    let cmd = "for f in \"$HOME\"/.claude/sessions/*.json; do [ -f \"$f\" ] && cat \"$f\" && echo; done 2>/dev/null; \
        echo '===CTX==='; \
        for f in \"$HOME\"/.claude/sessions/*.json; do [ -f \"$f\" ] || continue; \
        sid=$(grep -o '\"sessionId\":\"[^\"]*\"' \"$f\" | head -1 | sed 's/.*:\"//;s/\"//'); [ -n \"$sid\" ] || continue; \
        tp=$(ls \"$HOME\"/.claude/projects/*/\"$sid\".jsonl 2>/dev/null | head -1); [ -f \"$tp\" ] || continue; \
        u=$(tail -n 400 \"$tp\" 2>/dev/null | grep '\"usage\"' | tail -1 | grep -o '\"usage\":{[^{]*'); [ -n \"$u\" ] || continue; \
        it=$(printf '%s' \"$u\" | grep -o '\"input_tokens\":[0-9]*' | head -1 | sed 's/[^0-9]//g'); \
        cr=$(printf '%s' \"$u\" | grep -o '\"cache_read_input_tokens\":[0-9]*' | head -1 | sed 's/[^0-9]//g'); \
        cc=$(printf '%s' \"$u\" | grep -o '\"cache_creation_input_tokens\":[0-9]*' | head -1 | sed 's/[^0-9]//g'); \
        echo \"$sid $(( ${it:-0} + ${cr:-0} + ${cc:-0} ))\"; done 2>/dev/null; \
        echo '===PS==='; \
        ps -eo pid,ppid,tty,comm 2>/dev/null; \
        echo '===PANES==='; \
        tmux list-panes -a -F \
        '#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_id}\t#{pane_tty}\t#{pane_pid}' \
        2>/dev/null";
    let r = ssh::exec(Target::Remote(&id), cmd, Duration::from_secs(15)).await;
    if !r.ok && r.stdout.trim().is_empty() {
        let hint = if r.stderr.trim().is_empty() {
            "can't reach host — open a Console first"
        } else {
            r.stderr.trim()
        };
        return (StatusCode::OK, Json(json!({ "available": false, "reason": hint })));
    }
    let (sessions, rest) = r.stdout.split_once("===CTX===").unwrap_or((r.stdout.as_str(), ""));
    let (ctx, rest) = rest.split_once("===PS===").unwrap_or((rest, ""));
    let (ps, panes) = rest.split_once("===PANES===").unwrap_or((rest, ""));
    (StatusCode::OK, Json(json!({ "available": true, "sessions": build_inventory(sessions, ctx, ps, panes) })))
}

/// A live pane from `tmux list-panes` (see the format in `claude_inventory`):
/// `session \t window_index \t window_name \t pane_index \t pane_id \t pane_tty
/// \t pane_pid`.
struct PaneInfo {
    session: String,
    window: u32,
    window_name: String,
    pane: u32,
}

/// A live process row from `ps -eo pid,ppid,tty,comm`.
struct ProcInfo {
    ppid: u32,
    /// Controlling tty, normalized (no `/dev/` prefix).
    tty: String,
    comm: String,
}

/// A parsed Claude session-status file, `~/.claude/sessions/<pid>.json`. Claude
/// writes and live-updates this itself — the authoritative state signal.
#[derive(Deserialize)]
struct SessionFile {
    pid: u32,
    #[serde(default)]
    status: String,
    #[serde(rename = "waitingFor", default)]
    waiting_for: Option<String>,
    #[serde(default)]
    cwd: String,
    #[serde(rename = "statusUpdatedAt", default)]
    status_updated_at: u64,
    #[serde(rename = "sessionId", default)]
    session_id: String,
}

/// Strip a leading `/dev/` so `ps` ttys (`ttys007`, `pts/3`) and tmux
/// `#{pane_tty}` (`/dev/ttys007`, `/dev/pts/3`) compare equal.
fn norm_tty(t: &str) -> String {
    t.trim().trim_start_matches("/dev/").to_string()
}

/// Executable basename of a `ps` comm, without a login shell's leading `-`.
fn comm_base(comm: &str) -> &str {
    comm.trim().rsplit('/').next().unwrap_or("").trim_start_matches('-')
}

/// Claude's `status` → our tree state. `busy`/`shell` (generating or running a
/// tool) are both `working`; `waiting` is a blocking HITL prompt; anything else
/// (`idle`, empty) is `idle` — the frontend splits idle into read/unread.
fn map_status(status: &str) -> &'static str {
    match status {
        "busy" | "shell" => "working",
        "waiting" => "waiting",
        _ => "idle",
    }
}

/// Walk `pid`'s parent chain until an ancestor is some pane's `pane_pid` — the
/// fallback mapping when a Claude process has no controlling tty to match on.
fn pane_via_ppid(
    mut pid: u32,
    procs: &HashMap<u32, ProcInfo>,
    by_pane_pid: &HashMap<u32, String>,
) -> Option<String> {
    for _ in 0..32 {
        if let Some(pane) = by_pane_pid.get(&pid) {
            return Some(pane.clone());
        }
        let pr = procs.get(&pid)?;
        if pr.ppid == 0 || pr.ppid == pid {
            return None;
        }
        pid = pr.ppid;
    }
    None
}

/// Pure core of `claude_inventory`, split out so it's testable without SSH.
/// Correlates Claude's own session-status files (state) with the live process
/// table (liveness + tty) and tmux panes (location): a session file maps to a
/// pane by matching the Claude pid's tty to `#{pane_tty}`, or by its parent chain
/// reaching a `#{pane_pid}`. A file whose pid is dead (stale) or reused by a
/// non-`claude` process is dropped; a Claude with no matching pane (a plain SSH
/// shell, not tmux) is simply absent from the tree.
fn build_inventory(sessions: &str, ctx: &str, ps: &str, panes: &str) -> Vec<Value> {
    // Context-window tokens by sessionId (`<sid> <n>` per line): input +
    // cache-read + cache-creation from the transcript's last usage block — how
    // full the window is (matches Claude Code's own context-length figure).
    let mut ctx_by_sid: HashMap<String, u64> = HashMap::new();
    for line in ctx.lines() {
        let mut it = line.split_whitespace();
        if let (Some(sid), Some(n)) = (it.next(), it.next()) {
            if let Ok(n) = n.parse::<u64>() {
                ctx_by_sid.insert(sid.to_string(), n);
            }
        }
    }

    // Live processes: pid -> {ppid, tty, comm}.
    let mut procs: HashMap<u32, ProcInfo> = HashMap::new();
    for line in ps.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(tty)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else { continue }; // skips the header row
        let comm = it.collect::<Vec<_>>().join(" ");
        procs.insert(pid, ProcInfo { ppid: ppid.parse().unwrap_or(0), tty: norm_tty(tty), comm });
    }

    // Live panes, in tmux's own order (a stable child order), plus tty/pane_pid
    // lookup tables for mapping.
    let mut order: Vec<String> = Vec::new();
    let mut by_pane: HashMap<String, PaneInfo> = HashMap::new();
    let mut by_tty: HashMap<String, String> = HashMap::new();
    let mut by_pane_pid: HashMap<u32, String> = HashMap::new();
    for l in panes.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        let pane_id = f[4].trim().to_string();
        if pane_id.is_empty() {
            continue;
        }
        order.push(pane_id.clone());
        by_tty.insert(norm_tty(f[5]), pane_id.clone());
        if let Ok(pp) = f[6].trim().parse::<u32>() {
            by_pane_pid.insert(pp, pane_id.clone());
        }
        by_pane.insert(
            pane_id,
            PaneInfo {
                session: f[0].to_string(),
                window: f[1].trim().parse().unwrap_or(0),
                window_name: f[2].to_string(),
                pane: f[3].trim().parse().unwrap_or(0),
            },
        );
    }

    // Map each live Claude session file to a pane (1:1 — one pty per pane).
    let mut sess_by_pane: HashMap<String, SessionFile> = HashMap::new();
    for line in sessions.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(sf) = serde_json::from_str::<SessionFile>(line) else { continue };
        // Liveness + recycled-pid guard: the pid must be an alive `claude`.
        let Some(pr) = procs.get(&sf.pid) else { continue };
        if comm_base(&pr.comm) != "claude" {
            continue;
        }
        let pane = by_tty
            .get(&pr.tty)
            .cloned()
            .or_else(|| pane_via_ppid(sf.pid, &procs, &by_pane_pid));
        if let Some(pane_id) = pane {
            sess_by_pane.entry(pane_id).or_insert(sf);
        }
    }

    // Emit instances in tmux pane order, grouped by session.
    let mut by_sess: HashMap<String, Vec<Value>> = HashMap::new();
    for pane_id in &order {
        let Some(sf) = sess_by_pane.get(pane_id) else { continue };
        let info = &by_pane[pane_id];
        let project = sf.cwd.rsplit('/').next().map(str::trim).filter(|s| !s.is_empty());
        let c = json!({
            "paneId": pane_id,
            "window": info.window,
            "windowName": info.window_name,
            "pane": info.pane,
            "status": map_status(&sf.status),
            "waitingFor": sf.waiting_for,
            "statusUpdatedAt": sf.status_updated_at,
            "sessionId": sf.session_id,
            "project": project,
            "contextTokens": ctx_by_sid.get(&sf.session_id).copied(),
        });
        by_sess.entry(info.session.clone()).or_default().push(c);
    }

    // Children stay in tmux pane order (insertion order above); only the sessions
    // themselves are sorted, by name, so the session list is stable too.
    let mut sessions: Vec<Value> = by_sess
        .into_iter()
        .map(|(name, v)| json!({ "name": name, "claude": v }))
        .collect();
    sessions.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real capture (2026-07-31, CLI 2.1.220): the four `status` values map to the
    // three host states, each session file correlated to its pane by tty.
    #[test]
    fn session_file_status_maps_to_states() {
        let sessions = "\
{\"pid\":101,\"cwd\":\"/home/u/alpha\",\"status\":\"busy\",\"statusUpdatedAt\":10,\"sessionId\":\"a\"}
{\"pid\":102,\"cwd\":\"/home/u/beta\",\"status\":\"shell\",\"statusUpdatedAt\":20,\"sessionId\":\"b\"}
{\"pid\":103,\"cwd\":\"/home/u/gamma\",\"status\":\"waiting\",\"waitingFor\":\"input needed\",\"statusUpdatedAt\":30,\"sessionId\":\"c\"}
{\"pid\":104,\"cwd\":\"/home/u/delta\",\"status\":\"idle\",\"statusUpdatedAt\":40,\"sessionId\":\"d\"}";
        let ps = "\
  PID  PPID TTY      COMM
  101   91 pts/1    claude
  102   92 pts/2    claude
  103   93 pts/3    claude
  104   94 pts/4    claude";
        let panes = "\
s\t0\tmain\t0\t%1\t/dev/pts/1\t91
s\t0\tmain\t1\t%2\t/dev/pts/2\t92
s\t0\tmain\t2\t%3\t/dev/pts/3\t93
s\t0\tmain\t3\t%4\t/dev/pts/4\t94";
        // Context tokens joined by sessionId; sessions without a line stay null.
        let ctx = "a 128000\nc 42000";
        let inv = build_inventory(sessions, ctx, ps, panes);
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0]["name"], "s");
        let claude = inv[0]["claude"].as_array().unwrap();
        let states: Vec<&str> = claude.iter().map(|c| c["status"].as_str().unwrap()).collect();
        assert_eq!(states, ["working", "working", "waiting", "idle"]);
        assert_eq!(claude[2]["waitingFor"], "input needed");
        assert_eq!(claude[2]["statusUpdatedAt"], 30);
        assert_eq!(claude[0]["project"], "alpha");
        assert_eq!(claude[0]["paneId"], "%1");
        assert_eq!(claude[0]["contextTokens"], 128000);
        assert_eq!(claude[2]["contextTokens"], 42000);
        assert_eq!(claude[3]["contextTokens"], Value::Null);
    }

    // A Claude in a plain SSH shell (no tmux) has a live session file and pid but
    // no pane on its tty → absent from the tmux-grouped tree. Modelled on the real
    // capture: pid 18987 (tmux, ttys007) shows; pid 54048 (this agent, ttys008, no
    // pane) does not. `pane_current_command` is the `2.1.220` title — never matched.
    #[test]
    fn non_tmux_claude_is_absent() {
        let sessions = "\
{\"pid\":18987,\"cwd\":\"/home/u/proj\",\"status\":\"waiting\",\"waitingFor\":\"input needed\",\"statusUpdatedAt\":5,\"sessionId\":\"x\"}
{\"pid\":54048,\"cwd\":\"/home/u/proj\",\"status\":\"busy\",\"statusUpdatedAt\":9,\"sessionId\":\"y\"}";
        let ps = "\
18987 18502 ttys007  claude
54048 50310 ttys008  claude
18502 18501 ttys007  -zsh";
        let panes = "hi\t0\t2.1.220\t0\t%0\t/dev/ttys007\t18502";
        let inv = build_inventory(sessions, "", ps, panes);
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0]["name"], "hi");
        let claude = inv[0]["claude"].as_array().unwrap();
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0]["paneId"], "%0");
        assert_eq!(claude[0]["status"], "waiting");
    }

    // Stale files (pid gone) and recycled pids (pid now a non-claude process) must
    // never produce a phantom "running" Claude.
    #[test]
    fn stale_and_recycled_pids_excluded() {
        let sessions = "\
{\"pid\":200,\"cwd\":\"/home/u/dead\",\"status\":\"busy\",\"statusUpdatedAt\":1,\"sessionId\":\"m\"}
{\"pid\":201,\"cwd\":\"/home/u/reused\",\"status\":\"busy\",\"statusUpdatedAt\":2,\"sessionId\":\"n\"}";
        // 200 is absent from ps (dead); 201 is alive but now `vim`, not claude.
        let ps = "\
201  100 pts/1    vim
100   99 pts/1    -bash";
        let panes = "s\t0\tmain\t0\t%1\t/dev/pts/1\t100";
        assert!(build_inventory(sessions, "", ps, panes).is_empty());
    }

    // No controlling tty on the Claude pid (`??`) → fall back to the parent chain:
    // claude 300 -> ppid 100 == the pane's pane_pid.
    #[test]
    fn maps_via_ppid_when_tty_absent() {
        let sessions =
            "{\"pid\":300,\"cwd\":\"/home/u/proj\",\"status\":\"busy\",\"statusUpdatedAt\":1,\"sessionId\":\"p\"}";
        let ps = "\
300  100 ??       claude
100   99 pts/9    -zsh";
        let panes = "s\t0\tmain\t0\t%9\t/dev/pts/9\t100";
        let inv = build_inventory(sessions, "", ps, panes);
        assert_eq!(inv[0]["claude"][0]["paneId"], "%9");
        assert_eq!(inv[0]["claude"][0]["status"], "working");
    }

    // Children stay in tmux pane order; sessions are sorted by name.
    #[test]
    fn children_follow_pane_order_sessions_sorted() {
        let sessions = "\
{\"pid\":10,\"cwd\":\"/a\",\"status\":\"idle\",\"statusUpdatedAt\":1,\"sessionId\":\"1\"}
{\"pid\":11,\"cwd\":\"/b\",\"status\":\"waiting\",\"statusUpdatedAt\":2,\"sessionId\":\"2\"}
{\"pid\":12,\"cwd\":\"/c\",\"status\":\"busy\",\"statusUpdatedAt\":3,\"sessionId\":\"3\"}
{\"pid\":20,\"cwd\":\"/z\",\"status\":\"busy\",\"statusUpdatedAt\":4,\"sessionId\":\"4\"}";
        let ps = "\
10 1 pts/0 claude
11 1 pts/1 claude
12 1 pts/2 claude
20 1 pts/9 claude";
        // zzz's pane is listed before aaa's; the session list must come back
        // name-sorted, and aaa's three panes must stay in pane order 0,1,2.
        let panes = "\
zzz\t0\tmain\t0\t%9\t/dev/pts/9\t20
aaa\t0\tmain\t0\t%0\t/dev/pts/0\t10
aaa\t0\tmain\t1\t%1\t/dev/pts/1\t11
aaa\t0\tmain\t2\t%2\t/dev/pts/2\t12";
        let inv = build_inventory(sessions, "", ps, panes);
        let names: Vec<&str> = inv.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["aaa", "zzz"]);
        let ids: Vec<&str> =
            inv[0]["claude"].as_array().unwrap().iter().map(|c| c["paneId"].as_str().unwrap()).collect();
        assert_eq!(ids, ["%0", "%1", "%2"]);
    }
}
