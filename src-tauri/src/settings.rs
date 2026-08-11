//! The canonical settings store: `~/.rmux/settings.json`, owned by Rust.
//!
//! This is `theme.rs` generalised from colours to every *portable preference*.
//! The design and the eight decisions behind it are in
//! `references/settings-store/ADR-001-json-settings-store.md`; the short version:
//!
//! ## The file is the source of truth (Zed model)
//!
//! Settings lived in `localStorage`, per-machine and behind a staged Apply bar.
//! The operator asked for a hand-editable config file "like Zed", and a file that
//! is not authoritative reads as broken the first time it is hand-edited and
//! nothing happens. So this module owns `settings.json`, every GUI edit writes
//! through here, and a `notify` watcher makes an *external* hand-edit re-apply in
//! the running app — the UI is a live editor over the file, both ways. Exactly
//! the `theme.toml` mechanism, one file over.
//!
//! ## A single Rust schema is the single source (ADR §7)
//!
//! Every setting is one typed field on [`Settings`] with a real default (the
//! per-struct `Default` impls) and container `#[serde(default)]`, so the on-disk
//! file is *sparse* — it holds only the keys the operator changed, and any absent
//! key resolves to the in-code default when the sparse overrides are deserialised
//! straight into [`Settings`]. That one struct derives the default, validates an
//! incoming patch (a value that will not deserialise into its field's type is
//! refused), and generates the `settings.default.jsonc` reference document.
//!
//! The GUI stays a hand-built React panel, but becomes a thin *writer* over these
//! same keys via [`settings_patch`]. The registry ([`registry`]) carries the
//! human description of each field so the generated defaults doc — and, later, the
//! panel — reads from one list rather than three.
//!
//! ## Overrides only; comments survive
//!
//! A GUI save edits *only the changed key* into the existing file text with a
//! JSONC CST editor (`jsonc-parser`), so the operator's comments, key order and
//! any unknown keys are preserved — a whole-object reserialise would erase them
//! the instant any control was touched. A malformed hand-edit is *not*
//! overwritten: the running app falls back to defaults and the file is left for
//! the operator to fix (a typo costs a reload, not the whole config).

use std::collections::BTreeMap;
use std::path::PathBuf;

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use jsonc_parser::ParseOptions;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

/// Emitted to every window when a setting changes — from an in-app patch or an
/// external hand-edit. The payload is the whole merged [`Settings`], so a
/// listener re-applies without a round trip (mirrors `THEME_CHANGED`).
pub const SETTINGS_CHANGED: &str = "settings-changed";

/* ------------------------------------------------------------------ schema */

/// The root of every portable preference. `#[serde(default)]` at the container
/// level means a missing key deserialises to the value from `Default` — so the
/// sparse on-disk overrides merge over the in-code defaults simply by being
/// deserialised into this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub appearance: Appearance,
    pub terminal: Terminal,
    pub notify: Notify,
    pub editor: Editor,
    /// Action id → key chord (e.g. `"view.claude": "Mod+1"`). The whole map is
    /// patched at once because an action id itself contains dots (`view.claude`),
    /// which a dotted patch path could not address unambiguously.
    pub shortcuts: BTreeMap<String, String>,
    /// Hands-free pane following. A way of working, kept across launches.
    pub hands_free: bool,
    /// Benchmark logging for the diagnostics panel.
    pub debug_logging: bool,
    pub widget_rail: WidgetRail,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            terminal: Terminal::default(),
            notify: Notify::default(),
            editor: Editor::default(),
            shortcuts: default_shortcuts(),
            hands_free: false,
            debug_logging: false,
            widget_rail: WidgetRail::default(),
        }
    }
}

/// How the app looks. Mirrors the UI's `Appearance` type
/// (`ui/src/components/AppearancePanel.tsx`); the two are pinned by a test.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Appearance {
    /// Panel opacity, 0–100. Lower shows more desktop.
    pub tint: u32,
    /// Apple's native Liquid Glass, where the machine has it (macOS 26+). Off by
    /// default even where supported — a silent appearance change on an OS upgrade
    /// is unsettling.
    pub glass: bool,
    /// Apple's thinner glass style: more wallpaper, less contrast under small text.
    pub glass_clear: bool,
    /// Colour laid *over* native glass, 0–50 — legibility residue, a separate
    /// quantity from `tint`.
    pub overlay: u32,
    /// What sits behind the app: `desktop` (translucent over the wallpaper),
    /// `color`, or `image`. A non-desktop background switches glass off by physics.
    pub background: String,
    pub background_color: String,
    /// Absolute path on disk, written by `background_set`. Absent until a picture
    /// is chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_image: Option<String>,
    /// How completely the background covers the desktop, 0–100.
    pub background_cover: u32,
    /// Interface scale, 60–200. Implemented as `zoom`; scales layout with the type.
    pub scale: u32,
    /// Text size percentage, independent of `scale` — moves only type.
    pub font_scale: u32,
    /// UI typeface id (resolved through `lib/fonts.ts`). Drives the chrome.
    pub ui_font: String,
    /// Monospace typeface id. Drives every monospace surface at once.
    pub mono_font: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            tint: 38,
            glass: false,
            glass_clear: false,
            overlay: 14,
            // A first run is opaque, not the desktop: legibility must not depend
            // on a wallpaper rmux did not choose and cannot see.
            background: "color".into(),
            background_color: "#0b0b0d".into(),
            background_image: None,
            background_cover: 100,
            scale: 100,
            font_scale: 100,
            ui_font: DEFAULT_UI_FONT.into(),
            mono_font: DEFAULT_MONO_FONT.into(),
        }
    }
}

/// Kept in step with `ui/src/lib/fonts.ts` `DEFAULT_UI_FONT` / `DEFAULT_MONO_FONT`.
const DEFAULT_UI_FONT: &str = "sfu-futura";
const DEFAULT_MONO_FONT: &str = "ibm-plex-mono";

/// Terminal rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Terminal {
    /// Repaint cap in frames/sec, `0` = uncapped. Cuts compositor load on 4K
    /// displays; off by default (`references/terminal-fps/`).
    pub fps: u32,
    /// WebGL renderer for xterm. On by default; without it scrolling a busy TUI
    /// is visibly slow.
    pub gpu: bool,
}

impl Default for Terminal {
    fn default() -> Self {
        // gpu defaults *on* — the DOM-renderer fallback lags on a busy pane.
        Self { fps: 0, gpu: true }
    }
}

/// Notifications and alerts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Notify {
    /// Whether the session on screen is deliberately silent. Off by default.
    pub quiet_watched: bool,
    /// Whether an *unwatched* session that stops to ask gets a persistent alert
    /// rather than only a banner. On by default — a missed banner is stalled work.
    pub alert: bool,
    /// Whether the alert is accompanied by a sound. On by default.
    pub sound: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self { quiet_watched: false, alert: true, sound: true }
    }
}

/// The code editor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Editor {
    /// Autosave after typing settles. On by default — a first run must autosave.
    pub autosave: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self { autosave: true }
    }
}

/// Which instruments are in the widget rail and in what order.
///
/// Both are `Option` and default to `None` — *unset* means "the full rail, in the
/// declared order", and the UI's reconciliation (a widget added later appears
/// rather than being silently off) needs to tell "unset" from "explicitly empty".
/// The `known`-vocabulary bookkeeping that reconciliation depends on stays in
/// `localStorage` as ephemeral state; only the two portable choices live here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WidgetRail {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

/// The default key chords, mirroring `DEFAULTS` in `ui/src/lib/shortcuts.ts`.
///
/// Platform-split for the same reason the UI splits them: `Mod` is ⌘ on macOS
/// (free in a terminal) and Ctrl elsewhere (which xterm claims), so the two
/// platforms cannot share a chord. `cfg!` here and `isMac()` there agree because
/// it is the same machine. A test pins the id set against the UI's `ACTIONS`.
fn default_shortcuts() -> BTreeMap<String, String> {
    let pairs: &[(&str, &str)] = if cfg!(target_os = "macos") {
        &[
            ("view.claude", "Mod+1"),
            ("view.files", "Mod+2"),
            ("view.transcript", "Mod+3"),
            ("view.jira", "Mod+4"),
            ("view.git", "Mod+5"),
            ("session.terminal", "Mod+T"),
            ("pane.left", "Mod+Shift+ArrowLeft"),
            ("pane.right", "Mod+Shift+ArrowRight"),
            ("pane.up", "Mod+Shift+ArrowUp"),
            ("pane.down", "Mod+Shift+ArrowDown"),
            ("progress", "Mod+P"),
        ]
    } else {
        &[
            ("view.claude", "Mod+Shift+1"),
            ("view.files", "Mod+Shift+2"),
            ("view.transcript", "Mod+Shift+3"),
            ("view.jira", "Mod+Shift+4"),
            ("view.git", "Mod+Shift+5"),
            ("session.terminal", "Mod+Shift+T"),
            ("pane.left", "Mod+Shift+H"),
            ("pane.right", "Mod+Shift+L"),
            ("pane.up", "Mod+Shift+K"),
            ("pane.down", "Mod+Shift+J"),
            ("progress", "Mod+Shift+P"),
        ]
    };
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

/// Every action id, in the order `ui/src/lib/shortcuts.ts` `ACTIONS` lists them.
/// Pinned against `default_shortcuts` by a test so a new action cannot be added
/// on one side only.
const ACTION_IDS: [&str; 11] = [
    "view.claude",
    "view.files",
    "view.transcript",
    "view.jira",
    "view.git",
    "session.terminal",
    "pane.left",
    "pane.right",
    "pane.up",
    "pane.down",
    "progress",
];

/* ---------------------------------------------------------------- registry */

/// One field's human description, for the generated defaults document (and, in a
/// later phase, the Settings panel). Paired with the field's default — pulled
/// from `Settings::default()` — so the *default* has one source (the `Default`
/// impls) and the *description* has one source (this list).
pub struct SettingDoc {
    /// Dotted JSON path, e.g. `appearance.tint`.
    pub path: &'static str,
    /// The one-line description shown in the defaults doc.
    pub doc: &'static str,
}

/// The described settings, in the order the defaults document lists them.
/// `shortcuts`, `widgetRail.*` and `appearance.backgroundImage` are intentionally
/// absent — they are whole-object / optional values documented by their default
/// value alone rather than by a per-key line.
pub fn registry() -> Vec<SettingDoc> {
    vec![
        d("appearance.tint", "Panel opacity, 0–100. Lower shows more of the desktop."),
        d("appearance.glass", "Apple Liquid Glass where supported (macOS 26+). Off by default."),
        d("appearance.glassClear", "Thinner glass: more wallpaper, less contrast under small text."),
        d("appearance.overlay", "Colour laid over native glass, 0–50 (separate from tint)."),
        d("appearance.background", "What sits behind the app: \"desktop\", \"color\" or \"image\"."),
        d("appearance.backgroundColor", "Background colour when background is \"color\"."),
        d("appearance.backgroundCover", "How completely the background covers the desktop, 0–100."),
        d("appearance.scale", "Interface scale, 60–200 (zoom; scales layout with the type)."),
        d("appearance.fontScale", "Text size percentage, independent of scale."),
        d("appearance.uiFont", "UI typeface id (see the font picker)."),
        d("appearance.monoFont", "Monospace typeface id (terminal, Claude, editor)."),
        d("terminal.fps", "Terminal repaint cap in frames/sec. 0 = uncapped."),
        d("terminal.gpu", "WebGL terminal renderer. On by default."),
        d("notify.quietWatched", "Silence notifications for the session on screen."),
        d("notify.alert", "Persistent alert (not just a banner) for an unwatched session."),
        d("notify.sound", "Play a sound with the alert."),
        d("editor.autosave", "Autosave a file after typing settles."),
        d("handsFree", "Follow whichever pane starts working, keyboard and all."),
        d("debugLogging", "Benchmark logging for the diagnostics panel."),
    ]
}

fn d(path: &'static str, doc: &'static str) -> SettingDoc {
    SettingDoc { path, doc }
}

/* ------------------------------------------------------------------ store */

/// Remembers the last bytes we wrote, so the watcher can tell our own save from a
/// hand-edit and not echo it back as a change. (Same device as `ThemeStore`.)
#[derive(Default)]
pub struct SettingsStore {
    last_written: Mutex<Option<String>>,
}

fn config_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home directory".to_string())?;
    Ok(home.join(".rmux").join("settings.json"))
}

/// The banner written above an empty `settings.json`, so a first-time hand-editor
/// knows the file is sparse and where the reference lives.
const HEADER: &str = "// rmux settings — hand-editable. Only the keys you change need to be here;\n// everything else uses its default. See settings.default.jsonc for every option.\n";

/// Read the raw override text. `None` when the file is absent or unreadable.
fn read_text() -> Option<String> {
    let path = config_path().ok()?;
    std::fs::read_to_string(&path).ok()
}

/// Parse the override text to a JSON object, tolerating comments and trailing
/// commas. A malformed file yields `None` (an empty override set with a flag),
/// never a rewrite.
fn parse_overrides(text: &str) -> Option<serde_json::Map<String, Value>> {
    let parsed = jsonc_parser::parse_to_serde_value(text, &ParseOptions::default()).ok()??;
    match parsed {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// Merge the sparse overrides over the in-code defaults by deserialising them
/// straight into `Settings` (container `#[serde(default)]` fills every gap).
/// Returns the merged settings and whether the file was present-but-malformed.
fn load() -> (Settings, bool) {
    let Some(text) = read_text() else {
        return (Settings::default(), false);
    };
    if text.trim().is_empty() {
        return (Settings::default(), false);
    }
    match parse_overrides(&text) {
        Some(map) => match serde_json::from_value::<Settings>(Value::Object(map)) {
            Ok(s) => (s, false),
            Err(e) => {
                tracing::warn!(error = %e, "settings.json is malformed — falling back to defaults");
                (Settings::default(), true)
            }
        },
        None => {
            tracing::warn!("settings.json is not a JSON object — falling back to defaults");
            (Settings::default(), true)
        }
    }
}

/// `serde_json::Value` → the CST editor's input value. There is no `From` for
/// this in the crate, so it is spelled out; numbers keep their exact textual form.
fn to_input(v: &Value) -> CstInputValue {
    match v {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(a) => CstInputValue::Array(a.iter().map(to_input).collect()),
        Value::Object(o) => {
            CstInputValue::Object(o.iter().map(|(k, val)| (k.clone(), to_input(val))).collect())
        }
    }
}

/// Set `path` (dotted) to `value` inside a JSON object, creating intermediate
/// objects as needed. Used to build the *candidate* overrides for validation
/// before the real content-preserving edit runs.
fn set_path(map: &mut serde_json::Map<String, Value>, path: &[&str], value: Value) {
    let (head, rest) = path.split_first().expect("path is never empty");
    if rest.is_empty() {
        map.insert((*head).to_owned(), value);
        return;
    }
    let entry = map.entry((*head).to_owned()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    set_path(entry.as_object_mut().expect("just made it an object"), rest, value);
}

/// Apply a path-scoped edit to the file *text*, preserving comments, key order
/// and unknown keys. Creates intermediate objects; replaces an existing key's
/// value in place. Returns the new file text.
fn patch_text(text: &str, path: &[&str], value: &Value) -> Result<String, String> {
    let seed = if text.trim().is_empty() { format!("{HEADER}{{\n}}\n") } else { text.to_owned() };
    let root = CstRootNode::parse(&seed, &ParseOptions::default())
        .map_err(|e| format!("could not parse settings.json: {e}"))?;
    let mut obj = root.object_value_or_set();
    let (last, parents) = path.split_last().expect("path is never empty");
    for seg in parents {
        obj = obj.object_value_or_set(seg);
    }
    match obj.get(last) {
        Some(prop) => prop.set_value(to_input(value)),
        None => {
            obj.append(last, to_input(value));
        }
    }
    Ok(root.to_string())
}

fn write_text(store: &SettingsStore, text: &str) -> Result<(), String> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    *store.last_written.lock() = Some(text.to_owned());
    std::fs::write(&path, text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

/* ---------------------------------------------------------------- commands */

/// The merged settings the UI reads on startup.
#[tauri::command]
pub fn settings_state() -> Settings {
    load().0
}

/// Apply one path-scoped change: `path` like `"appearance.tint"`, `value` any
/// JSON. Validates that the result still deserialises into [`Settings`] (a value
/// of the wrong type for its field is refused), then edits only that key into the
/// file text and broadcasts. Returns the merged settings.
#[tauri::command]
pub fn settings_patch<R: Runtime>(
    app: AppHandle<R>,
    store: State<'_, SettingsStore>,
    path: String,
    value: Value,
) -> Result<Settings, String> {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err("a setting path is required".to_owned());
    }

    // Validate against the schema on a *candidate* built from the current
    // overrides, before touching the file.
    let text = read_text().unwrap_or_default();
    let mut candidate = if text.trim().is_empty() {
        serde_json::Map::new()
    } else {
        parse_overrides(&text).ok_or_else(|| {
            "settings.json is malformed; fix or delete it before changing a setting here".to_owned()
        })?
    };
    set_path(&mut candidate, &segments, value.clone());
    serde_json::from_value::<Settings>(Value::Object(candidate))
        .map_err(|e| format!("invalid value for {path}: {e}"))?;

    // Content-preserving write.
    let next = patch_text(&text, &segments, &value)?;
    write_text(&store, &next)?;
    Ok(broadcast(&app, load().0))
}

fn broadcast<R: Runtime>(app: &AppHandle<R>, settings: Settings) -> Settings {
    let _ = app.emit(SETTINGS_CHANGED, settings.clone());
    settings
}

// NOTE: the `settings_open` / `settings_open_defaults` commands (the Settings
// panel's "Open settings.json" / "View default settings" buttons) land in
// Phase 4 together with the opener plugin and its capability grant. This module
// already produces the reference text via `generate_defaults_doc`.

/// Build the defaults document: a banner, a commented index of every described
/// key, then the full default object as pretty JSON. Single-source — defaults
/// from `Settings::default()`, descriptions from [`registry`].
pub fn generate_defaults_doc() -> String {
    let defaults = serde_json::to_value(Settings::default()).unwrap_or_else(|_| json!({}));
    let mut out = String::new();
    out.push_str("// rmux — every setting, its default, and a one-line description.\n");
    out.push_str("// This file is generated and read-only. Put your overrides in settings.json;\n");
    out.push_str("// it needs only the keys you change (nest them by the dots below).\n//\n");
    out.push_str("// Options:\n");
    for s in registry() {
        let default = value_at(&defaults, s.path)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "…".to_owned());
        out.push_str(&format!("//   {} = {}  — {}\n", s.path, default, s.doc));
    }
    out.push_str("//   shortcuts = { action-id: \"chord\", … }  — key bindings (platform defaults below)\n");
    out.push_str("//   widgetRail.enabled / widgetRail.order  — which instruments show, and in what order\n\n");
    out.push_str(&serde_json::to_string_pretty(&defaults).unwrap_or_else(|_| "{}".to_owned()));
    out.push('\n');
    out
}

/// Read a dotted path out of a `serde_json::Value`.
fn value_at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = value;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/* ------------------------------------------------------------------ watcher */

/// Watch the config directory so an external hand-edit of `settings.json`
/// re-applies in the running app. Best-effort — a machine where the watch cannot
/// be set up still runs, it just picks up hand-edits only on the next launch.
/// Copied from `theme.rs::start_watcher`, one file over.
pub fn start_watcher<R: Runtime>(app: AppHandle<R>) {
    use notify::{EventKind, RecursiveMode, Watcher};

    let Ok(path) = config_path() else { return };
    let Some(dir) = path.parent().map(PathBuf::from) else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "settings watcher unavailable — hand-edits need a relaunch");
            return;
        }
    };
    // The directory, not the file inode: an editor that saves via temp-file +
    // rename changes the inode, and a watch on the file would go deaf after one
    // such save.
    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        tracing::warn!(error = %e, "could not watch the config dir for settings edits");
        return;
    }

    std::thread::spawn(move || {
        let _watcher = watcher;
        for res in rx {
            let Ok(event) = res else { continue };
            let touches = event.paths.iter().any(|p| p == &path);
            let relevant = matches!(
                event.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Any
            );
            if !touches || !relevant {
                continue;
            }

            // Ignore our own writes: equal bytes to the last we wrote means this
            // is the echo of an in-app patch the caller already broadcast.
            let Ok(current) = std::fs::read_to_string(&path) else { continue };
            {
                let store = app.state::<SettingsStore>();
                let last = store.last_written.lock();
                if last.as_deref() == Some(current.as_str()) {
                    continue;
                }
            }

            // A malformed hand-edit falls back to defaults for the running app
            // and is *not* rewritten (load() handles both).
            let _ = app.emit(SETTINGS_CHANGED, load().0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_rmux_defaults_not_the_type_defaults() {
        let s = Settings::default();
        // Container `#[serde(default)]` must resolve missing keys to *these*
        // values, not `u32::default()` (0) etc.
        assert_eq!(s.appearance.tint, 38);
        assert_eq!(s.appearance.background, "color");
        assert!(s.terminal.gpu);
        assert_eq!(s.terminal.fps, 0);
        assert!(s.notify.alert);
        assert!(s.editor.autosave);
        assert!(!s.hands_free);
    }

    #[test]
    fn sparse_overrides_merge_over_defaults() {
        // A file with a single deep key keeps every other default.
        let overrides = r#"{ "terminal": { "fps": 30 } }"#;
        let map = parse_overrides(overrides).unwrap();
        let s: Settings = serde_json::from_value(Value::Object(map)).unwrap();
        assert_eq!(s.terminal.fps, 30);
        assert!(s.terminal.gpu); // untouched → default on
        assert_eq!(s.appearance.tint, 38); // untouched → default
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_parse() {
        let text = "{\n  // a comment\n  \"handsFree\": true,\n}";
        let s: Settings =
            serde_json::from_value(Value::Object(parse_overrides(text).unwrap())).unwrap();
        assert!(s.hands_free);
    }

    #[test]
    fn a_patch_preserves_a_hand_written_comment() {
        let text = "{\n  // keep me\n  \"handsFree\": true\n}\n";
        let out = patch_text(text, &["terminal", "fps"], &json!(30)).unwrap();
        assert!(out.contains("// keep me"), "comment was dropped:\n{out}");
        assert!(out.contains("\"handsFree\": true"));
        // The new key landed and the merged view sees it.
        let s: Settings =
            serde_json::from_value(Value::Object(parse_overrides(&out).unwrap())).unwrap();
        assert_eq!(s.terminal.fps, 30);
        assert!(s.hands_free);
    }

    #[test]
    fn a_patch_replaces_an_existing_key_in_place() {
        let text = "{\n  \"terminal\": { \"fps\": 15, \"gpu\": true }\n}\n";
        let out = patch_text(text, &["terminal", "fps"], &json!(60)).unwrap();
        let s: Settings =
            serde_json::from_value(Value::Object(parse_overrides(&out).unwrap())).unwrap();
        assert_eq!(s.terminal.fps, 60);
        assert!(s.terminal.gpu); // sibling key untouched
    }

    #[test]
    fn a_patch_into_an_empty_file_seeds_a_valid_object() {
        let out = patch_text("", &["appearance", "scale"], &json!(120)).unwrap();
        let s: Settings =
            serde_json::from_value(Value::Object(parse_overrides(&out).unwrap())).unwrap();
        assert_eq!(s.appearance.scale, 120);
    }

    #[test]
    fn a_malformed_file_yields_defaults_and_a_flag() {
        // Not an object → malformed.
        assert!(parse_overrides("42").is_none());
        // Wrong type for a field → deserialise fails, load() would flag it.
        let bad = r#"{ "terminal": { "fps": "lots" } }"#;
        let map = parse_overrides(bad).unwrap();
        assert!(serde_json::from_value::<Settings>(Value::Object(map)).is_err());
    }

    #[test]
    fn shortcut_defaults_cover_every_action_and_nothing_else() {
        let s = default_shortcuts();
        assert_eq!(s.len(), ACTION_IDS.len());
        for id in ACTION_IDS {
            assert!(s.contains_key(id), "shortcut default missing for {id}");
        }
    }

    #[test]
    fn every_registry_path_resolves_in_the_default_object() {
        let defaults = serde_json::to_value(Settings::default()).unwrap();
        for s in registry() {
            assert!(value_at(&defaults, s.path).is_some(), "registry path absent: {}", s.path);
        }
    }

    #[test]
    fn the_defaults_doc_names_every_registered_key() {
        let doc = generate_defaults_doc();
        for s in registry() {
            assert!(doc.contains(s.path), "defaults doc missing {}", s.path);
        }
        assert!(doc.contains("\"tint\": 38"));
    }

    #[test]
    fn set_path_creates_intermediate_objects() {
        let mut map = serde_json::Map::new();
        set_path(&mut map, &["notify", "sound"], json!(false));
        let s: Settings = serde_json::from_value(Value::Object(map)).unwrap();
        assert!(!s.notify.sound);
        assert!(s.notify.alert); // sibling default preserved
    }
}
