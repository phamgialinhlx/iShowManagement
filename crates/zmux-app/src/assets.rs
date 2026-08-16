//! Minimal asset source for embedded SVG icons. gpui's `svg()` element calls
//! `AssetSource::load("icons/foo.svg")` to resolve paths; this serves a small
//! set of hand-written SVGs from `const` strings — no filesystem, no external
//! files. Add new icons by adding a match arm + a `const` SVG string.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// Zed's `threads_sidebar_left_open` icon — the sidebar panel is filled
/// (opacity 0.8). Used when the rail is visible. gpui renders SVGs as alpha
/// masks, so only the alpha channel matters; `text_color` provides the color.
const SIDEBAR_LEFT_SVG: &str = r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<rect opacity="0.8" width="5" height="12" rx="2" transform="matrix(-1 0 0 1 7 2)" fill="#fff"/>
<path d="M7 2V14" stroke="#fff" stroke-width="1.2"/>
<rect x="2" y="2" width="12" height="12" rx="1.5" stroke="#fff" stroke-width="1.2"/>
</svg>"##;

/// Zed's `threads_sidebar_left_closed` icon — the sidebar panel is faded
/// (opacity 0.1). Used when the rail is collapsed.
const SIDEBAR_LEFT_CLOSED_SVG: &str = r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<rect opacity="0.1" width="5" height="12" rx="2" transform="matrix(-1 0 0 1 7 2)" fill="#fff"/>
<path d="M7 2V14" stroke="#fff" stroke-width="1.2"/>
<rect x="2" y="2" width="12" height="12" rx="1.5" stroke="#fff" stroke-width="1.2"/>
</svg>"##;

/// Zed's `server` icon — used by the topbar "Connect to Server" trigger.
const SERVER_SVG: &str = r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<path d="M12.8 9H3.2C2.53726 9 2 9.44772 2 10V12C2 12.5523 2.53726 13 3.2 13H12.8C13.4627 13 14 12.5523 14 12V10C14 9.44772 13.4627 9 12.8 9Z" stroke="black" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
<path d="M12.8 3H3.2C2.53726 3 2 3.44772 2 4V6C2 6.55228 2.53726 7 3.2 7H12.8C13.4627 7 14 6.55228 14 6V4C14 3.44772 13.4627 3 12.8 3Z" stroke="black" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
<path d="M4 11H4.00667" stroke="black" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
<path d="M4 5H4.00667" stroke="black" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
</svg>"##;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let svg = match path {
            "icons/sidebar_left.svg" => SIDEBAR_LEFT_SVG,
            "icons/sidebar_left_closed.svg" => SIDEBAR_LEFT_CLOSED_SVG,
            "icons/server.svg" => SERVER_SVG,
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
