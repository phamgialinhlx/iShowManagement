# Implementation plan — merge Palette into Appearance

Derived from [ADR-002](./ADR-002-merge-palette-into-appearance.md). UI-only reorganisation over
unchanged Rust: no `theme.rs`, no watcher, no `applyAppearance` changes. Ordered so the app is
never left with two broken footers or a half-wired Apply.

## Phase 0 — Lift the combined state into `AppearancePanel`

`AppearancePanel` becomes the single owner of the merged panel's state. It already holds
`saved`/`draft` for material; it gains the palette state that `PalettePanel` held:

- **Snapshot** — `snap = themeSnapshot()`, refreshed via `subscribeTheme` (moved in).
- **Staged active selection** — `selectedName: string`, initialised to `snap.active`. Clicking a
  chip sets this and previews (`applyTheme(resolve(name, snap.user))`); it is **not** persisted
  until Apply (ADR-002 §2).
- **Colour draft** — `colourDraft: Theme | null` and `origin: string | null` (fork/rename
  bookkeeping), exactly as `PalettePanel` had them. Editing a well forks a built-in into
  `colourDraft`, previews live.
- **Derived flags:**
  - `themeStaged = selectedName !== snap.active`
  - `colourEdited = colourDraft !== null`
  - `materialDirty = JSON.stringify(draft) !== JSON.stringify(saved)` (unchanged)
  - `dirty = themeStaged || colourEdited || materialDirty` — the single flag the one Apply bar
    reads.
- **Preview theme** — `colourDraft ?? resolve(selectedName, snap.user)`; this is what the editor
  wells display and what `applyTheme` renders.

Switching a chip clears `colourDraft` (edits belonged to the previous base) and re-previews the
newly selected theme.

## Phase 1 — One `commit()` / `discard()` over both backends

- **`commit()`** (Apply):
  1. If `colourEdited`: `await saveTheme(colourDraft)`; if `origin && origin !== colourDraft.name
     && !isBuiltIn(origin)` → `await deleteTheme(origin)` (rename cleanup, from `PalettePanel`).
     Then `await setActiveTheme(colourDraft.name)`.
  2. Else if `themeStaged`: `await setActiveTheme(selectedName)`.
  3. Material: `localStorage.setItem(...)` + `applyAppearance(draft)` + `setSaved(draft)`
     (unchanged).
  4. Clear `colourDraft`/`origin`; realign `selectedName` to the new active via the refreshed
     snapshot.
- **`discard()`**: `setDraft(saved)` (material); `setColourDraft(null)`/`setOrigin(null)`;
  `setSelectedName(snap.active)`; `applyTheme(resolve(snap.active, snap.user))` to repaint the
  live preview back to saved.
- **Exempt actions stay instant** (ADR-002 §3): Duplicate/Delete call `saveTheme`/`deleteTheme`
  immediately; the GPU toggle and Custom CSS keep their current live behaviour. Duplicate leaves
  `selectedName` staged (previewed, not `setActiveTheme`'d); Delete of the previewed theme reverts
  the preview to saved active.

## Phase 2 — Compose the single panel (layout, ADR-002 §4)

Render order inside one `<section>`, Apply bar **last**:

1. **COLOURS** — sub-heading `PALETTE`; theme chips (`snap.all`, active/staged marker) →
   Duplicate/Delete → the 23-well editor (ANSI Normal/Bright, Special, Role). Lifted from
   `PalettePanel` verbatim, wired to the parent's `edit`/`selectedName`/preview.
2. **MATERIAL** — `BackgroundPicker` → `LiquidGlass` (desktop-only) → interface-scale row →
   Reset.
3. **LIVE** — the "APPLIED AS YOU TYPE" boundary → `TerminalRendering` → `UserCss`.
4. **Apply bar** — the merged `ApplyBar` (Phase 3).

Delete the two cross-referencing intro paragraphs; write one header + blurb: **APPEARANCE**,
*"Colours, backdrop and interface scale."*

## Phase 3 — One Apply bar (ADR-002 §7)

- Single `sticky bottom-0 -mx-6 -mb-6 … pt-3` footer (the pattern already in both panels), the
  **last child** of the scroll container.
- Buttons: **Apply** (`disabled={!dirty}`) → `commit`; **Apply & restart** → `commit` then
  `api.restartApp()`; **Discard** (shown when `dirty`) → `discard`.
- Status line, in priority order: error → busy/restarting → dirty-preview
  (*"Colours preview live; backdrop and scale apply on Apply."*) → idle
  (*"Saved to theme.toml — you can hand-edit it and the app repaints."*).

## Phase 4 — Remove the Palette route

- `Settings.tsx`: drop `"palette"` from `Section`, its `SECTIONS` entry, and the
  `{section === "palette" && …}` branch + `PalettePanel` import.
- Update APPEARANCE's `SECTIONS` blurb to *"Colours, backdrop and interface scale."*
- `PalettePanel.tsx`: its chips/editor/`Well`/`Dot` move into `AppearancePanel` (or a
  co-located `ColourSection` sub-component in the same file). Delete the standalone panel and its
  now-duplicate Apply bar.

## Test checklist

- `pnpm exec tsc --noEmit` clean; any new/renamed `*-check.ts` added to `tsconfig.json` include.
- `pnpm exec vite build` bundles.
- **Single footer:** scroll the whole panel — the Apply bar stays pinned; no second footer
  drifts (the two-sticky-bar bug is gone).
- **Combined dirty:** change only a colour → dirty; change only backdrop → dirty; change only the
  staged theme → dirty; each alone enables Apply.
- **Apply commits both backends:** stage a theme + a colour edit + a backdrop change → Apply →
  `theme.toml` written, active set, `rmux.appearance` written, workbench repainted; no restart.
- **Discard reverts all three:** stage all three → Discard → preview repaints to saved, material
  back to saved, chips back to saved active.
- **Preview split (ADR-002 §6):** dragging a colour well recolours live; dragging interface-scale
  / changing backdrop does **not** move until Apply.
- **Exemptions stay instant:** Duplicate/Delete act immediately; GPU toggle reloads; Custom CSS
  applies as typed — none of them flips the Apply bar's "not applied" copy for the *staged*
  inputs.
- **Two-window:** stage + Apply in Settings → workbench repaints without restart; a *staged*
  preview does not leak to the workbench before Apply.
- **Nav:** Settings shows one APPEARANCE entry, no PALETTE entry; the panel renders under one
  `ErrorBoundary`.

## Explicitly deferred

- Per-chip "apply on double-click" fast-switch (ADR-002 risk §1).
- A collapsible colour-editor disclosure for the tall panel (ADR-002 risk §2).
- Any Rust change — this cut touches no backend.
