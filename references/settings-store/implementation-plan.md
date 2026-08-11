# Implementation plan — `settings.json` store

Implements [ADR-001](./ADR-001-json-settings-store.md). Mirrors the existing `theme.toml`
mechanism (`src-tauri/src/theme.rs` + `ui/src/lib/theme-runtime.ts`) at every step — read that
pair first; this is the same shape generalised from colours to all settings.

## Phase 0 — schema (the spine everything hangs off)

**`src-tauri/src/settings.rs` (new).** One root `Settings` struct with nested sub-structs per
section (`appearance`, `terminal`, `notify`, `editor`, `shortcuts`, …). Every field:

```rust
/// Cap terminal repaints per second. 0 = uncapped. Lower cuts compositor load on 4K displays.
#[serde(default)]
#[setting(section = "terminal", label = "Frame-rate cap", widget = "segments")]
pub fps: u32,
```

- The doc comment is the description on every surface (defaults doc, and the GUI's canonical text).
- `#[serde(default)]` + a `Default` impl give the in-code default; the file is sparse (overrides only).
- A `#[setting]` derive (or a hand-written registry if the derive is too much for v1) collects, per
  field: the JSON path, the default value, the doc-comment string, and the section/label/widget.
  This registry feeds validation and the generated defaults doc — **single source** (ADR §7).

**Deliverable:** `Settings::default()`, a registry iterable, and unit tests pinning that every
field is reachable and defaults round-trip. Keep test volume low (table-driven; see the
agent-of-empires "one test per behavior" note).

## Phase 1 — Rust file ownership (mirror `theme.rs`)

In `settings.rs`:

- **Path:** `~/.rmux/settings.json` (reuse `theme.rs`'s home resolution).
- **Load:** read file → parse JSONC (`jsonc-parser`) → merge over `Settings::default()`. Malformed
  → return defaults **and a flag**; do **not** overwrite the file (ADR invariant).
- **Patch:** `settings_patch(path, value)` applies a path-scoped edit to the file *text* via
  `jsonc-parser`'s CST edit API, preserving comments / order / unknown keys. Validate the value
  against the registry first; reject with a reason on failure.
- **Watcher:** `notify` on the config dir (not the file inode — renames), byte-compare to suppress
  our own writes, emit `SETTINGS_CHANGED` with the fresh `Settings` on a real external change.
  Copy the suppression bookkeeping from `theme.rs` verbatim.
- **IPC commands:** `settings_state() -> Settings`, `settings_patch(path, value)`,
  `settings_open()` (Tauri opener on the file), `settings_open_defaults()` (write/refresh the
  generated `~/.rmux/settings.default.jsonc` then open it read-only).
- **Defaults doc generator:** walk the registry → emit `settings.default.jsonc` (each field: a
  `// <doc comment>` line then `"path": <default>,`). Add an `xtask`/test that regenerates and
  diffs it so CI fails when it goes stale (ADR risk).

**Verify:** a live test writes a patch, reads it back, confirms an adjacent hand-written comment
survives the patch; a malformed file yields defaults without being rewritten. (Pattern:
`theme.rs`'s tests.)

## Phase 2 — webview store (mirror `theme-runtime.ts`)

**`ui/src/lib/settings.ts` (new).**

- `applyCachedSettingsEarly()` — synchronous, pre-first-paint: read the `rmux.settings.cache`
  JSON from `localStorage` and apply, so there is no flash (exactly `applyCachedThemeEarly`).
- `initSettings()` — async: `invoke("settings_state")`, apply, and update the cache. Reconciles the
  cache against the file (the file wins).
- `get<T>(path)` — read from the in-memory settings object (populated by the two above).
- `patch(path, value)` — **debounced ~400ms, coalesced by path**: apply in-memory immediately
  (live preview), schedule the `invoke("settings_patch", …)`. Update the cache on ack.
- Listen for `SETTINGS_CHANGED` (Tauri event) → replace in-memory + cache, re-apply. A debounced
  write in flight must re-read the latest before it patches, so a concurrent hand-edit is not
  clobbered (ADR risk).

Wire `applyCachedSettingsEarly()` + `initSettings()` into `main.tsx` beside the theme init.

## Phase 3 — migrate + rewire each setting

- **Migration (`ui/src/lib/settings.ts`):** on `initSettings`, if the file is empty/absent and a
  `rmux.settings.migrated` flag is unset, collect the known setting keys from `localStorage`
  (`rmux.appearance`, `rmux.terminal.gpu`, `rmux.terminal.fps`, `rmux.userCss`, `rmux.shortcuts`,
  `rmux.notify.*`, `rmux.editor.autosave`, `rmux.handsFree`, `rmux.debugLogging`, widget-rail
  `enabled`/`order`), map to the new paths, send one `settings_seed` (or a batch of patches),
  stamp the flag. Add `ui/settings-migration-check.ts` (model on `workspace-migration-check.ts`)
  pinning the localStorage→path mapping.
- **Rewire getters/setters** to the store:
  - `terminal-fps.ts`: `terminalFps()` → `get("terminal.fps")`; `setTerminalFps` → `patch`.
  - `terminal-theme.ts`: `gpuRendering()`/`setGpuRendering` → `terminal.gpu`.
  - `user-css.ts`: `loadUserCss`/`saveUserCss` → `appearance.userCss`.
  - appearance (`AppearancePanel` `load`/`save`) → the `appearance.*` paths.
  - shortcuts, notify, editor.autosave, handsFree, debugLogging likewise.
  - Each keeps its **synchronous** read working via the cached in-memory object (no await on the
    hot path); writes go through `patch`.

## Phase 4 — GUI: drop the Apply bar, go live

- **`AppearancePanel`:** remove the Apply/Discard bar and the staged-draft machinery (ADR §3).
  Every control calls `patch(path, value)` on change (debounced persist is inside the store).
  Keep the live-preview repaint the panel already does.
- **Keep `--ui-zoom` pinned to 1 in the Settings window** (unchanged; ADR §3).
- Add **Open `settings.json`** and **View default settings** buttons (call `settings_open` /
  `settings_open_defaults`).
- The existing per-control "applies live" copy becomes true for *all* controls; drop the
  "applies on Apply" split language.

## Verification

- Rust: `cargo test` (settings load/patch/watch/migrate, defaults-doc freshness), `cargo clippy`,
  `cargo fmt`. Cross-compile the Windows target once (`cargo-zigbuild check … x86_64-pc-windows-gnu`)
  since a new Rust module + IPC surface is exactly where the stub/`cfg` drift bites.
- UI: `pnpm exec tsc --noEmit`, `pnpm exec vite build`; add `ui/settings-migration-check.ts` to
  `tsconfig` include and run it in the browser (like the other `*-check` harnesses).
- Manual: set a value in the GUI → confirm `~/.rmux/settings.json` gains *only* that key and keeps
  a hand-written comment; hand-edit the file → confirm the app updates live in every window;
  malformed edit → app keeps running on defaults and the file is untouched; upgrade path → a
  machine with existing `localStorage` settings comes up with them intact in the new file.
- Build the real artefact with `pnpm tauri build` before declaring done (never `cargo build
  --release` — the app would look for Vite). No agent rebuild: this is app-side only.

## Phasing option (if v1 must be smaller)

Ship Phases 0–4 with **plain JSON + whole-file rewrite** first (skip `jsonc-parser`), then add
JSONC + preserving writes as a fast-follow. Cost: between the two, GUI edits reformat the file and
drop comments — so document it and do not advertise hand-editing until the preserving-write phase
lands.
