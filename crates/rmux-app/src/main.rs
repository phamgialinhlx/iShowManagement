//! rmux — native gpui shell (scaffold).
//!
//! Current milestone: one native terminal tab running a local login shell,
//! emulated by `alacritty_terminal` and painted by gpui. The workspace shell
//! (panes/docks/tabs) grows around this.

mod terminal;

use gpui::{App, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_platform::application;

use crate::terminal::TerminalView;

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(600.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TerminalView::new(window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
