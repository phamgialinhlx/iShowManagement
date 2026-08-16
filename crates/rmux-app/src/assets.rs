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

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let svg = match path {
            "icons/sidebar_left.svg" => SIDEBAR_LEFT_SVG,
            "icons/sidebar_left_closed.svg" => SIDEBAR_LEFT_CLOSED_SVG,
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
