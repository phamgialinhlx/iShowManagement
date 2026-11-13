//! The canonical theme store: `theme.toml`, owned by Rust.
//!
//! ## The file is the source of truth
//!
//! Appearance colour lived in `localStorage` and was applied across windows with
//! the `storage` event. Themes do not: the operator asked for a config file, and
//! a file that is not authoritative reads as broken the first time it is
//! hand-edited and nothing happens. So this module owns `theme.toml`, every edit
//! writes through here, and a `notify` watcher makes an *external* hand-edit
//! repaint the running app — the UI is a live editor over the file, both ways.
//!
//! ## Rust stores only user themes and which one is active
//!
//! The four built-ins (SIGNAL ROOM, Nord, …) are defined once, in the UI
//! (`ui/src/lib/theme.ts`), because that is also where the *derivation* that
//! consumes them lives. Duplicating 88 colour values here to serve them back
//! would be two lists to keep in step. Instead the file holds `active` plus any
//! themes the operator saved, and the UI merges its code-defined built-ins over
//! the top. A missing or corrupt file therefore is not an error — it simply means
//! "no user themes, active = SIGNAL ROOM", and the built-ins reappear from code.
//!
//! Only the built-in *names* are known here, and only to refuse overwriting one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

/// Emitted to every window when the active theme or the theme set changes —
/// from an in-app edit or an external hand-edit. The payload is the whole state,
/// so a listener re-applies without a round trip.
pub const THEME_CHANGED: &str = "theme-changed";

/// The 22 colour values of one theme. No `name` — that is the map key on disk and
/// the `name` field once flattened for the UI.
///
/// `camelCase` so the field names match the TS `Theme` type and read the same in
/// the TOML file (`brightBlack`, `boldText`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
    pub background: String,
    pub foreground: String,
    pub bold_text: String,
    pub selection: String,
    pub cursor: String,
    pub accent: String,
    pub working: String,

    // -- optional, added with the herdr themes -------------------------------
    //
    // Every one of these is `Option` + `default` so a `theme.toml` written by an
    // older build — which has none of them — still parses, and the UI derives
    // whatever is missing. `skip_serializing_if` keeps them out of the file
    // until a theme actually carries them, so an old theme round-trips byte-
    // for-byte rather than gaining empty keys on its next save.
    /// herdr's UI palette slots (`panelBg`, `surface0`, …). A map rather than
    /// nineteen fields: the UI owns the slot list, and a slot this build does not
    /// know about must survive a save made by it rather than being dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<BTreeMap<String, String>>,
    /// `"light"` or `"dark"`; absent → inferred from the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<String>,
    /// Corner radius in px; absent → the UI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<u32>,
}

/// A theme as it crosses to the UI: `name` plus its colours, flat — exactly the
/// shape of the TS `Theme` type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedTheme {
    pub name: String,
    #[serde(flatten)]
    pub colors: ThemeColors,
}

/// What the UI reads and what the watcher broadcasts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeState {
    /// The active theme's name. May name a built-in the UI holds, so it is *not*
    /// required to appear in `user_themes`.
    pub active: String,
    /// Only the operator's themes. The UI adds its code-defined built-ins.
    pub user_themes: Vec<NamedTheme>,
    /// Follow the OS light/dark appearance (herdr's `[theme] auto_switch`).
    /// When on, `light_name`/`dark_name` choose the theme and `active` is the
    /// fallback for when the appearance cannot be read.
    pub auto_switch: bool,
    pub light_name: String,
    pub dark_name: String,
}

/// `theme.toml` on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThemeFile {
    #[serde(default = "default_active")]
    active: String,
    #[serde(default)]
    auto_switch: bool,
    #[serde(default = "default_light")]
    light_name: String,
    #[serde(default = "default_dark")]
    dark_name: String,
    #[serde(default)]
    themes: BTreeMap<String, ThemeColors>,
}

/// `active` defaults to Catppuccin Mocha — not the empty string a derived
/// `Default` would give. The watcher's `unwrap_or_default()` on a malformed
/// hand-edit depends on this, or a bad edit would broadcast an empty active name.
///
/// **Existing users keep what they chose.** This file is only ever written by an
/// explicit theme action (switch, save, delete), so a file that exists names a
/// theme somebody picked — SIGNAL ROOM included — and keeps it. Only a machine
/// with no file at all, i.e. one still on the default, lands on Mocha.
impl Default for ThemeFile {
    fn default() -> Self {
        Self {
            active: default_active(),
            auto_switch: false,
            light_name: default_light(),
            dark_name: default_dark(),
            themes: BTreeMap::new(),
        }
    }
}

fn default_active() -> String {
    DEFAULT_ACTIVE.to_owned()
}

fn default_light() -> String {
    DEFAULT_LIGHT.to_owned()
}

fn default_dark() -> String {
    DEFAULT_ACTIVE.to_owned()
}

const DEFAULT_ACTIVE: &str = "Catppuccin Mocha";
const DEFAULT_LIGHT: &str = "Catppuccin Latte";

/// Built-in names, known here only so a save cannot clobber one. The colours are
/// the UI's (`ui/src/lib/theme.ts`); if this list drifts from that one the only
/// cost is a built-in becoming editable or a name being wrongly refused, so a
/// test pins the pair by count.
const BUILT_IN_NAMES: [&str; 18] = [
    "Catppuccin Mocha",
    "Catppuccin Latte",
    "Tokyo Night",
    "Tokyo Night Day",
    "Dracula",
    "Nord",
    "Gruvbox Dark",
    "Gruvbox Light",
    "One Dark",
    "One Light",
    "Solarized Dark",
    "Solarized Light",
    "Kanagawa",
    "Kanagawa Lotus",
    "Rosé Pine",
    "Rosé Pine Dawn",
    "Vesper",
    "SIGNAL ROOM",
];

fn is_built_in(name: &str) -> bool {
    BUILT_IN_NAMES.contains(&name)
}

/// Remembers the last bytes we wrote, so the watcher can tell our own save from a
/// hand-edit and not echo it back as a change.
#[derive(Default)]
pub struct ThemeStore {
    last_written: Mutex<Option<String>>,
}

fn config_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home directory".to_string())?;
    Ok(home.join(".rmux").join("theme.toml"))
}

fn read_file() -> ThemeFile {
    let Ok(path) = config_path() else {
        return ThemeFile::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "theme.toml is malformed — falling back to built-ins");
            ThemeFile::default()
        }),
        Err(_) => ThemeFile::default(),
    }
}

fn write_file(store: &ThemeStore, file: &ThemeFile) -> Result<(), String> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string_pretty(file).map_err(|e| e.to_string())?;
    *store.last_written.lock() = Some(text.clone());
    std::fs::write(&path, text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

fn to_state(file: ThemeFile) -> ThemeState {
    let user_themes = file
        .themes
        .into_iter()
        .map(|(name, colors)| NamedTheme { name, colors })
        .collect();
    ThemeState {
        active: file.active,
        user_themes,
        auto_switch: file.auto_switch,
        light_name: file.light_name,
        dark_name: file.dark_name,
    }
}

#[tauri::command]
pub fn theme_state() -> ThemeState {
    to_state(read_file())
}

/// Create or update a user theme. Refuses a built-in name — those are immutable
/// and the UI forks them to a copy before an edit ever reaches here.
#[tauri::command]
pub fn theme_save<R: Runtime>(
    app: AppHandle<R>,
    store: State<'_, ThemeStore>,
    theme: NamedTheme,
) -> Result<ThemeState, String> {
    if is_built_in(&theme.name) {
        return Err(format!("\"{}\" is a built-in theme and cannot be overwritten", theme.name));
    }
    if theme.name.trim().is_empty() {
        return Err("a theme needs a name".to_owned());
    }
    let mut file = read_file();
    file.themes.insert(theme.name, theme.colors);
    write_file(&store, &file)?;
    Ok(broadcast(&app, file))
}

/// Write-through happened; tell every window. The watcher *also* sees the file
/// change but suppresses it as our own write, so this explicit emit is the one
/// propagation — an in-app edit reaches the other window, a hand-edit reaches all
/// of them through the watcher instead.
fn broadcast<R: Runtime>(app: &AppHandle<R>, file: ThemeFile) -> ThemeState {
    let state = to_state(file);
    let _ = app.emit(THEME_CHANGED, state.clone());
    state
}

/// Switch the active theme. The name may be a built-in (held by the UI), so it is
/// not required to exist in the stored set.
#[tauri::command]
pub fn theme_set_active<R: Runtime>(
    app: AppHandle<R>,
    store: State<'_, ThemeStore>,
    name: String,
) -> Result<ThemeState, String> {
    let mut file = read_file();
    file.active = name;
    write_file(&store, &file)?;
    Ok(broadcast(&app, file))
}

/// Set (or clear) following the OS appearance. Like `theme_set_active`, the names
/// may be built-ins the UI holds, so they are not checked against the file.
#[tauri::command]
pub fn theme_set_auto<R: Runtime>(
    app: AppHandle<R>,
    store: State<'_, ThemeStore>,
    auto_switch: bool,
    light_name: String,
    dark_name: String,
) -> Result<ThemeState, String> {
    if light_name.trim().is_empty() || dark_name.trim().is_empty() {
        return Err("auto-switch needs both a light and a dark theme".to_owned());
    }
    let mut file = read_file();
    file.auto_switch = auto_switch;
    file.light_name = light_name;
    file.dark_name = dark_name;
    write_file(&store, &file)?;
    Ok(broadcast(&app, file))
}

/// Delete a user theme. A built-in cannot be deleted (it is not in the file). If
/// the deleted theme was active (or an auto-switch pick), it falls back to the
/// default.
#[tauri::command]
pub fn theme_delete<R: Runtime>(
    app: AppHandle<R>,
    store: State<'_, ThemeStore>,
    name: String,
) -> Result<ThemeState, String> {
    let mut file = read_file();
    file.themes.remove(&name);
    if file.active == name {
        file.active = default_active();
    }
    if file.light_name == name {
        file.light_name = default_light();
    }
    if file.dark_name == name {
        file.dark_name = default_dark();
    }
    write_file(&store, &file)?;
    Ok(broadcast(&app, file))
}

/* --------------------------------------------------------------- watcher */

/// Watch the config directory so an external hand-edit of `theme.toml` repaints
/// the running app. Best-effort: a machine where the watch cannot be set up still
/// runs, it just does not pick up edits until the next launch.
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
            tracing::warn!(error = %e, "theme watcher unavailable — hand-edits need a relaunch");
            return;
        }
    };
    // The directory, not the file: editors that save by writing a temp file and
    // renaming over the original change the inode, and a watch on the file itself
    // would go deaf after the first such save.
    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        tracing::warn!(error = %e, "could not watch the config dir for theme edits");
        return;
    }

    std::thread::spawn(move || {
        // Hold the watcher for the life of the thread; dropping it stops the watch.
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

            // Ignore our own writes: compare the file's current bytes to the last
            // ones we wrote. Equal → this is the echo of an in-app save, which the
            // caller already applied; skip it.
            let Ok(current) = std::fs::read_to_string(&path) else { continue };
            {
                let store = app.state::<ThemeStore>();
                let last = store.last_written.lock();
                if last.as_deref() == Some(current.as_str()) {
                    continue;
                }
            }

            let file: ThemeFile = toml::from_str(&current).unwrap_or_default();
            let _ = app.emit(THEME_CHANGED, to_state(file));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_names_match_the_ui_count() {
        // The UI's `BUILT_INS` has eighteen entries; if either list grows this
        // must too, or a new built-in becomes wrongly editable / a name wrongly
        // refused.
        assert_eq!(BUILT_IN_NAMES.len(), 18);
        assert!(is_built_in("SIGNAL ROOM"));
        assert!(is_built_in("Catppuccin Mocha"));
        assert!(is_built_in("Rosé Pine Dawn"));
        assert!(!is_built_in("My Theme"));
    }

    #[test]
    fn a_malformed_file_parses_to_the_default_rather_than_erroring() {
        let file: ThemeFile = toml::from_str("this is not = valid = toml").unwrap_or_default();
        assert_eq!(file.active, DEFAULT_ACTIVE);
        assert!(file.themes.is_empty());
    }

    #[test]
    fn a_file_with_only_active_keeps_the_built_ins_empty() {
        let file: ThemeFile = toml::from_str("active = \"Nord\"").unwrap();
        assert_eq!(file.active, "Nord");
        assert!(file.themes.is_empty());
    }

    #[test]
    fn a_user_theme_round_trips_through_toml() {
        let colors = ThemeColors {
            black: "#0a0a0a".into(),
            red: "#ff6b6b".into(),
            green: "#5ef2b0".into(),
            yellow: "#ffd166".into(),
            blue: "#54b6ff".into(),
            magenta: "#c792ff".into(),
            cyan: "#54e6ff".into(),
            white: "#e8e6e1".into(),
            bright_black: "#7e7b74".into(),
            bright_red: "#ff8b8b".into(),
            bright_green: "#7ef5c4".into(),
            bright_yellow: "#ffdd8a".into(),
            bright_blue: "#7cc7ff".into(),
            bright_magenta: "#d9b0ff".into(),
            bright_cyan: "#8aefff".into(),
            bright_white: "#ffffff".into(),
            background: "#060606".into(),
            foreground: "#e8e6e1".into(),
            bold_text: "#ffffff".into(),
            selection: "#e63b2e".into(),
            cursor: "#e8e6e1".into(),
            accent: "#e63b2e".into(),
            working: "#f2a83c".into(),
            ui: None,
            appearance: None,
            radius: None,
        };
        let mut themes = BTreeMap::new();
        themes.insert("My Theme".to_owned(), colors);
        let file = ThemeFile { active: "My Theme".into(), themes, ..ThemeFile::default() };

        let text = toml::to_string_pretty(&file).unwrap();
        // camelCase keys reach the file, so it reads the way the UI names them.
        assert!(text.contains("brightBlack"));
        assert!(text.contains("[themes.\"My Theme\"]"));

        let back: ThemeFile = toml::from_str(&text).unwrap();
        assert_eq!(back.active, "My Theme");
        assert_eq!(back.themes["My Theme"].accent, "#e63b2e");
    }

    #[test]
    fn a_missing_file_defaults_to_mocha_but_a_stored_choice_is_kept() {
        // No file → the new default. A file that names SIGNAL ROOM was written by
        // an explicit choice and must keep it.
        assert_eq!(ThemeFile::default().active, "Catppuccin Mocha");
        let file: ThemeFile = toml::from_str("active = \"SIGNAL ROOM\"").unwrap();
        assert_eq!(file.active, "SIGNAL ROOM");
        assert!(!file.auto_switch);
        assert_eq!(file.light_name, "Catppuccin Latte");
        assert_eq!(file.dark_name, "Catppuccin Mocha");
    }

    #[test]
    fn a_theme_from_an_older_build_loads_and_round_trips_without_new_keys() {
        // Exactly what a pre-herdr build wrote: 23 colours, nothing else.
        let old = r##"active = "Mine"

[themes.Mine]
black = "#000000"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
blue = "#0000ff"
magenta = "#ff00ff"
cyan = "#00ffff"
white = "#ffffff"
brightBlack = "#808080"
brightRed = "#ff8080"
brightGreen = "#80ff80"
brightYellow = "#ffff80"
brightBlue = "#8080ff"
brightMagenta = "#ff80ff"
brightCyan = "#80ffff"
brightWhite = "#ffffff"
background = "#101010"
foreground = "#e0e0e0"
boldText = "#ffffff"
selection = "#404040"
cursor = "#e0e0e0"
accent = "#ff0000"
working = "#ffaa00"
"##;
        let file: ThemeFile = toml::from_str(old).unwrap();
        let mine = &file.themes["Mine"];
        assert!(mine.ui.is_none() && mine.appearance.is_none() && mine.radius.is_none());
        let text = toml::to_string_pretty(&file).unwrap();
        assert!(!text.contains("ui"), "no empty ui table on re-save:\n{text}");
        assert!(!text.contains("radius"));
    }

    #[test]
    fn ui_slots_round_trip_including_ones_this_build_does_not_know() {
        let toml_in = r##"
[themes.Mine]
black = "#000000"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
blue = "#0000ff"
magenta = "#ff00ff"
cyan = "#00ffff"
white = "#ffffff"
brightBlack = "#808080"
brightRed = "#ff8080"
brightGreen = "#80ff80"
brightYellow = "#ffff80"
brightBlue = "#8080ff"
brightMagenta = "#ff80ff"
brightCyan = "#80ffff"
brightWhite = "#ffffff"
background = "#101010"
foreground = "#e0e0e0"
boldText = "#ffffff"
selection = "#404040"
cursor = "#e0e0e0"
accent = "#ff0000"
working = "#ffaa00"
appearance = "dark"
radius = 6

[themes.Mine.ui]
panelBg = "#181825"
someFutureSlot = "#123456"
"##;
        let file: ThemeFile = toml::from_str(toml_in).unwrap();
        let back: ThemeFile = toml::from_str(&toml::to_string_pretty(&file).unwrap()).unwrap();
        let ui = back.themes["Mine"].ui.as_ref().unwrap();
        assert_eq!(ui["panelBg"], "#181825");
        assert_eq!(ui["someFutureSlot"], "#123456");
        assert_eq!(back.themes["Mine"].radius, Some(6));
        assert_eq!(back.themes["Mine"].appearance.as_deref(), Some("dark"));
    }
}
