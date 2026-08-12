//! The workspace shell: a splittable tree of panes filling the window. This is
//! the zmux-native replacement for Zed's `workspace` crate — it owns the split
//! geometry (`pane_group`) and the panes (`pane`) without the project/collab
//! domain model (ADR-0002).

use gpui::{App, Context, Entity, FocusHandle, Focusable, Subscription, Window, actions, div, prelude::*, rgb};

use crate::pane::{Pane, PaneEvent};
use crate::pane_group::{PaneGroup, SplitDirection};

actions!(zmux, [SplitLeft, SplitRight, SplitUp, SplitDown, ClosePane]);

pub struct Workspace {
    center: PaneGroup,
    focus: FocusHandle,
    /// One subscription per live pane, so an emptied pane prunes itself from the
    /// tree. Rebuilt whenever the pane set changes (see `resubscribe`).
    pane_subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pane = cx.new(|cx| Pane::new(window, cx));
        let mut this = Self {
            center: PaneGroup::new(pane),
            focus: cx.focus_handle(),
            pane_subs: Vec::new(),
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
        }
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
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .size_full()
            .bg(rgb(0x14110f))
            .child(self.center.render(window, cx))
    }
}
