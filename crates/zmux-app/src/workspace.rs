//! The workspace shell: a splittable tree of panes filling the window, with a
//! left-edge session rail and a host-picker overlay. This is the zmux-native
//! replacement for Zed's `workspace` crate — it owns the split geometry
//! (`pane_group`) and the panes (`pane`) without the project/collab domain
//! model (ADR-0002), and it bridges the `Backend` service to terminal tabs.

use std::collections::HashMap;

use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, Focusable, SharedString, Subscription,
    WeakEntity, Window, actions, div, prelude::*, px, svg,
};
use zmux_transport::TargetId;
use theme::ActiveTheme;

use crate::backend::Backend;
use crate::pane::{Pane, PaneEvent};
use crate::pane_group::{PaneGroup, SplitDirection};
use crate::picker::{HostPicker, HostPickerEvent};
use crate::rail::{RailEvent, RailView};
use crate::state::{PersistedSession, SessionKind, State};
use crate::terminal::TerminalView;

actions!(zmux, [SplitLeft, SplitRight, SplitUp, SplitDown, ClosePane, ToggleRail, OpenHostPicker]);

pub struct Workspace {
    center: PaneGroup,
    focus: FocusHandle,
    /// One subscription per live pane, so an emptied pane prunes itself from the
    /// tree. Rebuilt whenever the pane set changes (see `resubscribe`).
    pane_subs: Vec<Subscription>,
    /// The session rail (always mounted; shown/hidden by `rail_visible`).
    rail: Entity<RailView>,
    rail_sub: Subscription,
    rail_visible: bool,
    /// The host-picker overlay, mounted only while open (the `Cmd+Shift+O`
    /// full-screen-modal presentation).
    picker: Option<Entity<HostPicker>>,
    picker_sub: Option<Subscription>,
    /// Subscription for whichever `HostPicker` the topbar's popover last
    /// created. Independent of `picker`/`picker_sub` above: the popover hosts
    /// its own entity and manages its own show/hide chrome (`ui::PopoverMenu`),
    /// so reusing `self.picker` here would render the same picker twice — once
    /// as the full-screen backdrop, once inside the popover.
    popover_picker_sub: Option<Subscription>,
    /// Open terminal per (server, session name), so a rail row click focuses an
    /// existing tab instead of starting a duplicate. Weak so a closed tab's
    /// entry is lazily reclaimed.
    session_map: HashMap<(TargetId, String), WeakEntity<TerminalView>>,
    /// Persisted servers + adopted session names.
    state: State,
    /// A pane that should be focused on the next render (set after a drag-split
    /// drops a tab into a new pane). `render` has `&mut Window`, which is needed
    /// to call `focus_active`; the event handler doesn't.
    pending_focus: Option<Entity<Pane>>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = State::load();
        let rail = cx.new(|cx| RailView::new(&state, cx));
        let rail_sub = cx.subscribe_in(&rail, window, Self::on_rail_event);

        let pane = cx.new(|cx| Pane::new(window, cx));
        let mut this = Self {
            center: PaneGroup::new(pane),
            focus: cx.focus_handle(),
            pane_subs: Vec::new(),
            rail,
            rail_sub,
            rail_visible: true,
            picker: None,
            picker_sub: None,
            popover_picker_sub: None,
            session_map: HashMap::new(),
            state,
            pending_focus: None,
        };
        this.resubscribe(cx);
        this
    }

    /// Re-subscribe to every current pane's events. Called after any change to
    /// the set of panes; dropping the old subscriptions detaches removed panes.
    fn resubscribe(&mut self, cx: &mut Context<Self>) {
        let panes: Vec<Entity<Pane>> = self.center.panes().into_iter().cloned().collect();
        self.pane_subs = panes
            .into_iter()
            .map(|pane| cx.subscribe(&pane, Self::on_pane_event))
            .collect();
    }

    fn on_pane_event(&mut self, pane: Entity<Pane>, event: &PaneEvent, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Empty => {
                if self.center.panes().len() > 1 && self.center.remove(&pane) {
                    self.resubscribe(cx);
                    cx.notify();
                }
            }
            PaneEvent::SplitDrop { tab, from, direction } => {
                let target = pane.clone();
                // Guard: skip no-op split (already checked in pane's on_drop,
                // but double-check to be safe).
                if from.upgrade().as_ref() == Some(&target) && target.read(cx).tab_count() <= 1 {
                    return;
                }
                let new = cx.new(|_cx| Pane::empty());
                self.center.split(&target, &new, *direction);
                // Move tab into the new pane (no window needed — adopt_tab
                // just pushes and notifies).
                let tab_id = tab.entity_id();
                new.update(cx, |p, cx| p.adopt_tab(tab.clone(), cx));
                // Remove from source pane (remove_item doesn't need window).
                if let Some(src) = from.upgrade() {
                    src.update(cx, |p, cx| p.remove_item(tab_id, cx));
                }
                self.resubscribe(cx);
                self.pending_focus = Some(new);
                cx.notify();
            }
        }
    }

    fn on_rail_event(
        &mut self,
        _rail: &Entity<RailView>,
        event: &RailEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            RailEvent::OpenSession { target, name, kind, folder } => {
                self.open_session(
                    target.clone(),
                    name.clone(),
                    *kind,
                    folder.clone(),
                    window,
                    cx,
                );
            }
            RailEvent::NewShell { target, folder } => {
                self.new_shell(target.clone(), folder.clone(), window, cx);
            }
            RailEvent::Dismissed => {
                self.focus_terminal_area(window, cx);
            }
        }
    }

    fn on_picker_event(
        &mut self,
        _picker: &Entity<HostPicker>,
        event: &HostPickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            HostPickerEvent::Selected(target) => {
                self.connect_server(target.clone(), cx);
                self.close_picker(cx);
                self.focus_terminal_area(window, cx);
            }
            HostPickerEvent::Dismissed => {
                self.close_picker(cx);
                self.focus_terminal_area(window, cx);
            }
        }
    }

    /// Add a server to the rail, persist it, and start its connect+list.
    fn connect_server(&mut self, target: TargetId, cx: &mut Context<Self>) {
        if self.rail.read(cx).has_server(&target) {
            return;
        }
        self.state.add_server(target.clone());
        let state = self.state.clone();
        self.rail.update(cx, |rail, cx| rail.connect(target, &state, cx));
    }

    /// Open (attach to, or focus if already open) a session. The dedup map
    /// keeps one terminal per (server, name); a closed tab's weak ref is
    /// reclaimed on the next lookup.
    fn open_session(
        &mut self,
        target: TargetId,
        name: String,
        kind: SessionKind,
        folder: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(existing) = self
            .session_map
            .get(&(target.clone(), name.clone()))
            .and_then(|w| w.upgrade())
        {
            self.focus_terminal(&existing, window, cx);
            return;
        }

        let backend = cx.global::<Backend>();
        let ensure_rx = backend.ensure(target.clone());
        let label = name.clone();
        let target_ = target.clone();
        let name_ = name.clone();
        let kind_ = kind;
        let folder_ = folder.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(server)) = ensure_rx.await else { return };
            let Ok(cmd) = server.attach_argv(&name_, folder_.as_deref(), 100, 30) else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                let view = cx.new(|cx| TerminalView::new(cx, cmd, Some(label)));
                let pane = this.focused_pane(window, cx);
                pane.update(cx, |pane, cx| pane.add_view(view.clone(), window, cx));
                this.session_map.insert((target_.clone(), name_.clone()), view.downgrade());
                this.state.add_session(
                    &target_,
                    PersistedSession { name: name_.clone(), kind: kind_, folder: folder_.clone() },
                );
                let handle = view.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                this.rail.update(cx, |rail, cx| rail.refresh_list(&target_, cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// Start a new persistent shell on a server. A fresh unique name mints a
    /// new agent session; the daemon's `open_or_attach` creates it on the host.
    fn new_shell(
        &mut self,
        target: TargetId,
        folder: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = format!("zmux-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        self.open_session(target, name, SessionKind::Shell, folder, window, cx);
    }

    /// Make `item` the active tab of whatever pane holds it, and focus it.
    fn focus_terminal(
        &mut self,
        item: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for pane in self.center.panes() {
            if pane.read(cx).has_item(item) {
                let pane = pane.clone();
                pane.update(cx, |pane, cx| pane.activate_item(item, window, cx));
                return;
            }
        }
    }

    /// Focus the active terminal of the focused pane (used when the rail is
    /// dismissed, so the keyboard returns to the terminal area).
    fn focus_terminal_area(&self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.focused_pane(window, cx);
        pane.update(cx, |pane, cx| pane.focus_active(window, cx));
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        self.picker = None;
        self.picker_sub = None;
        cx.notify();
    }

    /// The pane holding focus, falling back to the first pane.
    fn focused_pane(&self, window: &Window, cx: &App) -> Entity<Pane> {
        self.center
            .panes()
            .into_iter()
            .find(|pane| pane.read(cx).contains_focus(window, cx))
            .cloned()
            .unwrap_or_else(|| self.center.first_pane())
    }

    fn split(&mut self, direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.focused_pane(window, cx);
        let new = cx.new(|cx| Pane::new(window, cx));
        self.center.split(&old, &new, direction);
        self.resubscribe(cx);
        if let Some(item) = new.read(cx).active_item() {
            let handle = item.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.focused_pane(window, cx);
        pane.update(cx, |pane, cx| pane.close_active(window, cx));
    }

    fn toggle_rail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rail_visible = !self.rail_visible;
        if self.rail_visible {
            let handle = self.rail.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        } else {
            self.focus_terminal_area(window, cx);
        }
        cx.notify();
    }

    fn open_host_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = cx.new(|cx| HostPicker::new(cx));
        let sub = cx.subscribe_in(&picker, window, Self::on_picker_event);
        let handle = picker.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.picker = Some(picker);
        self.picker_sub = Some(sub);
        cx.notify();
    }

    /// Build a fresh `HostPicker` for the topbar's `ui::PopoverMenu` to host.
    /// The popover manages its own chrome (position, click-outside, focus
    /// restore) and closes on `DismissEvent`, which `HostPicker` now emits
    /// alongside `HostPickerEvent`; this only needs to subscribe to the
    /// latter so a selection still reaches `connect_server`.
    pub(crate) fn open_host_picker_for_popover(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<HostPicker> {
        let picker = cx.new(|cx| HostPicker::new(cx));
        self.popover_picker_sub = Some(cx.subscribe_in(&picker, window, Self::on_picker_event));
        picker
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus the pane that received a drag-split tab. The event handler
        // can't call focus_active (no &mut Window), so it stashes the pane
        // here for one frame. Imperceptible — fires on the same render cycle.
        if let Some(pane) = self.pending_focus.take() {
            pane.update(cx, |p, cx| p.focus_active(window, cx));
        }

        // Collect the non-cx-borrowing parts first. `self.center.render` borrows
        // `cx` mutably, so it is inlined after the `on_action` listeners (which
        // borrow `cx` immutably) to keep the borrows from overlapping.
        let rail = self.rail.clone();
        let picker = self.picker.clone();
        let rail_visible = self.rail_visible;
        let pane_count = self.center.panes().len();
        let colors = cx.theme().colors();
        let bg_color = colors.background;
        let bar_bg = colors.status_bar_background;
        let bar_border = colors.border_variant;
        let icon_color = colors.icon;
        let text_muted = colors.text_muted;
        let rail_icon_path: &'static str = if rail_visible {
            "icons/sidebar_left.svg"
        } else {
            "icons/sidebar_left_closed.svg"
        };
        let pane_label: SharedString = format!("{} panes", pane_count).into();
        let topbar = crate::topbar::render_topbar(cx);

        div()
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &SplitLeft, window, cx| {
                this.split(SplitDirection::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split(SplitDirection::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitUp, window, cx| {
                this.split(SplitDirection::Up, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split(SplitDirection::Down, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                this.close_active_tab(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRail, window, cx| {
                this.toggle_rail(window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenHostPicker, window, cx| {
                this.open_host_picker(window, cx)
            }))
            .size_full()
            .relative()
            .bg(bg_color)
            .flex()
            .flex_col()
            .child(topbar)
            // Main area: rail + pane tree.
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .when(rail_visible, |this| this.child(rail))
                    .child(
                        div()
                            .flex_1()
                            .size_full()
                            .child(self.center.render(window, cx)),
                    ),
            )
            // Bottom bar: rail toggle (left) + pane count (right).
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(24.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .bg(bar_bg)
                    .border_t_1()
                    .border_color(bar_border)
                    .child(
                        div()
                            .id("rail-toggle")
                            .px_2()
                            .h_full()
                            .flex()
                            .items_center()
                            .text_color(icon_color)
                            .child(svg().path(rail_icon_path).size(px(14.)).text_color(icon_color))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.toggle_rail(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .px_2()
                            .text_xs()
                            .text_color(text_muted)
                            .child(pane_label),
                    ),
            )
            .when_some(picker, |this, picker| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::black())
                        .child(picker),
                )
            })
    }
}
