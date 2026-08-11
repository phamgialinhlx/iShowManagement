# Glossary — settings store

- **Setting** — a portable user *preference* that belongs in the config file: it passes the test
  *"would a user want this in their dotfiles and applied on a fresh machine?"* (appearance,
  terminal.gpu/fps, shortcuts, notify.*, userCss, editor.autosave, handsFree, debugLogging).

- **State** — ephemeral, machine-specific, or data-like values that stay in `localStorage` and are
  *not* in the config file (deck layout, grid, `seen` watermarks, session list, jira selections,
  progress activity, caches, panel widths).

- **`settings.json`** — `~/.rmux/settings.json`, a **JSONC** document of user *overrides only*, the
  **source of truth** for settings. Hand-editable and file-watched. Sibling to `theme.toml`
  (which stays the colour store).

- **Overrides-only / sparse** — the file holds just the keys the user changed; any absent key
  resolves to the in-code default, so new-version defaults reach everyone automatically.

- **Defaults document** — a generated, read-only `settings.default.jsonc`: every key, its default,
  and a doc-comment description. The discoverable reference for hand-editors (Zed's `default.json`).

- **Single-source schema** — one Rust `Settings` struct where each field's serde default + doc
  comment + `#[setting]` annotation is the *one* declaration that derives the default, validation,
  and the defaults document.

- **Content-preserving write** — a GUI save edits only the changed keys into the existing file
  *text* (via `jsonc-parser` CST edits), preserving comments, key order, and unknown keys — never a
  whole-object reserialise.

- **Live preview + debounced persist** — a GUI change applies in-memory instantly (no Apply bar);
  the file write waits ~400ms for the interaction to settle. The reversal of ADR-002's staging.

- **No-flash cache** — a `localStorage` copy of the settings, applied synchronously before first
  paint, then reconciled against the file over IPC (the file wins). Never authoritative.

- **`SETTINGS_CHANGED`** — the Rust→webview broadcast (like `THEME_CHANGED`) that propagates a
  change — GUI or hand-edit — to every window with no restart.
