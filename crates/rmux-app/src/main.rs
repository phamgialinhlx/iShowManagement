//! rmux — native gpui shell (scaffold).
//!
//! This is the vendoring-validation entry point: it proves the copied Zed
//! presentation stack builds and runs from this workspace. The real workspace
//! shell (panes/docks/tabs) replaces `Scaffold` from here.

use gpui::{
    App, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

struct Scaffold;

impl Render for Scaffold {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .bg(rgb(0x14110f))
            .justify_center()
            .items_center()
            .text_color(rgb(0xe8e6e1))
            .child("rmux — gpui vendoring works")
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(720.), px(480.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Scaffold),
        )
        .unwrap();
        cx.activate(true);
    });
}
