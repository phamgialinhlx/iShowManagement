//! A second (third, …) workbench window.
//!
//! rmux is one app the operator wants on several desktops at once — a window of
//! sessions per Space, switched between with the OS. Each extra window is a full
//! workbench with its **own** layout: the layout keys in `localStorage` are
//! namespaced by the window's label (see `ui/src/lib/window-scope.ts`), so two
//! windows do not mirror and clobber each other through the shared origin.
//!
//! Built on demand, never declared in `tauri.conf.json` — the same reasoning as
//! the settings window: a declared window is built at launch, and most sessions
//! only ever want the one.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Open a fresh workbench window and return its label.
///
/// The label is `main-2`, `main-3`, … — the first that is free, so closing the
/// middle one and opening another reuses the gap rather than counting for ever.
/// It is put in the query string (`?window=<label>`) because the UI needs it
/// **synchronously**, before the first render, to pick its `localStorage`
/// namespace; asking Tauri for the label is async and would flash the primary
/// window's layout first. The label pattern is granted its window ACL by the
/// `main-*` glob in `capabilities/default.json`, or the custom title bar's drag
/// and close controls would be rejected silently.
#[tauri::command]
pub async fn open_window(app: AppHandle) -> Result<String, String> {
    let label = (2..)
        .map(|n| format!("main-{n}"))
        .find(|l| app.get_webview_window(l).is_none())
        .expect("a free window label exists");

    let url = format!("index.html?window={label}");
    let builder = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title("rmux")
        // The main window's size and floor, so a new one is instantly the same
        // instrument rather than an odd little panel.
        .inner_size(1440.0, 900.0)
        .min_inner_size(900.0, 600.0)
        // Chrome identical to the main window (`tauri.conf.json`) — transparent
        // plus the under-window material, with the UI painting `.atmosphere` on
        // top. Without the material, `transparent` is literally see-through.
        .transparent(true)
        .effects(tauri::utils::config::WindowEffectsConfig {
            effects: vec![tauri::utils::WindowEffect::UnderWindowBackground],
            state: Some(tauri::utils::WindowEffectState::Active),
            radius: None,
            color: None,
            // Added by tauri 2.12. `false` is the behaviour every build
            // before it had: the vibrancy view sits *behind* the webview and
            // must not take mouse events, or it swallows clicks meant for the
            // page. The glass rules already depend on it being inert.
            interactive: false,
        })
        .decorations(true);

    // macOS-only builder methods — calling them on Linux/Windows does not
    // compile (they do not exist there), which is what broke the first CI run
    // for the settings window.
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);

    builder.build().map_err(|e| format!("could not open a new window: {e}"))?;

    Ok(label)
}

#[cfg(test)]
mod tests {
    #[test]
    fn labels_are_the_scope_the_ui_reads() {
        // The label goes into `?window=<label>`, and `window-scope.ts` treats
        // any `window` value other than `settings` as a layout namespace. So a
        // new-window label must never be `settings` (that renders the settings
        // screen) or empty (that is the primary window's unscoped keys).
        for n in 2..12 {
            let label = format!("main-{n}");
            assert_ne!(label, "settings");
            assert!(!label.is_empty());
            // And it matches the `main-*` ACL glob in capabilities/default.json.
            assert!(label.starts_with("main-"));
        }
    }
}
