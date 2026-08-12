//! A `Pane`: a tab strip of terminals, one visible at a time. This is the
//! zmux-native, remote-session-aware tab container promised by ADR-0002 (in
//! place of Zed's `Project`-fused `Pane`/`Item`). The split tree (`pane_group`)
//! arranges `Pane`s; each `Pane` owns its tabs and supports drag-and-drop of a
//! tab within itself or onto another pane.

use gpui::{
    App, ClickEvent, Context, Entity, EntityId, EventEmitter, Focusable, IntoElement, MouseButton,
    MouseDownEvent, SharedString, WeakEntity, Window, div, prelude::*, px, rgb,
};

use crate::terminal::TerminalView;

const TAB_HEIGHT: f32 = 28.;
const BAR_BG: u32 = 0x0a0908;
const ACTIVE_TAB_BG: u32 = 0x14110f;
const ACTIVE_TEXT: u32 = 0xe8e6e1;
const INACTIVE_TEXT: u32 = 0x8a827a;
const DROP_HINT: u32 = 0x2a2621;

/// Emitted when a pane loses its last tab and should be removed from the tree.
pub enum PaneEvent {
    Empty,
}

/// The payload dragged when a tab is picked up: the source pane and the item.
struct TabDrag {
    from: WeakEntity<Pane>,
    item: Entity<TerminalView>,
}

/// A floating label shown under the cursor while a tab is dragged.
struct TabDragPreview {
    title: SharedString,
}

impl Render for TabDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .bg(rgb(DROP_HINT))
            .text_color(rgb(ACTIVE_TEXT))
            .rounded_md()
            .child(self.title.clone())
    }
}

pub struct Pane {
    items: Vec<Entity<TerminalView>>,
    active_ix: usize,
}

impl EventEmitter<PaneEvent> for Pane {}

impl Pane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let item = cx.new(|cx| TerminalView::new_local(cx));
        let pane = Self { items: vec![item], active_ix: 0 };
        pane.focus_active(window, cx);
        pane
    }

    pub fn active_item(&self) -> Option<Entity<TerminalView>> {
        self.items.get(self.active_ix).cloned()
    }

    /// Whether this pane holds the given terminal tab.
    pub fn has_item(&self, item: &Entity<TerminalView>) -> bool {
        self.items.iter().any(|i| i.entity_id() == item.entity_id())
    }

    /// Make `item` the active tab and focus it. No-op if it isn't in this pane.
    pub fn activate_item(
        &mut self,
        item: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.items.iter().position(|i| i.entity_id() == item.entity_id()) {
            self.activate(ix, window, cx);
        }
    }

    /// Insert an already-constructed terminal as a new tab and focus it. Used
    /// when the workspace opens a remote session: the view is built from a
    /// resolved attach argv, then handed to the focused pane.
    pub fn add_view(&mut self, item: Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) {
        self.items.push(item);
        self.active_ix = self.items.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Whether any of this pane's terminals currently holds focus.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.items.iter().any(|item| item.read(cx).has_focus(window))
    }

    /// Focus the active tab's terminal.
    pub fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if let Some(item) = self.active_item() {
            let handle = item.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.items.len() {
            self.active_ix = ix;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn add_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let item = cx.new(|cx| TerminalView::new_local(cx));
        self.items.push(item);
        self.active_ix = self.items.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Close the active tab. Emits `Empty` (for the workspace to prune this pane)
    /// if it was the last one.
    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_at(self.active_ix, window, cx);
    }

    fn close_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.items.len() {
            return;
        }
        self.items.remove(ix);
        if self.items.is_empty() {
            cx.emit(PaneEvent::Empty);
            return;
        }
        if self.active_ix >= self.items.len() {
            self.active_ix = self.items.len() - 1;
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Remove a tab by identity (used when it is dragged out to another pane).
    fn remove_item(&mut self, id: EntityId, cx: &mut Context<Self>) {
        if let Some(pos) = self.items.iter().position(|item| item.entity_id() == id) {
            self.items.remove(pos);
            if self.items.is_empty() {
                cx.emit(PaneEvent::Empty);
                return;
            }
            if self.active_ix >= self.items.len() {
                self.active_ix = self.items.len() - 1;
            }
            cx.notify();
        }
    }

    fn drop_tab(&mut self, drag: &TabDrag, to_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let item_id = drag.item.entity_id();
        if drag.from.entity_id() == cx.entity().entity_id() {
            // Reorder within this pane.
            let Some(from_ix) = self.items.iter().position(|item| item.entity_id() == item_id)
            else {
                return;
            };
            let item = self.items.remove(from_ix);
            let target = if to_ix > from_ix { to_ix - 1 } else { to_ix }.min(self.items.len());
            self.items.insert(target, item);
            self.active_ix = target;
            self.focus_active(window, cx);
            cx.notify();
        } else {
            // Move a tab in from another pane.
            let item = drag.item.clone();
            let _ = drag.from.update(cx, |src, cx| src.remove_item(item_id, cx));
            let target = to_ix.min(self.items.len());
            self.items.insert(target, item);
            self.active_ix = target;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn render_tab(
        &self,
        ix: usize,
        item: &Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = ix == self.active_ix;
        let title: SharedString = item.read(cx).title().into();
        let this = cx.entity();
        let drag_item = item.clone();
        let drag_title = title.clone();

        div()
            .id(ix)
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .px_2()
            .h(px(TAB_HEIGHT))
            .bg(if is_active { rgb(ACTIVE_TAB_BG) } else { rgb(BAR_BG) })
            .text_color(if is_active { rgb(ACTIVE_TEXT) } else { rgb(INACTIVE_TEXT) })
            .child(div().text_color(rgb(INACTIVE_TEXT)).child("›_"))
            .child(title)
            .child(
                // Close button; stops propagation so it doesn't also activate.
                div()
                    .id("close")
                    .px_1()
                    .text_color(rgb(INACTIVE_TEXT))
                    .child("✕")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |pane, _: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            pane.close_at(ix, window, cx);
                        }),
                    ),
            )
            .on_click(cx.listener(move |pane, _: &ClickEvent, window, cx| {
                pane.activate(ix, window, cx);
            }))
            .on_drag(
                TabDrag { from: this.downgrade(), item: drag_item },
                move |_, _, _, cx| cx.new(|_| TabDragPreview { title: drag_title.clone() }),
            )
            .drag_over::<TabDrag>(|style, _, _, _| style.bg(rgb(DROP_HINT)))
            .on_drop(cx.listener(move |pane, drag: &TabDrag, window, cx| {
                pane.drop_tab(drag, ix, window, cx);
            }))
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| self.render_tab(ix, item, cx).into_any_element())
            .collect();
        let end_ix = self.items.len();

        div()
            .id("tab-bar")
            .flex()
            .flex_row()
            .items_center()
            .h(px(TAB_HEIGHT))
            .w_full()
            .bg(rgb(BAR_BG))
            .children(tabs)
            .child(
                div()
                    .id("new-tab")
                    .px_2p5()
                    .h(px(TAB_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(rgb(INACTIVE_TEXT))
                    .child("+")
                    .on_click(cx.listener(|pane, _: &ClickEvent, window, cx| {
                        pane.add_terminal(window, cx);
                    })),
            )
            // Dropping on the empty part of the bar appends the tab.
            .on_drop(cx.listener(move |pane, drag: &TabDrag, window, cx| {
                pane.drop_tab(drag, end_ix, window, cx);
            }))
    }
}

impl Render for Pane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_item();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(ACTIVE_TAB_BG))
            .child(self.render_tab_bar(cx))
            .when_some(active, |this, item| {
                this.child(div().flex_1().size_full().child(item))
            })
    }
}
