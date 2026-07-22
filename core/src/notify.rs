//! Claude Code notifications: install a `Stop` + `Notification` hook on a remote
//! host that appends one line per event to `~/.ism/notify.jsonl`, then poll the
//! tail of that file over the shared SSH connection so the UI can raise a native
//! banner when Claude finishes or needs the user.
//!
//! The signal path is file-based (hooks can't reach the tmux pane), so it works
//! whether or not Claude runs inside tmux. The actual banner is fired by the
//! webview via `POST /api/notify` (see `api::notify`); this module only handles
//! install/uninstall/status and the event poll.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::api::{AppState, LOCAL_ID};
use crate::security::safe_name;
use crate::ssh::{self, Target};

/// Substring identifying our hook commands in the user's settings.json — used to
/// find/replace/remove our entries idempotently without touching their own hooks.
const SENTINEL: &str = "ism-notify.sh";

/// The remote helper script. Written verbatim (via a quoted heredoc), so the
/// `$HOME`/`$1`/`$(…)` below are expanded at *run* time on the host, not now.
const SCRIPT: &str = r#"#!/bin/sh
# iShowManagement Claude notifier. $1 = event kind ("stop"|"notification").
# stdin = Claude's JSON payload. Appends one line:
#   <kind>\t<tmux-session>\t<compact-json>
# The tmux column is empty when Claude isn't running inside tmux — the hook
# inherits Claude's $TMUX/$TMUX_PANE, so `#S` resolves to Claude's own session.
d="$HOME/.ism"
mkdir -p "$d" 2>/dev/null
payload=$(cat)
sess=""
[ -n "$TMUX" ] && sess=$(tmux display-message -p '#S' 2>/dev/null)
printf '%s\t%s\t%s\n' "$1" "$sess" "$(printf '%s' "$payload" | tr '\r\n' '  ')" >> "$d/notify.jsonl"
"#;

fn bad(msg: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}
fn oops(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}

/// A hook group `{ "matcher"?: …, "hooks": [{ type, command, timeout }] }`.
fn our_group(kind: &str, matcher: Option<&str>) -> Value {
    let mut g = json!({
        "hooks": [{
            "type": "command",
            "command": format!("$HOME/.claude/ism-notify.sh {kind}"),
            "timeout": 5
        }]
    });
    if let Some(m) = matcher {
        g["matcher"] = json!(m);
    }
    g
}

/// Is this hook group one we installed? (Any inner command mentions our script.)
fn group_is_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| s.contains(SENTINEL))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Drop our previously-installed groups from a `hooks` map's `Stop`/`Notification`
/// arrays, pruning arrays/keys left empty. Leaves the user's own hooks untouched.
fn strip_ours(hooks: &mut serde_json::Map<String, Value>) {
    for key in ["Stop", "Notification"] {
        if let Some(arr) = hooks.get_mut(key).and_then(|v| v.as_array_mut()) {
            arr.retain(|g| !group_is_ours(g));
            if arr.is_empty() {
                hooks.remove(key);
            }
        }
    }
}

/// Merge our hooks into an existing settings value (idempotent — replaces any
/// prior copy of ours). `install=false` only strips ours (for uninstall).
fn apply(mut settings: Value, install: bool) -> Value {
    if !settings.is_object() {
        settings = json!({});
    }
    let obj = settings.as_object_mut().unwrap();
    let hooks_v = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks_v.is_object() {
        *hooks_v = json!({});
    }
    let hooks = hooks_v.as_object_mut().unwrap();

    strip_ours(hooks);

    if install {
        hooks
            .entry("Stop")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(our_group("stop", None));
        let notif = hooks
            .entry("Notification")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();
        notif.push(our_group("notification", Some("permission_prompt")));
        notif.push(our_group("notification", Some("idle_prompt")));
    }

    // Tidy: drop an empty hooks object so we don't leave `{"hooks":{}}` behind.
    if hooks_v.as_object().map(|m| m.is_empty()).unwrap_or(false) {
        obj.remove("hooks");
    }
    settings
}

/// A quoted-heredoc write (no shell expansion of the body). Content must not
/// contain a line equal to the delimiter — our JSON/script never will.
fn heredoc(path: &str, body: &str) -> String {
    format!("cat > {path} << 'ISM_EOF'\n{body}\nISM_EOF\n")
}

fn guard(id: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if id == LOCAL_ID {
        return Err(bad("Claude notifications are for remote hosts"));
    }
    if !safe_name(id) {
        return Err(bad("bad server id"));
    }
    Ok(())
}

/// `GET /api/servers/{id}/claude-notify` — is the hook installed on the host?
pub async fn status(
    State(_): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = guard(&id) {
        return e;
    }
    let cmd = "test -f \"$HOME/.claude/ism-notify.sh\" \
        && grep -q 'ism-notify.sh' \"$HOME/.claude/settings.json\" 2>/dev/null \
        && echo INSTALLED || echo NO";
    let r = ssh::exec(Target::Remote(&id), cmd, Duration::from_secs(15)).await;
    if !r.ok && r.stdout.trim().is_empty() {
        let hint = if r.stderr.trim().is_empty() {
            "can't reach host — open a Console first to authenticate"
        } else {
            r.stderr.trim()
        };
        return (StatusCode::OK, Json(json!({ "reachable": false, "installed": false, "reason": hint })));
    }
    let installed = r.stdout.contains("INSTALLED");
    (StatusCode::OK, Json(json!({ "reachable": true, "installed": installed })))
}

/// `POST /api/servers/{id}/claude-notify` — install the hook + helper script,
/// merging into any existing `~/.claude/settings.json`.
pub async fn install(
    State(_): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = guard(&id) {
        return e;
    }
    // Read the current settings (empty if absent), merge, write everything back.
    let read = ssh::exec(
        Target::Remote(&id),
        "cat \"$HOME/.claude/settings.json\" 2>/dev/null || true",
        Duration::from_secs(15),
    )
    .await;
    if !read.ok && read.stdout.trim().is_empty() && !read.stderr.trim().is_empty() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, &short(&read.stderr));
    }

    let existing: Value = if read.stdout.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&read.stdout) {
            Ok(v) => v,
            Err(_) => {
                return oops(
                    StatusCode::CONFLICT,
                    "~/.claude/settings.json is not valid JSON — fix it and retry",
                )
            }
        }
    };
    let merged = apply(existing, true);
    let settings_json = serde_json::to_string_pretty(&merged).unwrap_or_else(|_| "{}".into());

    let cmd = format!(
        "set -e\nmkdir -p \"$HOME/.claude\" \"$HOME/.ism\"\n{}chmod +x \"$HOME/.claude/ism-notify.sh\"\n{}",
        heredoc("\"$HOME/.claude/ism-notify.sh\"", SCRIPT),
        heredoc("\"$HOME/.claude/settings.json\"", &settings_json),
    );
    let w = ssh::exec(Target::Remote(&id), &cmd, Duration::from_secs(20)).await;
    if !w.ok {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, &short(&w.stderr));
    }
    (StatusCode::OK, Json(json!({ "ok": true, "installed": true })))
}

/// `DELETE /api/servers/{id}/claude-notify` — remove our hooks + script.
pub async fn uninstall(
    State(_): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = guard(&id) {
        return e;
    }
    let read = ssh::exec(
        Target::Remote(&id),
        "cat \"$HOME/.claude/settings.json\" 2>/dev/null || true",
        Duration::from_secs(15),
    )
    .await;
    let existing: Value = serde_json::from_str(read.stdout.trim()).unwrap_or_else(|_| json!({}));
    let cleaned = apply(existing, false);
    let settings_json = serde_json::to_string_pretty(&cleaned).unwrap_or_else(|_| "{}".into());

    let cmd = format!(
        "rm -f \"$HOME/.claude/ism-notify.sh\"\n{}",
        heredoc("\"$HOME/.claude/settings.json\"", &settings_json),
    );
    let w = ssh::exec(Target::Remote(&id), &cmd, Duration::from_secs(20)).await;
    if !w.ok {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, &short(&w.stderr));
    }
    (StatusCode::OK, Json(json!({ "ok": true, "installed": false })))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    /// Byte offset already consumed. Absent = initialize (return current size,
    /// no history, so opening a host doesn't replay a banner storm).
    pub cursor: Option<u64>,
}

#[derive(Serialize)]
struct NotifyEvent {
    kind: String,
    /// tmux session Claude ran in, or `None` for a plain (non-tmux) shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux: Option<String>,
    #[serde(rename = "notificationType", skip_serializing_if = "Option::is_none")]
    notification_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

/// `GET /api/servers/{id}/claude-notify/events?cursor=N` — new lines since byte N.
pub async fn events(
    State(_): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = guard(&id) {
        return e;
    }
    let cmd = match q.cursor {
        None => "f=\"$HOME/.ism/notify.jsonl\"; sz=$(wc -c < \"$f\" 2>/dev/null || echo 0); \
                 echo \"ISMCURSOR $sz\""
            .to_string(),
        Some(c) => format!(
            "f=\"$HOME/.ism/notify.jsonl\"; sz=$(wc -c < \"$f\" 2>/dev/null || echo 0); \
             start={c}; [ \"$sz\" -lt \"$start\" ] && start=0; echo \"ISMCURSOR $sz\"; \
             [ \"$sz\" -gt \"$start\" ] && tail -c +$((start+1)) \"$f\" 2>/dev/null || true",
        ),
    };
    let r = ssh::exec(Target::Remote(&id), &cmd, Duration::from_secs(15)).await;
    if !r.ok && r.stdout.trim().is_empty() {
        return (StatusCode::OK, Json(json!({ "cursor": q.cursor.unwrap_or(0), "events": [] })));
    }

    let mut lines = r.stdout.lines();
    let cursor = lines
        .next()
        .and_then(|l| l.strip_prefix("ISMCURSOR "))
        .and_then(|n| n.trim().parse::<u64>().ok())
        .unwrap_or_else(|| q.cursor.unwrap_or(0));

    let events: Vec<NotifyEvent> = lines
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_event)
        .collect();

    (StatusCode::OK, Json(json!({ "cursor": cursor, "events": events })))
}

/// What the UI reports it's actively watching, and when. Fired banners are
/// suppressed only for `host` while the report is recent — when the webview is
/// backgrounded it suspends, the reports stop, this goes stale, and banners fire.
struct Watch {
    host: Option<String>,
    at: Option<Instant>,
    /// Host alias → tmux session names currently open as tabs in the app. Scopes
    /// notifications to the sessions the user has attached; unlike `host`/`at`,
    /// this is used without a TTL — the tabs stay open while the app is
    /// backgrounded (when the heartbeat that refreshes it naturally stops).
    open_tmux: HashMap<String, Vec<String>>,
}
static WATCH: LazyLock<Mutex<Watch>> =
    LazyLock::new(|| Mutex::new(Watch { host: None, at: None, open_tmux: HashMap::new() }));

/// The report is only trusted this long; a suspended webview stops sending.
const WATCH_TTL: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
pub struct WatchReq {
    /// Host alias whose terminal is on screen right now; `null` when the app is
    /// backgrounded or its active tab isn't a live terminal.
    #[serde(default, rename = "activeHost")]
    pub active_host: Option<String>,
    /// Host alias → tmux session names open as tabs in the app (all live hosts).
    #[serde(default, rename = "openTmux")]
    pub open_tmux: HashMap<String, Vec<String>>,
}

/// `POST /api/watching` — the UI heartbeats which host it's actively watching and
/// which tmux sessions it has open.
pub async fn set_watching(Json(w): Json<WatchReq>) -> StatusCode {
    let mut g = WATCH.lock().unwrap();
    g.host = w.active_host;
    g.at = Some(Instant::now());
    g.open_tmux = w.open_tmux;
    StatusCode::NO_CONTENT
}

/// Is the user watching `id`'s terminal right now (fresh report + matching host)?
fn is_watching(id: &str) -> bool {
    let g = WATCH.lock().unwrap();
    matches!((g.at, g.host.as_deref()), (Some(at), Some(h)) if h == id && at.elapsed() < WATCH_TTL)
}

/// Should an event from `id` be delivered? A tmux event passes only if that
/// session is open as a tab in the app; an event with no tmux session (Claude in
/// a plain console) always passes.
fn tmux_allowed(id: &str, sess: Option<&str>) -> bool {
    let Some(name) = sess else { return true };
    let g = WATCH.lock().unwrap();
    g.open_tmux.get(id).is_some_and(|v| v.iter().any(|s| s == name))
}

/// Banner title/body for an event, labelled by host alias.
fn banner_text(host: &str, ev: &NotifyEvent) -> (String, String) {
    if ev.kind == "stop" {
        let body = ev
            .summary
            .clone()
            .or_else(|| ev.project.as_ref().map(|p| format!("Done in {p}")))
            .unwrap_or_else(|| "Task complete".into());
        return (format!("{host} · Claude finished"), body);
    }
    if ev.notification_type.as_deref() == Some("permission_prompt") {
        let body = ev.message.clone().unwrap_or_else(|| "Waiting for permission".into());
        return (format!("{host} · Claude needs you"), body);
    }
    let body = ev
        .message
        .clone()
        .or_else(|| ev.project.as_ref().map(|p| format!("in {p}")))
        .unwrap_or_else(|| "Waiting for your input".into());
    (format!("{host} · Claude is waiting"), body)
}

#[derive(Deserialize)]
pub struct NotifyWsParams {
    pub id: String,
}

/// `GET /ws/notify?id=<alias>` — push Claude events live as they land. Core tails
/// the remote `~/.ism/notify.jsonl` over the shared SSH connection and sends one
/// JSON message per event. Runs in this native process (not the webview), so the
/// stream keeps delivering even when the app window is hidden/minimized — where a
/// webview timer would be throttled. The tail follows from end (no history replay)
/// and dies with the socket.
pub async fn notify_ws(
    ws: WebSocketUpgrade,
    Query(p): Query<NotifyWsParams>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| notify_stream(socket, p.id))
}

async fn notify_stream(mut socket: WebSocket, id: String) {
    // Returning drops the socket, which closes it.
    if id == LOCAL_ID || !safe_name(&id) {
        return;
    }

    // Ride the console's ControlMaster instead of racing a second cloudflared dial.
    ssh::wait_for_master(&id, Duration::from_secs(3)).await;

    let mut cmd = tokio::process::Command::new("ssh");
    for a in ssh::exec_args(&id) {
        cmd.arg(a);
    }
    // `-n 0` = no history; `-F` follows across rotation and waits if the file
    // doesn't exist yet (installed but Claude hasn't fired an event).
    cmd.arg("tail -n 0 -F \"$HOME/.ism/notify.jsonl\" 2>/dev/null");
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    cmd.kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return,
    };
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    let mut lines = BufReader::new(stdout).lines();

    loop {
        tokio::select! {
            next = lines.next_line() => {
                match next {
                    Ok(Some(line)) => {
                        let Some(ev) = parse_event(&line) else { continue };
                        // Scope to the tmux sessions the user has open in the app.
                        // A plain-console event (no tmux) always passes; a tmux
                        // event only passes if that session is an open tab.
                        if !tmux_allowed(&id, ev.tmux.as_deref()) {
                            continue;
                        }
                        // Fire the OS banner from here — the native process is
                        // never suspended, so it works while the app window is
                        // backgrounded (where the webview's JS is frozen).
                        // Suppress only when the user is watching this host now.
                        if !is_watching(&id) {
                            let (title, body) = banner_text(&id, &ev);
                            crate::api::deliver_banner(title, body, None).await;
                        }
                        // Still push to the webview for the in-app sidebar badge.
                        if let Ok(txt) = serde_json::to_string(&ev) {
                            if socket.send(Message::Text(txt.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    _ => break, // tail/ssh exited or read error
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {} // ignore client frames / ping-pong
                }
            }
        }
    }
    let _ = child.start_kill();
}

/// Parse one notify line into a compact event for the UI. New lines are
/// `<kind>\t<tmux-session>\t<payload-json>`; lines written before the tmux column
/// existed are `<kind>\t<payload-json>` (no session).
fn parse_event(line: &str) -> Option<NotifyEvent> {
    let mut parts = line.splitn(3, '\t');
    let kind = parts.next()?;
    let second = parts.next()?;
    // Three columns → middle is the tmux session; two → the old format, no session.
    let (sess, raw) = match parts.next() {
        Some(payload) => (second, payload),
        None => ("", second),
    };
    let p: Value = serde_json::from_str(raw.trim()).unwrap_or(json!({}));
    let cwd = p.get("cwd").and_then(|v| v.as_str());
    let project = cwd.map(|c| c.rsplit('/').next().unwrap_or(c).to_string());
    let summary = p
        .get("last_assistant_message")
        .and_then(|v| v.as_str())
        .map(|s| s.chars().take(140).collect::<String>());
    Some(NotifyEvent {
        kind: kind.to_string(),
        tmux: if sess.is_empty() { None } else { Some(sess.to_string()) },
        notification_type: p.get("notification_type").and_then(|v| v.as_str()).map(String::from),
        message: p.get("message").and_then(|v| v.as_str()).map(String::from),
        project,
        summary,
    })
}

fn short(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        "command failed".into()
    } else {
        t.chars().take(300).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_is_idempotent_and_preserves_user_hooks() {
        let user = json!({
            "model": "opus",
            "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "my-own" } ] } ] }
        });
        let once = apply(user.clone(), true);
        let twice = apply(once.clone(), true);
        assert_eq!(once, twice, "installing twice must be stable");

        // User's own Stop hook survives.
        let stop = once["hooks"]["Stop"].as_array().unwrap();
        assert!(stop.iter().any(|g| g["hooks"][0]["command"] == "my-own"));
        // Exactly one of ours in Stop; two matchers in Notification.
        assert_eq!(stop.iter().filter(|g| group_is_ours(g)).count(), 1);
        assert_eq!(once["hooks"]["Notification"].as_array().unwrap().len(), 2);
        // Unrelated keys untouched.
        assert_eq!(once["model"], "opus");
    }

    #[test]
    fn uninstall_removes_only_ours_and_prunes() {
        let user = json!({ "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "keep" } ] } ] } });
        let installed = apply(user, true);
        let removed = apply(installed, false);
        let stop = removed["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], "keep");
        // Notification array had only ours → key pruned entirely.
        assert!(removed["hooks"].get("Notification").is_none());
    }

    #[test]
    fn uninstall_from_only_ours_leaves_clean_object() {
        let installed = apply(json!({}), true);
        let removed = apply(installed, false);
        // No hooks left at all → hooks key pruned.
        assert!(removed.get("hooks").is_none(), "{removed}");
    }

    #[test]
    fn parse_event_extracts_project_and_message() {
        // Old two-column format (no tmux session) still parses.
        let line = "notification\t{\"cwd\":\"/home/u/my-proj\",\"notification_type\":\"permission_prompt\",\"message\":\"Allow Write?\"}";
        let e = parse_event(line).unwrap();
        assert_eq!(e.kind, "notification");
        assert_eq!(e.tmux, None);
        assert_eq!(e.notification_type.as_deref(), Some("permission_prompt"));
        assert_eq!(e.message.as_deref(), Some("Allow Write?"));
        assert_eq!(e.project.as_deref(), Some("my-proj"));
    }

    #[test]
    fn parse_event_reads_tmux_column() {
        // New three-column format carries the tmux session; empty column → None.
        let with = parse_event("stop\tmain\t{\"cwd\":\"/home/u/proj\"}").unwrap();
        assert_eq!(with.tmux.as_deref(), Some("main"));
        assert_eq!(with.project.as_deref(), Some("proj"));

        let without = parse_event("stop\t\t{\"cwd\":\"/home/u/proj\"}").unwrap();
        assert_eq!(without.tmux, None);
    }
}
