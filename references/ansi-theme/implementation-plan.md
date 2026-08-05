# Implementation plan — ANSI theme system

Derived from ADR-001. Ordered so each phase is independently testable and the app is never
left visually broken. Phases 1–4 change nothing on screen (SIGNAL ROOM seed reproduces
today); the visible change is phase 5 onward.

## Phase 0 — Model & seed (no behaviour change)

- Define the `Theme` type: 16 ANSI + 5 specials + 2 roles = 23 named colour fields. One shared
  definition the UI and Rust both agree on (TS type + serde struct; keep field names identical).
- Capture the **SIGNAL ROOM seed** from current code: ANSI 16 + Bold Text (`#ffffff`) from
  `terminal-theme.ts`; Background `#060606`, Text `#e8e6e1`, Selection `rgba(230,59,46,.30)`,
  Cursor `#e8e6e1` from `signal-room.css` / theme; Accent `#e63b2e`, Working `#f2a83c`.
- Author the other three built-ins (**Nord**, **Solarized Dark**, **Gruvbox Dark**) as full
  23-value tables — pick each scheme's own Accent (its red) and Working (its yellow/orange).

## Phase 1 — Derivation (the heart)

- One pure function `deriveTokens(theme) -> Record<cssVar, string>` producing the full token
  set: `--app-bg/-panel/-panel-2/-elev` (elevation ramp off Background), `--text/-soft/-faint`
  (ramp off Text→Background — reuse the existing `color-mix` ratios), `--text-bright` (Bold
  Text), `--border/-strong/-hover` (Text at fixed alphas), `--primary` (Accent **as `r g b`
  triplet**), `--busy/--warn` (Working), selection, cursor.
- `terminalTheme(theme)` and `claudeTheme(theme)` derive from the theme's ANSI 16 + specials
  (replaces the hard-coded `TERMINAL_THEME`; keep `TERMINAL_THEME` as the SIGNAL ROOM seed).
- `monacoTheme(theme)` derives syntax hues from the theme's ANSI (keyword=magenta, string=green,
  number=yellow, type=cyan, …) — the mapping `monaco.ts` already encodes, now sourced.
- **Verify contrast**: extend the existing greys check so the derived SIGNAL ROOM ramp still
  measures ≥ today's ratios (rule -1).

## Phase 2 — Rust: canonical file

- `theme.toml` in `app_config_dir()`: `active = "…"` + `[themes.<name>]` tables.
- Commands: `theme_load` (sync, called before first paint), `theme_list`, `theme_save(theme)`,
  `theme_set_active(name)`, `theme_delete(name)`. Built-ins are code-defined and merged over the
  file on read; the file only holds user themes + `active`.
- Missing/corrupt file → rebuild built-ins, `active = "SIGNAL ROOM"`, don't throw.
- ACL grants for any plugin surface touched; app commands need none.

## Phase 3 — Apply pipeline & cross-window sync

- `applyTheme(theme)`: write `deriveTokens` onto `document.documentElement`, push
  `terminalTheme`/`claudeTheme` into every live xterm (appearance channel — xterm doesn't
  re-read CSS), re-define + re-inject the Monaco theme (`ensureThemeStyles`).
- Startup applies the active theme before first paint (Rust hands it over synchronously).
- Cross-window: on switch/apply, Rust writes the file, then bump a `rmux.theme.rev` key in
  `localStorage`; the existing `storage` listener re-fetches via `theme_load` and re-applies.
  (Keeps file-canonical *and* the instant, no-restart behaviour.)
- **File watcher** (`notify`): watch the *directory* containing `theme.toml`; on an external
  change re-read + re-apply (same path as the `rev` ping). Suppress our own writes with an
  ignore-window or a last-written content hash so Apply/switch doesn't echo. Re-arm after an
  atomic-rename save (inode change). Idle cost is ~zero — it's event-driven.
- Fold any existing `rmux.appearance` `textColor`/`accent` override into a seeded user theme so
  no current customisation is lost.

## Phase 4 — The sweep (~40 spots, ~12 files)

- Convert literal hex / `rgba()` in `TopProcesses`, `HostStatus`, `ContextMeter`, `TokenSpend`,
  `SessionRail`, `SignIn`, `LockScreen`, `SshPrompt`, `Settings`, `JiraProgress` onto tokens.
- Route **semantic gauge colours** (load green→amber→red) through the theme's ANSI green/yellow/
  red so meters follow the theme.
- After this, grep proves zero hard-coded chrome colours remain outside the palette defs.

## Phase 5 — UI: switcher + editor (the visible feature)

- New **PALETTE** section in Settings › Appearance (or its own Settings tab):
  - **Theme list**: built-ins (marked read-only) + user themes; active marker; new / duplicate
    / rename / delete. Selecting one switches instantly (writes file, repaints all windows).
  - **Editor** mirroring the macOS Text pane, *colour wells only*: the ANSI 16 grid (Normal /
    Bright rows) + Background / Text / Bold Text / Selection / Cursor + Accent / Working.
  - Editing a built-in **auto-forks** to `<name> (copy)`. Colour edits **live-preview** on the
    workbench; **Apply** writes to file, **Discard** reverts to saved.
  - **Import** (paste an iTerm/base16-ish block or another `theme.toml` table) — cheap way to
    get palettes in without hand-typing 23 wells.

## Test checklist

- `cargo test --workspace`, `cargo clippy --workspace --all-targets` clean.
- `pnpm exec tsc --noEmit`; add any new `*-check.ts` harness to `tsconfig.json` include.
- SIGNAL ROOM active == byte-identical screenshot to pre-change (seed correctness).
- Switch to Nord → whole chrome re-skins, no hard-coded patch survives (grep + eyeball).
- Kill `theme.toml` → built-ins rebuild, `active` falls back, no crash.
- Contrast harness passes for all four built-ins.
- Two-window: switch in Settings → workbench repaints without restart.
- Hand-edit `theme.toml` externally (incl. an atomic-rename save) → running app repaints; an
  in-app Apply does **not** double-apply via the watcher.
- xterm + Monaco both adopt the new palette on switch (not just chrome).

## Explicitly deferred

- Contrast **warning** in the editor for user themes that break rule -1 (flagged, not built).
- A full theme-manager beyond new/dup/rename/delete (tags, ordering, export-to-file button).
