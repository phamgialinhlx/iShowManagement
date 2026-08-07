//! Apple's Liquid Glass, natively, when the machine has it.
//!
//! ## Why this cannot be done in CSS
//!
//! The last three attempts at glass were CSS, and all three failed for one
//! reason that is worth writing down because it is not obvious and it reads
//! backwards: **`backdrop-filter` filters the page's own backdrop, and the
//! desktop is not page content.**
//!
//! rmux's window is `transparent: true` over a native material. What sits behind
//! the webview is a macOS view, composited by the window server *below* the
//! webview's own layer — so a filter running inside the page has nothing to
//! sample but an empty transparent document, which WebKit resolves to an opaque
//! dark field. That is why every CSS glass produced a *more* solid panel rather
//! than a translucent one, and why refraction — the part that actually reads as
//! glass — is unreachable from the page at any level of effort. You cannot bend
//! light you were never handed.
//!
//! The compositor, on the other hand, has the wallpaper already.
//! `NSGlassEffectView` (macOS 26) is that same compositor doing real refraction,
//! specular edges and tinting, for the cost of one view.
//!
//! ## It is one sheet, not sixteen panels
//!
//! Glass is an `NSView`; every rmux panel is HTML inside a single webview. So
//! the window gets *one* glass surface, behind everything, and the UI floats on
//! it. Per-panel glass would mean one native view per panel positioned against
//! DOM geometry — sixteen rectangles to keep in sync in a 4x4 grid, and wrong
//! for one frame on every resize. rmux already gave up per-panel frost when
//! `backdrop-filter` came out; this changes nothing and looks considerably
//! better.
//!
//! ## The class is looked up, never linked
//!
//! `NSGlassEffectView` is `API_AVAILABLE(macos(26.0))`. `objc2` resolves every
//! class through `objc_getClass` at runtime and **panics** (`class_not_present`)
//! when one is absent — so `NSGlassEffectView::class()` would take the app down
//! on macOS 15 at the moment glass was applied. Measured on the shipped binary:
//! zero undefined `OBJC_CLASS` symbols, so this is a panic rather than the dyld
//! load failure it first looked like. The distinction does not change the fix:
//! the class is resolved by name with `AnyClass::get`, whose `None` is the
//! entire version check and the difference between a fallback and a crash. The
//! typed bindings are still used for the instance methods, which send messages
//! to the object and never name the class.
//!
//! Everything else — Windows, Linux, and any Mac before 26 — keeps the
//! `underWindowBackground` vibrancy configured in `tauri.conf.json`, untouched.

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_app_kit::{
        NSColor, NSGlassEffectView, NSGlassEffectViewStyle, NSView, NSVisualEffectView,
        NSWindow, NSWindowOrderingMode,
    };
    use objc2_foundation::{NSObjectProtocol, NSRect};

    use super::GlassOptions;

    /// A colour the operator chose, as `#rrggbb`, plus an opacity.
    ///
    /// Parsed rather than passed through: this ends up in `setTintColor:`, and
    /// AppKit's string-based colour lookups are not a thing we want to feed
    /// arbitrary UI input into.
    fn tint(hex: &str, alpha: f64) -> Option<Retained<NSColor>> {
        let hex = hex.trim().trim_start_matches('#');
        if hex.len() != 6 {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|v| v as f64 / 255.0);
        let (r, g, b) = (channel(0)?, channel(2)?, channel(4)?);
        // sRGB explicitly. The generic constructor is device-dependent, and a
        // tint that shifts with the display profile is a support ticket.
        Some(NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, alpha.clamp(0.0, 1.0)))
    }

    /// The window's content view, which owns both the webview and whatever
    /// material sits under it.
    ///
    /// # Safety
    /// `ns_window` must be the `NSWindow` pointer Tauri handed us, and this must
    /// be the main thread.
    unsafe fn content_view(ns_window: *mut std::ffi::c_void) -> Option<Retained<NSView>> {
        if ns_window.is_null() {
            return None;
        }
        let window: &NSWindow = unsafe { &*ns_window.cast() };
        window.contentView()
    }

    /// Is this machine new enough to have Liquid Glass at all?
    pub fn available() -> bool {
        AnyClass::get(c"NSGlassEffectView").is_some()
    }

    /// Find the glass we installed, if it is installed.
    unsafe fn existing(content: &NSView) -> Option<Retained<NSGlassEffectView>> {
        let class = AnyClass::get(c"NSGlassEffectView")?;
        content
            .subviews()
            .iter()
            .find(|view| view.isKindOfClass(class))
            .map(|view| unsafe { Retained::cast_unchecked::<NSGlassEffectView>(view) })
    }

    /// Apply the operator's choice. Returns whether native glass is now on.
    ///
    /// # Safety
    /// Must run on the main thread.
    pub unsafe fn apply(ns_window: *mut std::ffi::c_void, options: &GlassOptions) -> bool {
        let Some(content) = (unsafe { content_view(ns_window) }) else {
            return false;
        };

        if !options.enabled {
            if let Some(glass) = unsafe { existing(&content) } {
                glass.removeFromSuperview();
            }
            // The vibrancy Tauri installed is left alone, so turning glass off
            // lands back on exactly the material the app shipped with rather
            // than on a bare transparent window.
            return false;
        }

        let Some(class) = AnyClass::get(c"NSGlassEffectView") else {
            return false;
        };

        let glass = match unsafe { existing(&content) } {
            Some(glass) => glass,
            None => {
                let frame: NSRect = content.bounds();
                // `alloc` on the looked-up class, not on the Rust type: see the
                // module note. The cast is sound because the object *is* an
                // NSGlassEffectView — it came from that exact class, which we
                // asked for by name rather than by a reference that panics when
                // the OS is too old to have it.
                let allocated: Allocated<AnyObject> = unsafe { msg_send![class, alloc] };
                let object: Retained<AnyObject> =
                    unsafe { msg_send![allocated, initWithFrame: frame] };
                let glass = unsafe { Retained::cast_unchecked::<NSGlassEffectView>(object) };

                {
                    // Follows the window. Autoresizing rather than constraints
                    // because the content view Tauri hands us is not laid out
                    // with Auto Layout, and mixing the two is how you get a
                    // view that is correct until the first resize.
                    glass.setAutoresizingMask(
                        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
                    );
                    // Beneath everything, including the webview — which is
                    // transparent, so this is what shows through it.
                    content.addSubview_positioned_relativeTo(
                        &glass,
                        NSWindowOrderingMode::Below,
                        None,
                    );
                }
                glass
            }
        };

        unsafe {
            // Rule 1 of the design system, and it survives the platform change:
            // zero radius. The window's own rounding is the window's business.
            glass.setCornerRadius(0.0);
            glass.setStyle(if options.clear {
                NSGlassEffectViewStyle::Clear
            } else {
                NSGlassEffectViewStyle::Regular
            });
            glass.setTintColor(
                options.tint.as_deref().and_then(|hex| tint(hex, options.tint_opacity)).as_deref(),
            );

            // The vibrancy is hidden rather than removed. Two stacked materials
            // both sampling behind the window is twice the frosting and reads
            // as a solid panel — the exact failure CSS glass had. Hiding keeps
            // Tauri's view intact so disabling glass restores it for free.
            if let Some(class) = AnyClass::get(c"NSVisualEffectView") {
                for view in content.subviews().iter() {
                    if view.isKindOfClass(class) {
                        Retained::cast_unchecked::<NSVisualEffectView>(view).setHidden(true);
                    }
                }
            }
        }

        true
    }

    /// Put the vibrancy back when glass is switched off.
    ///
    /// # Safety
    /// Must run on the main thread.
    pub unsafe fn restore_vibrancy(ns_window: *mut std::ffi::c_void) {
        let Some(content) = (unsafe { content_view(ns_window) }) else {
            return;
        };
        let Some(class) = AnyClass::get(c"NSVisualEffectView") else {
            return;
        };
        unsafe {
            for view in content.subviews().iter() {
                if view.isKindOfClass(class) {
                    Retained::cast_unchecked::<NSVisualEffectView>(view).setHidden(false);
                }
            }
        }
    }
}

use serde::{Deserialize, Serialize};

/// What the operator asked for, from Settings › Appearance.
///
/// **The fields are the IPC contract, not dead weight** — they have to exist for
/// the payload to deserialise on every platform, and only the macOS
/// implementation below reads them. So off macOS the compiler is right that
/// nothing reads them and wrong that they can go: dropping one would reject the
/// message the UI sends. Silenced narrowly, and only where it applies, rather
/// than by weakening the lint or adding a fake reader.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlassOptions {
    pub enabled: bool,
    /// `Clear` is the thinner, more transparent of Apple's two styles; it is
    /// what makes the wallpaper legible through the window. `Regular` frosts
    /// more and is easier to read text on.
    #[serde(default)]
    pub clear: bool,
    /// `#rrggbb`, or absent for untinted glass.
    #[serde(default)]
    pub tint: Option<String>,
    #[serde(default)]
    pub tint_opacity: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlassStatus {
    /// The machine has `NSGlassEffectView` — macOS 26 or later.
    pub available: bool,
    /// It is installed right now.
    pub active: bool,
}

/// Whether this build, on this machine, can do native glass.
///
/// The UI asks before offering the control: a toggle that silently does nothing
/// is worse than no toggle, and on Windows, Linux and older Macs this genuinely
/// cannot work.
#[tauri::command]
pub fn glass_status() -> GlassStatus {
    #[cfg(target_os = "macos")]
    {
        GlassStatus { available: imp::available(), active: ACTIVE.load(std::sync::atomic::Ordering::Relaxed) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        GlassStatus::default()
    }
}

#[cfg(target_os = "macos")]
static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Apply a glass setting to **every** window the app has open.
///
/// Not just the calling one, and that is the whole point: Settings is its own
/// window (`settings_window.rs`), so a command scoped to the caller would glass
/// the settings panel and leave the workbench — the window the operator is
/// actually looking at — untouched. The setting is about the app's appearance,
/// so it is applied app-wide.
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn set_glass<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    options: GlassOptions,
) -> Result<GlassStatus, String> {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use tauri::Manager;

    let windows: Vec<_> = app.webview_windows().into_values().collect();
    let mut active = false;

    for window in windows {
        let Ok(ns_window) = window.ns_window() else { continue };
        let ns_window = ns_window as usize;
        let options = options.clone();
        let (tx, rx) = mpsc::channel();

        // AppKit is main-thread-only, and a view mutated from a command
        // handler's thread is a crash that reproduces once a week rather than
        // immediately.
        if window
            .run_on_main_thread(move || {
                let applied = unsafe { imp::apply(ns_window as *mut _, &options) };
                if !applied {
                    unsafe { imp::restore_vibrancy(ns_window as *mut _) };
                }
                let _ = tx.send(applied);
            })
            .is_err()
        {
            // A window closing mid-call is ordinary, not a failure of the
            // setting — the rest still get it.
            continue;
        }

        active |= rx.recv().unwrap_or(false);
    }

    ACTIVE.store(active, Ordering::Relaxed);
    Ok(GlassStatus { available: imp::available(), active })
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn set_glass(_options: GlassOptions) -> Result<GlassStatus, String> {
    // Not an error. The operator asked for glass on a platform that has none,
    // and the honest answer is "unavailable", which is what the status says.
    Ok(GlassStatus::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platform_without_glass_reports_it_rather_than_failing() {
        // The UI branches on `available`, so this must be a truthful answer on
        // every platform and never an error the operator has to dismiss.
        let status = glass_status();
        if !cfg!(target_os = "macos") {
            assert!(!status.available);
        }
        assert!(!status.available || cfg!(target_os = "macos"));
    }

    #[test]
    fn options_default_to_the_untinted_frosted_style() {
        // `enabled` is the only field the UI must send. Everything else has to
        // survive being absent, because the setting is persisted in
        // localStorage and an older saved shape must still load.
        let options: GlassOptions = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(options.enabled);
        assert!(!options.clear);
        assert_eq!(options.tint, None);
    }
}
