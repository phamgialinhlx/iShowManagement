//! The session rail: a left-edge tree of connected servers and their
//! `rmux-agent` sessions, grouped by working directory. This is the
//! zmux-native, remote-session-aware replacement for the old `WorkspaceRail` —
//! a Server → Project → Session tree (ADR-0002).
//!
//! The rail *shows* state and *emits* intents; it never opens terminals itself.
//! The workspace subscribes to `RailEvent` and does the ensure→attach work, so
//! the rail stays free of terminal/pane concerns. Live session lists come from
//! `backend.list`; Claude status comes from `backend.watch_status` (one push
//! stream per host, no polling); adopted session names survive restart in
//! `state.json`.

use std::collections::HashMap;

use futures::channel::mpsc;
use futures::StreamExt as _;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, IntoElement, KeyDownEvent,
    Keystroke, SharedString, Window, div, prelude::*, px,
};
use rmux_transport::TargetId;
use theme::ActiveTheme;

use crate::backend::{self, AgentSession, Backend, StatusLine};
use crate::state::{SessionKind, State};

/// A rail row the cursor can land on. Project headers are visual only.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RailItem {
    Server(usize),
    Session { server: usize, session: usize },
}

/// What the rail asks the workspace to do. The workspace owns the terminal
/// dedup map, so "open" covers both focus-existing and create-new.
#[derive(Clone, Debug)]
pub enum RailEvent {
    /// Open (attach to, or focus if already open) a session.
    OpenSession { target: TargetId, name: String, kind: SessionKind, folder: Option<String> },
    /// Start a new persistent shell on a server, optionally in a folder.
    NewShell { target: TargetId, folder: Option<String> },
    /// The rail was dismissed (esc); return focus to the terminal area.
    Dismissed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ConnectState {
    Connecting,
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LiveStatus {
    Idle,
    Busy,
    Waiting,
    /// A shell, or a Claude session whose status is unknown. Rendered as idle.
    Shell,
    /// Persisted but not reported by the host — the session died. Click to
    /// revive (the daemon re-creates it under the same name).
    Gone,
}

struct SessionNode {
    name: String,
    alias: Option<String>,
    kind: SessionKind,
    folder: Option<String>,
    status: LiveStatus,
    attached: bool,
    live: bool,
}

struct ServerNode {
    target: TargetId,
    label: SharedString,
    state: ConnectState,
    sessions: Vec<SessionNode>,
    folded: bool,
}

/// The rail view. One per workspace.
pub struct RailView {
    focus: FocusHandle,
    servers: Vec<ServerNode>,
    /// Cursor into the flat row list. Clamped on each structural change.
    cursor: usize,
    /// Claude status by host, then by cwd (the only key shared between the
    /// agent's session list and `watch-status`'s Claude updates).
    status_by_host: HashMap<TargetId, HashMap<String, String>>,
}

impl EventEmitter<RailEvent> for RailView {}

impl RailView {
    pub fn new(state: &State, cx: &mut Context<Self>) -> Self {
        let mut rail = Self {
            focus: cx.focus_handle(),
            servers: Vec::new(),
            cursor: 0,
            status_by_host: HashMap::new(),
        };
        // Repopulate from persisted state: each saved server reconnects, and
        // its saved session names show immediately (as "gone" until the live
        // list lands). A relaunch looks like the rail you left.
        for server in &state.servers {
            rail.connect(server.target.clone(), state, cx);
        }
        rail
    }

    /// Connect to a target, list its sessions, and start its status stream.
    /// Safe to call for an already-added server (idempotent on the backend).
    pub fn connect(&mut self, target: TargetId, state: &State, cx: &mut Context<Self>) {
        let label: SharedString = target.label().into();
        let persisted = state.server(&target).map(|s| s.sessions.clone()).unwrap_or_default();
        if !self.servers.iter().any(|s| s.target == target) {
            let sessions = persisted
                .into_iter()
                .map(|p| SessionNode {
                    name: p.name,
                    alias: None,
                    kind: p.kind,
                    folder: p.folder,
                    status: LiveStatus::Gone,
                    attached: false,
                    live: false,
                })
                .collect();
            self.servers.push(ServerNode {
                target: target.clone(),
                label,
                state: ConnectState::Connecting,
                sessions,
                folded: false,
            });
        }
        cx.notify();

        let backend = cx.global::<Backend>();
        let ensure_rx = backend.ensure(target.clone());
        let target2 = target.clone();
        cx.spawn(async move |this, cx| {
            let result = match ensure_rx.await {
                Ok(inner) => inner,
                Err(_) => Err("connect cancelled".to_string()),
            };
            let ok = result.is_ok();
            if let Err(e) = &result {
                log::warn!("connect {} failed: {e}", target2.label());
            }
            let _ = this.update(cx, |view, cx| {
                view.set_connect_state(&target2, match &result {
                    Ok(_) => ConnectState::Ready,
                    Err(e) => ConnectState::Failed(e.clone()),
                });
                cx.notify();
            });
            if ok {
                let _ = this.update(cx, |view, cx| {
                    view.refresh_list(&target2, cx);
                    view.start_status(&target2, cx);
                });
            }
        })
        .detach();
    }

    fn set_connect_state(&mut self, target: &TargetId, state: ConnectState) {
        if let Some(s) = self.servers.iter_mut().find(|s| &s.target == target) {
            s.state = state;
        }
    }

    /// Re-fetch the live session list for a server and merge it with persisted
    /// names. Live sessions update folder/status; persisted names not in the
    /// live list stay as "gone".
    pub fn refresh_list(&self, target: &TargetId, cx: &mut Context<Self>) {
        let backend = cx.global::<Backend>();
        let rx = backend.list(target);
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(sessions)) = rx.await.map_err(|e| e.to_string()) else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                view.merge_live(&target, sessions);
                cx.notify();
            });
        })
        .detach();
    }

    fn start_status(&self, target: &TargetId, cx: &mut Context<Self>) {
        let (tx, mut rx) = mpsc::unbounded::<StatusLine>();
        cx.global::<Backend>().watch_status(target, tx);
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            while let Some(line) = rx.next().await {
                let _ = this.update(cx, |view, cx| {
                    view.apply_status(&target, &line);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn apply_status(&mut self, target: &TargetId, line: &StatusLine) {
        if let StatusLine::Update(up) = line {
            if let Some(cwd) = &up.cwd {
                self.status_by_host
                    .entry(target.clone())
                    .or_default()
                    .insert(cwd.clone(), up.status.clone());
            }
        }
    }

    /// Fold the live agent session list into the tree, keeping persisted names
    /// that are no longer live as "gone" rows.
    fn merge_live(&mut self, target: &TargetId, live: Vec<AgentSession>) {
        let state = State::load();
        let persisted = state.server(target).map(|s| s.sessions.clone()).unwrap_or_default();
        let host_status = self.status_by_host.get(target).cloned();

        let mut live_names: Vec<SessionNode> = live
            .into_iter()
            .map(|a| {
                let kind = if backend::is_claude_session(&a.command) {
                    SessionKind::Claude
                } else {
                    SessionKind::Shell
                };
                let status = live_status(kind, &a.cwd, host_status.as_ref());
                SessionNode {
                    name: a.name,
                    alias: a.alias,
                    kind,
                    folder: a.cwd,
                    status,
                    attached: a.attached,
                    live: true,
                }
            })
            .collect();

        // Persisted names not in the live list stay as "gone" rows so a click
        // can revive them under the same name.
        let live_set: std::collections::HashSet<String> =
            live_names.iter().map(|s| s.name.clone()).collect();
        for p in persisted {
            if !live_set.contains(p.name.as_str()) {
                live_names.push(SessionNode {
                    name: p.name,
                    alias: None,
                    kind: p.kind,
                    folder: p.folder,
                    status: LiveStatus::Gone,
                    attached: false,
                    live: false,
                });
            }
        }

        if let Some(srv) = self.servers.iter_mut().find(|s| &s.target == target) {
            srv.sessions = live_names;
        }
    }

    /// The flat list of selectable rows, in display order. Servers that are
    /// folded contribute only their header.
    fn flat(&self) -> Vec<RailItem> {
        let mut out = Vec::new();
        for (six, server) in self.servers.iter().enumerate() {
            out.push(RailItem::Server(six));
            if !server.folded {
                for (ix, _) in server.sessions.iter().enumerate() {
                    out.push(RailItem::Session { server: six, session: ix });
                }
            }
        }
        out
    }

    fn clamp_cursor(&mut self) {
        let len = self.flat().len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.flat().len();
        if len == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        if next != self.cursor {
            self.cursor = next;
            cx.notify();
        }
    }

    fn activate(&mut self, cx: &mut Context<Self>) {
        let items = self.flat();
        let Some(item) = items.get(self.cursor).copied() else { return };
        match item {
            RailItem::Server(six) => {
                if let Some(s) = self.servers.get_mut(six) {
                    s.folded = !s.folded;
                }
                cx.notify();
            }
            RailItem::Session { server, session } => {
                let srv = self.servers.get(server);
                let sess = srv.and_then(|s| s.sessions.get(session));
                let Some((target, name, kind, folder)) =
                    sess.map(|s| (srv.unwrap().target.clone(), s.name.clone(), s.kind, s.folder.clone()))
                else {
                    return;
                };
                cx.emit(RailEvent::OpenSession { target, name, kind, folder });
            }
        }
    }

    fn new_shell(&mut self, cx: &mut Context<Self>) {
        let items = self.flat();
        let Some(item) = items.get(self.cursor).copied() else { return };
        let server_ix = match item {
            RailItem::Server(six) => six,
            RailItem::Session { server, .. } => server,
        };
        let Some(srv) = self.servers.get(server_ix) else { return };
        let target = srv.target.clone();
        let folder = match item {
            RailItem::Session { session, .. } => {
                srv.sessions.get(session).and_then(|s| s.folder.clone())
            }
            _ => None,
        };
        cx.emit(RailEvent::NewShell { target, folder });
    }

    fn kill_selected(&self, cx: &mut Context<Self>) {
        let items = self.flat();
        let Some(RailItem::Session { server, session }) = items.get(self.cursor).copied() else {
            return;
        };
        let (target, name) = {
            let Some(srv) = self.servers.get(server) else { return };
            let Some(sess) = srv.sessions.get(session) else { return };
            (srv.target.clone(), sess.name.clone())
        };
        let backend = cx.global::<Backend>();
        let rx = backend.kill(&target, &name);
        cx.spawn(async move |this, cx| {
            let _ = rx.await;
            let _ = this.update(cx, |view, cx| {
                view.refresh_list(&target, cx);
            });
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        match ks.key.as_str() {
            "up" | "k" if no_mods(ks) => self.move_cursor(-1, cx),
            "down" | "j" if no_mods(ks) => self.move_cursor(1, cx),
            "enter" if no_mods(ks) => self.activate(cx),
            "n" if no_mods(ks) => self.new_shell(cx),
            "x" if no_mods(ks) => self.kill_selected(cx),
            "escape" if no_mods(ks) => cx.emit(RailEvent::Dismissed),
            _ => {}
        }
    }

    /// Whether a server is already in the rail (for the picker).
    pub fn has_server(&self, target: &TargetId) -> bool {
        self.servers.iter().any(|s| &s.target == target)
    }
}

fn no_mods(ks: &Keystroke) -> bool {
    !ks.modifiers.platform && !ks.modifiers.control && !ks.modifiers.alt
}

/// Resolve a session's live status. Shells are always idle; Claude sessions
/// take the `watch-status` value for their cwd (the only shared key), defaulting
/// to idle when the host hasn't reported one.
fn live_status(
    kind: SessionKind,
    cwd: &Option<String>,
    host_status: Option<&HashMap<String, String>>,
) -> LiveStatus {
    match kind {
        SessionKind::Shell => LiveStatus::Shell,
        SessionKind::Claude => {
            let Some(cwd) = cwd.as_deref() else { return LiveStatus::Idle };
            let Some(map) = host_status else { return LiveStatus::Idle };
            match map.get(cwd).map(String::as_str) {
                Some("busy") => LiveStatus::Busy,
                Some("waiting") => LiveStatus::Waiting,
                _ => LiveStatus::Idle,
            }
        }
    }
}

impl Focusable for RailView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────

impl Render for RailView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.clamp_cursor();
        let items = self.flat();
        let colors = cx.theme().colors();
        let status = cx.theme().status();

        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(items.len());
        for (row_ix, item) in items.iter().enumerate() {
            let selected = row_ix == self.cursor;
            let row = match *item {
                RailItem::Server(six) => {
                    let Some(srv) = self.servers.get(six) else { continue };
                    server_row(srv, selected, colors, status).into_any_element()
                }
                RailItem::Session { server, session } => {
                    let Some(srv) = self.servers.get(server) else { continue };
                    let Some(sess) = srv.sessions.get(session) else { continue };
                    let host_status = self.status_by_host.get(&srv.target);
                    let live = if sess.live {
                        live_status(sess.kind, &sess.folder, host_status)
                    } else {
                        LiveStatus::Gone
                    };
                    session_row(sess, &live, selected, colors, status).into_any_element()
                }
            };
            rows.push(row);
        }

        div()
            .track_focus(&self.focus)
            .key_context("Rail")
            .on_key_down(cx.listener(Self::on_key))
            .h_full()
            .w(px(240.))
            .flex_shrink_0()
            .bg(colors.panel_background)
            .border_r_1()
            .border_color(colors.border_variant)
            .flex()
            .flex_col()
            .children(rows)
    }
}

fn server_row(
    srv: &ServerNode,
    selected: bool,
    colors: &theme::ThemeColors,
    status: &theme::StatusColors,
) -> impl IntoElement {
    let arrow = if srv.folded { "›" } else { "⌄" };
    let (state_label, state_color) = match &srv.state {
        ConnectState::Connecting => ("…", colors.text_disabled),
        ConnectState::Ready => ("", colors.text_disabled),
        ConnectState::Failed(_) => ("!", status.warning),
    };
    div()
        .id("server")
        .w_full()
        .px_2()
        .py_1()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .bg(if selected { colors.element_active } else { colors.panel_background })
        .text_color(colors.text_muted)
        .text_sm()
        .child(div().w_3().text_color(colors.text_disabled).child(arrow))
        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(srv.label.clone()))
        .when(!state_label.is_empty(), |this| {
            this.child(div().text_color(state_color).child(state_label))
        })
}

fn session_row(
    sess: &SessionNode,
    status: &LiveStatus,
    selected: bool,
    colors: &theme::ThemeColors,
    status_colors: &theme::StatusColors,
) -> impl IntoElement {
    let icon = match sess.kind {
        SessionKind::Shell => "›_",
        SessionKind::Claude => "◇",
    };
    let dot_color = status_color(status, colors, status_colors);
    let label = sess.alias.clone().unwrap_or_else(|| sess.name.clone());
    let dim = !sess.live;
    let text = if dim { colors.text_disabled } else { colors.text_muted };
    let folder: SharedString = sess
        .folder
        .as_deref()
        .and_then(|f| f.rsplit('/').next())
        .unwrap_or("(other)")
        .to_string()
        .into();

    div()
        .id("session")
        .w_full()
        .pl_6()
        .pr_2()
        .py_0p5()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .bg(if selected { colors.element_active } else { colors.panel_background })
        .text_color(text)
        .text_sm()
        .child(div().w_3().text_color(colors.text_disabled).child(icon))
        .child(div().w_2().h_2().rounded_full().bg(dot_color))
        .child(div().flex_1().child(label))
        .child(div().text_xs().text_color(colors.text_disabled).child(folder))
        .when(sess.attached, |this| {
            this.child(div().text_xs().text_color(colors.text_accent).child("●"))
        })
}

fn status_color(
    status: &LiveStatus,
    colors: &theme::ThemeColors,
    status_colors: &theme::StatusColors,
) -> Hsla {
    match status {
        LiveStatus::Busy => status_colors.info,
        LiveStatus::Waiting => status_colors.warning,
        LiveStatus::Idle => status_colors.success,
        LiveStatus::Shell => colors.text_disabled,
        LiveStatus::Gone => colors.text_disabled,
    }
}
