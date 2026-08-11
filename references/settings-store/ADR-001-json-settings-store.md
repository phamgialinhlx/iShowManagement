# ADR-001 — A hand-editable JSON settings store (`settings.json`)

Status: **Accepted** (grilled 2026-08-10)

Supersedes part of the storage model assumed by [ADR-002](../ansi-theme/ADR-002-merge-palette-into-appearance.md)
and [ADR-003](../ansi-theme/ADR-003-font-customization.md): those keep appearance preferences in
`localStorage` behind a staged Apply bar. This ADR moves the *settings* subset into a
file-backed, hand-editable source of truth and **removes the Apply bar** (see decision 3).

## Context

rmux stores configuration in two places today:

- **`~/.rmux/theme.toml`** — Rust-owned, hand-editable, **file-watched** (external edits broadcast
  via `THEME_CHANGED`, its own writes suppressed by a byte-compare), exposed to the webview over
  IPC (`theme_state` / `theme_save`), lenient on a malformed edit (`unwrap_or_default()`), and
  cached in `localStorage` for a no-flash startup (`applyCachedThemeEarly` → `initTheme`
  reconciles). This is, already, a single-purpose version of exactly what Zed's `settings.json`
  is.
- **`localStorage`** — everything else, and it mixes two very different kinds of data: portable
  *user settings* (`rmux.appearance`, `rmux.terminal.gpu`, `rmux.terminal.fps`, `rmux.shortcuts`,
  `rmux.notify.*`, `rmux.userCss`, …) and ephemeral *app/session state* (`rmux.deck`, `rmux.grid`,
  `rmux.seen`, the session list, `rmux.jira.*`, widths, caches).

The operator asked for "a json config file store like Zed" — a hand-editable, portable settings
file, rather than per-machine `localStorage`. `theme.toml` proves the mechanism already works
here; this generalises it to all *settings*.

**The settings ↔ state boundary.** Only portable preferences move to the file — the test is
*"would a user want this in their dotfiles and applied on a fresh machine?"* Ephemeral,
machine-specific, or data-like keys (deck layout, grid, `seen` watermarks, sessions, jira
selections, progress activity, caches, panel widths) stay in `localStorage`.

## Decisions

1. **The file is the source of truth for the settings subset (Zed model).** The GUI reads from
   and writes to it; hand-edits are authoritative and apply live. `localStorage` keeps only
   ephemeral state and a no-flash cache. Rejected: a *sync/export layer* over `localStorage`
   (hand-edits would not be authoritative — not actually "like Zed" — and two stores can silently
   disagree, the drift this codebase repeatedly warns against).

2. **`theme.toml` stays; `settings.json` is a sibling.** `~/.rmux/settings.json` beside
   `~/.rmux/theme.toml`. Colours remain a separate concern with a working, watched store and a
   whole palette editor; Zed itself keeps theme *definitions* separate from `settings.json`.
   Rejected: absorbing the colour palettes into `settings.json` (bloats it with 23-colour blobs
   and rewrites a proven path for no functional gain). Accepted cost: **two formats** to
   hand-edit (TOML for colours, JSONC for the rest).

3. **Fully live / file-first; the Apply bar is removed.** This is a deliberate reversal of
   ADR-002, which staged appearance edits so a slider would not "re-lay the window out on every
   tick." Zed has no Apply button, and live preview is the point. Two consequences are load-bearing:
   - **The persist is debounced (decision 4)** so "live" does not mean a file write per tick.
   - **The Settings window itself is never scaled** (`--ui-zoom` pinned to 1 there) is *kept* — so
     the interface-scale slider does not crawl out from under the cursor while dragged. That rule
     was always separate from the staging decision and survives it.

4. **Live preview in-memory, debounced persist to the file (~400ms, coalesced).** A GUI change
   applies to the running document immediately; the *file write* waits for the drag to settle.
   Taken literally, "fully live" would write `settings.json` and fire the watcher forty times
   during one slider drag — disk churn and a preview/watch feedback loop. Debouncing the write
   (not the preview) keeps the live feel and file-first authority without either. Rejected:
   persist-on-every-change (the churn above); an Apply bar (decision 3).

5. **JSONC, with content-preserving writes.** Reads accept comments and trailing commas; a
   hand-editable file that rejects an annotation is broken on its first use. Writes edit **only the
   changed keys** into the existing file text via a JSONC CST editor (`jsonc-parser`), preserving
   comments, key order, and unknown keys — the Zed/VSCode approach. This is *required* by decision
   4: with frequent debounced writes, a whole-object reserialise would erase the user's comments
   the instant they touched any control. Rejected: whole-file rewrite (destroys comments/format);
   plain JSON (no annotation at all — undercuts the entire motivation).

6. **A generated, viewable defaults document; the user file holds only overrides (sparse).**
   `settings.json` contains just the keys the user changed; anything absent resolves to the
   in-code default — so a new version's new defaults reach every user automatically. A generated
   `settings.default.jsonc` (every key, its default, and a doc-comment description) is the
   discoverable reference, openable read-only. Rejected: seeding the user file with every default
   (freezes users on the defaults captured at seed time); no reference doc at all (a hand-editor
   cannot discover options without the GUI).

7. **A single-source Rust schema drives the file side.** Each setting is one declaration — a
   struct field with a serde default and a doc comment — and from it the code derives the in-code
   default, PATCH validation, and the generated defaults document. The rich React Settings panel
   stays hand-built but becomes a thin *writer* over the same keys. This single-sources the
   drift-prone parts (default, validation, description) without regenerating a polished UI.
   Rejected: deriving the GUI too (agent-of-empires all the way — throws away the hand-tuned
   panel); hand-maintaining the doc beside the struct and the GUI (three sources for one fact —
   guaranteed drift).

8. **One-time migration from `localStorage` on first launch, versioned.** If `settings.json` does
   not exist, the known setting keys are read from `localStorage`, written into the file once, and
   a schema version is stamped; thereafter the file is authoritative and those keys are just the
   no-flash cache. Rejected: fresh start (silently discards everyone's configured appearance /
   shortcuts / prefs — data loss); reading `localStorage` as a permanent fallback (two live
   sources forever — the drift this whole ADR removes).

## The model

`~/.rmux/settings.json` is a JSONC document of *user overrides only*. A single Rust `Settings`
struct is the schema: every field carries `#[serde(default)]` and a doc comment, and a
`#[setting(...)]`-style annotation supplies the section/label a surface needs. The struct derives:

- the **default** (a fully-defaulted `Settings`),
- **validation** for an incoming key/value patch,
- the generated **`settings.default.jsonc`** reference (field path, default, doc-comment
  description).

Rust owns the file (`settings.rs`, mirroring `theme.rs`): it loads and merges over defaults on
startup, applies path-scoped patches with a comment-preserving CST edit, watches the file for
external hand-edits (suppressing its own writes by byte-compare), and broadcasts `SETTINGS_CHANGED`
to every window. The webview reads a `localStorage` cache first for a no-flash first paint, then
reconciles against the file over IPC — the `applyCachedThemeEarly` → `initTheme` pattern.

## Invariants this must not break

- **One source of truth.** After migration, a setting lives in the file, not in `localStorage`.
  The only `localStorage` copy is a labelled no-flash *cache*, never read as authoritative.
- **A hand-edit is never destroyed.** Writes preserve comments, order, and unknown keys; a
  *malformed* file falls back to defaults for the running app but is **not overwritten**, so a
  typo costs a reload, not the user's whole config.
- **Leniency.** Unknown keys are ignored; an out-of-range value falls back to its default with a
  *surfaced* notice, never a silent reset (the transcript/theme "never bind to a schema" ethos).
- **No per-tick file writes.** The preview is live; the persist is debounced and coalesced.
- **Cross-window, no restart.** A change (GUI or hand-edit) reaches every window via
  `SETTINGS_CHANGED`; nothing needs a relaunch (the `storage`-event guarantee, now over IPC).
- **The Settings window is never scaled** (decision 3), even though scale is now live everywhere
  else.
- **State stays out of the file.** Ephemeral/session/machine keys remain in `localStorage`; the
  file is dotfiles-portable and contains nothing that would be wrong on another machine.

## Consequences

- **New `src-tauri/src/settings.rs`** — the `Settings` schema, load/merge, path-scoped
  comment-preserving patch (`jsonc-parser`), `notify` watcher with byte-suppression,
  `SETTINGS_CHANGED` emit, and the `settings.default.jsonc` generator. Modelled on `theme.rs`.
- **New `ui/src/lib/settings.ts`** — the webview store: cached read for first paint, IPC
  reconcile on init, `patch(path, value)` (debounced) writing through Rust, and a
  `SETTINGS_CHANGED` listener. Modelled on `theme-runtime.ts`.
- **Every setting getter/setter is rewired.** `terminal-fps.ts`, `terminal-theme.ts` (gpu),
  `user-css.ts`, appearance, shortcuts, notify — each moves from `localStorage.getItem/setItem` to
  the cached store + IPC patch.
- **`AppearancePanel` loses its Apply/Discard bar.** All controls become live (debounced persist),
  and it gains **Open `settings.json`** and **View default settings** buttons (Tauri opener).
- **A new dependency, `jsonc-parser`**, and a `#[setting]` derive/registry for the schema.
- **Two formats coexist** (`theme.toml`, `settings.json`) — accepted (decision 2).
- **Shortcuts live as a `shortcuts` section inside `settings.json`** for v1, not a separate
  `keymap.json`; rmux's action→chord map is simpler than Zed's context-scoped keymap. Splittable
  later without moving anything else.

## Risks / open follow-ups

- **Scope.** The single-source schema (decision 7) and the CST preserving-write path (decision 5)
  are the bulk of the work; both are new machinery. A phased build is possible: plain-JSON
  whole-rewrite first, JSONC + preservation second — at the cost of shipping the drift/clobber
  problems in between.
- **The GUI is a second writer.** Hand-edit-while-a-control-is-mid-debounce is a race; the
  `SETTINGS_CHANGED` reconcile must land the file's value, and an in-flight debounced write must
  re-read before it patches, or a hand-edit can be clobbered by a stale GUI value.
- **Descriptions still double** between the generated doc (from doc-comments) and the hand-built
  GUI copy. Decision 7 single-sources the *file* side only; the GUI keeps its richer prose. If
  they drift, the doc-comment is canonical.
- **`settings.default.jsonc` staleness.** It is generated; a check (like `cargo xtask gen-docs` in
  agent-of-empires) should fail CI if it is out of date, or it silently lies.
