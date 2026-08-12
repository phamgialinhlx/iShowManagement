//! The host picker: a keyboard-driven overlay for connecting to an SSH host
//! (or the local machine). Opened with `cmd-shift-o` from the workspace.
//!
//! A filter-as-you-type list over `~/.ssh/config` hosts (`Backend::hosts`) plus
//! a `Local` entry. Selecting a host emits `Selected(TargetId)`, which the
//! workspace turns into a rail server + connect. There is no real text input —
//! the query is built from raw key-downs, like the rail, so the picker is
//! fully keyboard-driven and needs no IME/text-field machinery.

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, SharedString,
    Window, div, prelude::*, px, rgb,
};
use rmux_ssh::config::ConfigHost;
use rmux_transport::{SshHostId, TargetId};

use crate::backend::Backend;

#[derive(Clone, Debug)]
pub enum HostPickerEvent {
    Selected(TargetId),
    Dismissed,
}

/// One selectable row.
#[derive(Clone)]
struct HostEntry {
    target: TargetId,
    alias: SharedString,
    detail: Option<SharedString>,
}

pub struct HostPicker {
    focus: FocusHandle,
    query: String,
    entries: Vec<HostEntry>,
    cursor: usize,
}

impl EventEmitter<HostPickerEvent> for HostPicker {}

impl HostPicker {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // `Local` first, then SSH-config hosts in file order. Local is the
        // common case for a scratch shell and worth a zero-typing shortcut.
        let mut entries = vec![HostEntry {
            target: TargetId::Local,
            alias: "local".into(),
            detail: None,
        }];
        for host in Backend::hosts() {
            entries.push(host_entry(host));
        }
        Self { focus: cx.focus_handle(), query: String::new(), entries, cursor: 0 }
    }

    fn filtered(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                q.is_empty()
                    || e.alias.to_lowercase().contains(&q)
                    || e.detail.as_ref().is_some_and(|d| d.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn clamp_cursor(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.filtered().len();
        if len == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        if next != self.cursor {
            self.cursor = next;
            cx.notify();
        }
    }

    fn select(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.filtered().get(self.cursor).copied() else {
            return;
        };
        let target = self.entries[ix].target.clone();
        cx.emit(HostPickerEvent::Selected(target));
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.alt {
            return;
        }
        match ks.key.as_str() {
            "up" | "k" => self.move_cursor(-1, cx),
            "down" | "j" => self.move_cursor(1, cx),
            "enter" => self.select(cx),
            "escape" => cx.emit(HostPickerEvent::Dismissed),
            "backspace" => {
                self.query.pop();
                self.clamp_cursor();
                cx.notify();
            }
            _ => {
                // Append a typed character. `key_char` carries the
                // layout-resolved glyph; fall back to the key name for spaces.
                if let Some(ch) = ks.key_char.as_deref() {
                    if !ch.is_empty() {
                        self.query.push_str(ch);
                        self.clamp_cursor();
                        cx.notify();
                    }
                } else if ks.key == "space" {
                    self.query.push(' ');
                    self.clamp_cursor();
                    cx.notify();
                }
            }
        }
    }
}

fn host_entry(host: ConfigHost) -> HostEntry {
    let detail = match (host.hostname.as_ref(), host.user.as_ref()) {
        (Some(hn), Some(u)) => Some(format!("{u}@{hn}").into()),
        (Some(hn), None) => Some(hn.clone().into()),
        (None, Some(u)) => Some(format!("{u}@{}", host.alias).into()),
        (None, None) => None,
    };
    HostEntry {
        target: TargetId::Ssh(SshHostId {
            alias: host.alias.clone(),
            user: host.user.clone(),
            port: None,
        }),
        alias: host.alias.into(),
        detail,
    }
}

impl Focusable for HostPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

const OVERLAY_BG: u32 = 0x14110f;
const ROW_BG: u32 = 0x0a0908;
const CURSOR_BG: u32 = 0x2a2621;
const FG: u32 = 0xe8e6e1;
const DIM: u32 = 0x6b645c;
const ACCENT: u32 = 0x8fae7b;

impl Render for HostPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.clamp_cursor();
        let filtered = self.filtered();
        let query: SharedString = if self.query.is_empty() {
            "Type to filter hosts…".into()
        } else {
            self.query.clone().into()
        };
        let query_color = if self.query.is_empty() { DIM } else { FG };

        let rows: Vec<gpui::AnyElement> = filtered
            .iter()
            .enumerate()
            .map(|(row_ix, &entry_ix)| {
                let selected = row_ix == self.cursor;
                let entry = &self.entries[entry_ix];
                div()
                    .id(entry_ix)
                    .w_full()
                    .px_3()
                    .py_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .bg(if selected { rgb(CURSOR_BG) } else { rgb(ROW_BG) })
                    .text_color(if selected { rgb(ACCENT) } else { rgb(FG) })
                    .child(div().flex_1().child(entry.alias.clone()))
                    .when_some(entry.detail.clone(), |this, d| {
                        this.child(div().text_xs().text_color(rgb(DIM)).child(d))
                    })
                    .into_any_element()
            })
            .collect();

        div()
            .track_focus(&self.focus)
            .key_context("Picker")
            .on_key_down(cx.listener(Self::on_key))
            .w(px(360.))
            .max_h(px(460.))
            .bg(rgb(OVERLAY_BG))
            .border_1()
            .border_color(rgb(0x2a2621))
            .rounded_md()
            .shadow_md()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .text_color(rgb(query_color))
                    .border_b_1()
                    .border_color(rgb(0x1a1714))
                    .child(query),
            )
            .child(
                // Long host lists scroll; the query stays pinned above.
                div().id("picker-rows").min_h(px(0.)).flex().flex_col().overflow_y_scroll().children(rows),
            )
    }
}
