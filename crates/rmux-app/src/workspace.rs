//! The workspace shell: a splittable tree of terminal panes filling the window.
//! This is the zmux-native replacement for Zed's `workspace` crate — it owns
//! the split geometry (see `pane.rs`) without the project/collab domain model
//! (ADR-0002). Non-terminal tabs and the remote-session tab trait come later.

use gpui::{
    App, Context, FocusHandle, Focusable, Window, actions, div, prelude::*, rgb,
};

use crate::pane::{PaneGroup, SplitDirection};
use crate::terminal::TerminalView;

actions!(zmux, [SplitLeft, SplitRight, SplitUp, SplitDown, ClosePane]);

pub struct Workspace {
    center: PaneGroup,
    /// The pane new splits/closes act on when none is focused. Focus is the
    /// real source of truth; this is the fallback.
    active_pane: gpui::Entity<TerminalView>,
    focus: FocusHandle,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let first = cx.new(|cx| TerminalView::new(window, cx));
        Self {
            center: PaneGroup::new(first.clone()),
            active_pane: first,
            focus: cx.focus_handle(),
        }
    }

    /// The pane that currently holds focus, falling back to `active_pane`.
    fn focused_pane(&self, window: &Window, cx: &App) -> gpui::Entity<TerminalView> {
        self.center
            .panes()
            .into_iter()
            .find(|pane| pane.read(cx).has_focus(window))
            .cloned()
            .unwrap_or_else(|| self.active_pane.clone())
    }

    fn focus_pane(&mut self, pane: gpui::Entity<TerminalView>, window: &mut Window, cx: &mut App) {
        let handle = pane.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.active_pane = pane;
    }

    fn split(&mut self, direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.focused_pane(window, cx);
        let new = cx.new(|cx| TerminalView::new(window, cx));
        self.center.split(&old, &new, direction);
        self.focus_pane(new, window, cx);
        cx.notify();
    }

    fn close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.center.panes().len() <= 1 {
            return;
        }
        let target = self.focused_pane(window, cx);
        if self.center.remove(&target) {
            let next = self.center.first_pane();
            self.focus_pane(next, window, cx);
            cx.notify();
        }
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
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| this.close_pane(window, cx)))
            .size_full()
            .bg(rgb(0x14110f))
            .child(self.center.render(window, cx))
    }
}
