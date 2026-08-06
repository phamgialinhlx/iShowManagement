//! The Settings window.
//!
//! A **separate window**, not a panel in the rail. Account management, the app
//! lock and the Claude credential are all things you visit occasionally, decide,
//! and leave — squeezing them into a 240px sidebar next to a live terminal made
//! each one feel like a widget rather than a decision, and the Claude sign-in in
//! particular needs room for a URL and a pasted code.
//!
//! It is created on demand rather than declared in `tauri.conf.json`, because a
//! window declared there is built at startup: the app would pay for a second
//! webview on every launch to show something most sessions never open.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// The window label. The UI reads it to decide which screen to render, so it is
/// part of the contract with `ui/src/main.tsx`, not just a handle.
pub const LABEL: &str = "settings";

/// What the settings window loads. The query parameter is how the UI knows which
/// screen to render — see `ui/src/main.tsx`.
const WINDOW_URL: &str = "index.html?window=settings";

/// Open Settings, or focus it if it is already open.
///
/// Focusing rather than building a second one matters: `WebviewWindowBuilder`
/// fails on a duplicate label, so without this the second click would surface an
/// error instead of the window the operator asked for.
#[tauri::command]
pub async fn open_settings(app: AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window(LABEL) {
        existing.unminimize().ok();
        existing.show().map_err(|e| e.to_string())?;
        existing.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    // The label is put in the query string so the UI can branch on it
    // *synchronously*, before the first render. Asking Tauri for the label is
    // async, and awaiting it would flash the workbench inside this window.
    let builder = WebviewWindowBuilder::new(&app, LABEL, WebviewUrl::App(WINDOW_URL.into()))
        .title("rmux — settings")
        // Sized for the tallest panel — Palette lays out the 16 ANSI wells plus
        // the specials and roles, which overran the old 620px height and clipped
        // its own header and Apply bar off the top and bottom.
        .inner_size(920.0, 800.0)
        .min_inner_size(640.0, 560.0)
        // Matching the main window's chrome, so the two read as one app.
        //
        // `transparent` and the window effect go **together**. On its own,
        // transparency means literally see-through: the workbench behind showed
        // straight through this window's content. The material under the webview
        // is what the design is actually leaning on — the UI then paints its own
        // backdrop layer on top (`.atmosphere`), exactly as the main window does.
        .transparent(true)
        .effects(tauri::utils::config::WindowEffectsConfig {
            effects: vec![tauri::utils::WindowEffect::UnderWindowBackground],
            state: Some(tauri::utils::WindowEffectState::Active),
            radius: None,
            color: None,
        })
        .decorations(true);

    // `title_bar_style` and `hidden_title` are macOS-only on `WebviewWindowBuilder`
    // — not merely no-ops elsewhere, they do not exist, so calling them
    // unconditionally fails to *compile* for Linux and Windows. That is what
    // kept the first CI run from producing anything on either.
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);

    builder
        .build()
        .map_err(|e| format!("could not open settings: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_matches_what_the_ui_switches_on() {
        // `ui/src/main.tsx` renders Settings when the window label is this
        // string. They are two halves of one contract, and a rename on either
        // side alone would show the workbench in the settings window — twice the
        // terminals, and no settings.
        assert_eq!(LABEL, "settings");
        // …and the query parameter the UI actually reads carries that label.
        assert_eq!(WINDOW_URL, format!("index.html?window={LABEL}"));
    }
}

/// Relaunch the app.
///
/// Offered beside Apply rather than required by it. Every appearance change now
/// takes effect live — the `storage` listener in `AppearancePanel` is what
/// carries it between windows — so a restart is a *clean slate*, not a
/// correctness step. It earns its place for one thing: the terminals re-measure
/// their cell size from scratch, which is the tidiest way to settle xterm and
/// its WebGL atlas after an interface-scale change.
///
/// Sessions survive it. Shells and Claude run under `rmux-agent` on the target,
/// so relaunching reattaches rather than restarts the work — which is precisely
/// why offering this is safe at all.
#[tauri::command]
pub fn restart_app<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    app.restart();
}
