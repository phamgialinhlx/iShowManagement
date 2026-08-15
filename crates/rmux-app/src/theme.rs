//! The theme system: wires Zed's vendored `theme` crate and defines the
//! "Signal Room" default theme that reproduces zmux's exact current colours.
//!
//! All UI code reads colours via `cx.theme().colors().FIELD` (the `ActiveTheme`
//! trait). This module sets up the `GlobalTheme` global during app init; future
//! work can swap the active theme at runtime (e.g. from `~/.rmux/settings.json`).

use std::sync::Arc;

use gpui::{App, Font, Pixels, WindowBackgroundAppearance, font, px, rgb};
use theme::{
    AccentColors, Appearance, GlobalTheme, LoadThemes, PlayerColors, StatusColors, SyntaxTheme,
    SystemColors, Theme, ThemeColors, ThemeSettingsProvider, ThemeStyles, UiDensity,
};

/// Hardcoded font/density settings. The `theme` crate requires a
/// `ThemeSettingsProvider` to be registered or `theme_settings(cx)` panics.
/// `Font` contains `SharedString` (Arc-based), so it lives in the struct, not a
/// `static`.
struct SignalRoomSettings {
    font: Font,
}

impl SignalRoomSettings {
    fn new() -> Self {
        Self { font: font("Lilex") }
    }
}

impl ThemeSettingsProvider for SignalRoomSettings {
    fn ui_font<'a>(&'a self, _cx: &'a App) -> &'a Font {
        &self.font
    }
    fn buffer_font<'a>(&'a self, _cx: &'a App) -> &'a Font {
        &self.font
    }
    fn ui_font_size(&self, _cx: &App) -> Pixels {
        px(13.)
    }
    fn buffer_font_size(&self, _cx: &App) -> Pixels {
        px(13.)
    }
    fn ui_density(&self, _cx: &App) -> UiDensity {
        UiDensity::Default
    }
}

/// The Signal Room dark theme — reproduces every colour zmux uses today.
/// Built by overriding the ~30 fields zmux reads on top of `ThemeColors::dark()`
/// (which provides sensible defaults for the ~70 unused editor/scrollbar/vim
/// fields).
fn signal_room_theme() -> Theme {
    // Hex → Hsla helper (rgb returns Rgba, .into() converts).
    let h = |hex: u32| -> gpui::Hsla { rgb(hex).into() };

    let colors = ThemeColors {
        // ── Backgrounds ──
        background: h(0x14110f),
        panel_background: h(0x0a0908),
        surface_background: h(0x0a0908),
        elevated_surface_background: h(0x14110f),
        tab_bar_background: h(0x0a0908),
        tab_active_background: h(0x14110f),
        tab_inactive_background: h(0x0a0908),
        status_bar_background: h(0x0a0908),
        title_bar_background: h(0x14110f),
        toolbar_background: h(0x14110f),

        // ── Text ramp ──
        text: h(0xe8e6e1),
        text_muted: h(0xcfc9c0),
        text_placeholder: h(0x8a827a),
        text_disabled: h(0x6b645c),
        text_accent: h(0x8fae7b),

        // ── Borders / elements ──
        border: h(0x2a2621),
        border_variant: h(0x1a1714),
        pane_focused_border: h(0x5c5346),
        pane_group_border: h(0x2a2621),
        element_active: h(0x2a2621),

        // ── Terminal ──
        terminal_background: h(0x14110f),
        terminal_foreground: h(0xe8e6e1),
        terminal_bright_foreground: h(0xe8e6e1),
        terminal_ansi_background: h(0x14110f),
        terminal_ansi_black: h(0x2a2621),
        terminal_ansi_red: h(0xd77b6b),
        terminal_ansi_green: h(0x8fae7b),
        terminal_ansi_yellow: h(0xd9b06a),
        terminal_ansi_blue: h(0x6f9bd8),
        terminal_ansi_magenta: h(0xb58bd0),
        terminal_ansi_cyan: h(0x76b8b0),
        terminal_ansi_white: h(0xcfc9c0),
        terminal_ansi_bright_black: h(0x6b645c),
        terminal_ansi_bright_red: h(0xe8907f),
        terminal_ansi_bright_green: h(0xa6c48c),
        terminal_ansi_bright_yellow: h(0xe8c67d),
        terminal_ansi_bright_blue: h(0x88b0e8),
        terminal_ansi_bright_magenta: h(0xcaa0e0),
        terminal_ansi_bright_cyan: h(0x8fd0c8),
        terminal_ansi_bright_white: h(0xf0ece4),

        // Remaining fields inherit from ThemeColors::dark()
        ..ThemeColors::dark()
    };

    let status = StatusColors {
        info: h(0x88b0e8),
        warning: h(0xd9b06a),
        success: h(0x8fae7b),
        ..StatusColors::dark()
    };

    Theme {
        id: "signal_room".to_string(),
        name: "Signal Room".into(),
        appearance: Appearance::Dark,
        styles: ThemeStyles {
            window_background_appearance: WindowBackgroundAppearance::Opaque,
            system: SystemColors::default(),
            accents: AccentColors::dark(),
            colors,
            status,
            player: PlayerColors::dark(),
            syntax: Arc::new(SyntaxTheme::new(vec![])),
        },
    }
}

/// Register the settings provider, initialise the theme system, and set Signal
/// Room as the active theme. Call once during app startup, after fonts are
/// registered.
pub fn init_theme(cx: &mut App) {
    // Register before theme::init in case anything queries fonts during init.
    theme::set_theme_settings_provider(Box::new(SignalRoomSettings::new()), cx);
    // Sets up SystemAppearance, ThemeRegistry (loads "One Dark" fallback),
    // FontFamilyCache, GlobalTheme.
    theme::init(LoadThemes::JustBase, cx);
    // Replace the fallback with our custom theme.
    GlobalTheme::update_theme(cx, Arc::new(signal_room_theme()));
}
