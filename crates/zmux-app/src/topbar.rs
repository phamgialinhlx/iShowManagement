//! The topbar: a thin strip above the pane area holding the "Connect to
//! Server" trigger. Mirrors Zed's titlebar `render_remote_project_connection`
//! (a `Button` wrapped in a `ui::PopoverMenu`) but simplified — zmux has no
//! single "current connection" to headline, so the trigger is a fixed action
//! rather than a status display.
//!
//! Unlike the bottom bar (hand-rolled `div`/`svg`), this uses the vendored
//! `ui` crate for the trigger + popover, since replicating Zed's popover
//! behavior (anchoring, click-outside-to-dismiss, focus restore) by hand
//! would just reimplement what `ui::PopoverMenu` already does.

use gpui::{AnyElement, Context, div, prelude::*, px};
use theme::ActiveTheme;
use ui::{Button, Icon, IconName, IconSize, LabelSize, PopoverMenu};

use crate::workspace::Workspace;

/// Render the topbar as a child of `Workspace::render`. Takes `cx: &mut
/// Context<Workspace>` (not a bare `&App`) because the popover's `.menu(...)`
/// closure needs to call back into `Workspace::open_host_picker_for_popover`.
/// Returns `AnyElement` rather than `impl IntoElement`: the latter's Rust
/// 2024 auto-captured lifetime ties the returned element to `cx`'s borrow,
/// which conflicts with `Workspace::render`'s later `cx.listener(...)` calls
/// on the same borrow.
pub fn render_topbar(cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors();
    let bar_bg = colors.status_bar_background;
    let bar_border = colors.border_variant;
    let workspace = cx.weak_entity();

    div()
        .flex_shrink_0()
        .h(px(32.))
        .flex()
        .flex_row()
        .items_center()
        .px_2()
        .bg(bar_bg)
        .border_b_1()
        .border_color(bar_border)
        .child(
            PopoverMenu::new("connect-server-menu")
                .menu(move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.open_host_picker_for_popover(window, cx)
                        })
                        .ok()
                })
                .trigger(
                    Button::new("connect-server-trigger", "Connect to Server")
                        .start_icon(Icon::new(IconName::Server).size(IconSize::Small))
                        .label_size(LabelSize::Small),
                )
                .anchor(gpui::Anchor::TopLeft),
        )
        .into_any_element()
}
