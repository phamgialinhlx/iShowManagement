//! zmux — native gpui shell (scaffold).
//!
//! Current milestone: one native terminal tab running a local login shell,
//! emulated by `alacritty_terminal` and painted by gpui. The workspace shell
//! (panes/docks/tabs) grows around this.

mod assets;
mod backend;
mod pane;
mod pane_group;
mod picker;
mod rail;
mod state;
mod terminal;
mod theme;
mod workspace;

use gpui::{App, Bounds, KeyBinding, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_platform::application;

use crate::backend::Backend;
use crate::workspace::{
    ClosePane, OpenHostPicker, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleRail, Workspace,
};

/// Minimal stderr logger so gpui's own warnings/errors surface. Without an
/// installed logger these are silently dropped — which once hid the fact that a
/// missing `font-kit` feature made gpui fall back to a no-op text system (empty
/// stderr proved nothing). Warn-level keeps it quiet in normal use.
struct StderrLogger;
impl log::Log for StderrLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("[{}] {}: {}", record.level(), record.target(), record.args());
    }
    fn flush(&self) {}
}
static LOGGER: StderrLogger = StderrLogger;

fn main() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
    application()
        .with_assets(crate::assets::Assets)
        .run(|cx: &mut App| {
        // gpui ships the font *names* (".ZedMono" → "Lilex") but not the font
        // *data*, so text renders nothing until the actual files are registered.
        // Lilex is bundled (OFL) and embedded into the binary here.
        let fonts = vec![
            std::borrow::Cow::Borrowed(
                include_bytes!("../assets/fonts/Lilex-Regular.ttf").as_slice(),
            ),
            std::borrow::Cow::Borrowed(
                include_bytes!("../assets/fonts/Lilex-Bold.ttf").as_slice(),
            ),
        ];
        cx.text_system().add_fonts(fonts).expect("register Lilex");

        // Theme system: sets up the GlobalTheme with the Signal Room default.
        // All UI colours read from cx.theme().colors().* after this.
        crate::theme::init_theme(cx);

        // Backend service: tokio runtime + target/agent cache, global for the
        // process lifetime.
        cx.set_global(Backend::new().expect("build backend"));

        cx.bind_keys([
            KeyBinding::new("cmd-left", SplitLeft, Some("Workspace")),
            KeyBinding::new("cmd-right", SplitRight, Some("Workspace")),
            KeyBinding::new("cmd-up", SplitUp, Some("Workspace")),
            KeyBinding::new("cmd-down", SplitDown, Some("Workspace")),
            KeyBinding::new("cmd-w", ClosePane, Some("Workspace")),
            KeyBinding::new("cmd-b", ToggleRail, Some("Workspace")),
            KeyBinding::new("cmd-shift-o", OpenHostPicker, Some("Workspace")),
        ]);

        let bounds = Bounds::centered(None, size(px(900.), px(600.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("zmux".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Workspace::new(window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
