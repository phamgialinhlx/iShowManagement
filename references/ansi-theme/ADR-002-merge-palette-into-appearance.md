# ADR-002 — Merge Palette and Appearance into one Settings panel

Status: **Accepted** (grilled 2026-08-05)
Builds on: [ADR-001](./ADR-001-ansi-theme-system.md), which introduced the **PALETTE** section
as a second Settings tab beside **APPEARANCE**.

## Context

ADR-001 shipped the ANSI theme system as its own Settings entry, **PALETTE**, sitting next to
the pre-existing **APPEARANCE** entry. In use the split reads as arbitrary: both are "how the
app looks," and the two entries even cross-reference each other in prose — Appearance's blurb
points *at* Palette for colour, and Palette explains it owns what Appearance used to. A seam
that has to be narrated in copy is a seam that shouldn't be there.

The two halves do differ underneath, and that difference is the whole design problem:

- **Different persistence.** Palette is file-canonical (`theme.toml` via Rust, ADR-001
  decision 4). Appearance is `localStorage`, applied across windows by the `storage` event.
- **Different Apply semantics.** Palette: switching is instant, colour edits live-preview and
  fork built-ins, its own `sticky` Apply writes the file. Appearance: a staged `draft` vs
  `saved`, its own `sticky` Apply commits, plus two controls that apply live.
- **Two `sticky bottom-0` footers.** You cannot pin two footers in one scroll container — the
  upper un-sticks the moment you scroll to the lower. Any real merge has to resolve this.

The operator asked to "merge tab appearance and palette into one."

## Decisions

1. **One panel, one Apply bar — true consolidation, not a navigation merge.** The two sections
   become a single scrolling panel with **one** dirty state, **one** Apply, **one** Discard,
   **one** sticky footer. The alternative — one sidebar entry over two internally-unchanged
   cards — was rejected: it still leaves two Apply models and forces one of the two footers to
   stop being sticky, which is the visible half of exactly the seam this closes. The theme's
   fork-on-edit machinery stays; only the *commit trigger* moves to the shared bar.

2. **Everything stages; nothing persists until Apply — including the theme switch.** ADR-001
   made theme-*switching* instant (`setActiveTheme` on click). Under one Apply bar that
   undercuts the bar's only promise — "the app still looks the way it did until you press
   Apply." So clicking a theme chip now **previews** it (via `applyTheme`, the same live-preview
   the colour editor already uses) and stages the active-theme choice; Apply persists it,
   Discard reverts it. Cost accepted: flipping to a preset is two clicks (pick, then Apply)
   rather than one. The payoff is a single true mental model instead of two rules.

3. **Three deliberate exemptions stay instant.** The staging rule governs *composing the look*;
   it does not swallow everything on the panel. Exempt, and labelled as such:
   - **Custom CSS** — applies as typed. Its entire value is live feedback; staging it is
     pointless. (Already under an "APPLIED AS YOU TYPE" heading — kept.)
   - **GPU rendering toggle** — reloads the window; a reload cannot be meaningfully staged.
   - **Theme library CRUD — Duplicate / Delete.** These are file operations on the *set* of
     saved themes, not edits to the current look. Staging a deletion behind Apply — and a
     Discard that resurrects it — is stranger than acting now. **Duplicate** creates the file
     but leaves "active" as a *staged* selection (previewed, not persisted), so it can't sneak
     an active-theme change past the bar. **Delete** of the currently-previewed theme reverts
     the preview to the saved active.

   The panel's contract, stated once: **Apply commits the look you composed; the library and the
   two live renderers act now.**

4. **Layout: cohesive blocks, colours first.** Top to bottom —
   - **COLOURS**: theme chips → Duplicate/Delete → the 23-well editor (ANSI grid, specials,
     roles).
   - **MATERIAL**: window backdrop → liquid glass (desktop only) → interface scale → Reset.
   - **LIVE** (under the "APPLIED AS YOU TYPE" boundary): terminal rendering, custom CSS.
   - **Apply bar**: the **last child**, so `sticky bottom-0` actually pins.

   Common-controls-first (chips, then material, then the big editor) was rejected: it puts the
   backdrop *between* the theme chips and their own "EDITING: X" editor, which is the same kind
   of unrelated-feeling seam the merge exists to remove. A settings panel is expected to scroll;
   cohesion beats fold position. Colours lead because the theme *is* the look's identity.

5. **The merged entry is named APPEARANCE.** One sidebar entry replaces two; **PALETTE**
   survives as the *sub-heading* over the colour block, so the concept isn't lost, only nested.
   THEME was rejected (backdrop-image and interface-scale aren't theme data); "PALETTE &
   APPEARANCE" was rejected (a compound label advertises that the merge didn't really happen).
   The blurb names both halves: *"Colours, backdrop and interface scale."* The two
   cross-referencing intro paragraphs are deleted — they only existed to bridge the split.

6. **The preview split is kept: colours preview live, material applies on Apply.** Staging
   unifies *persistence*, not *visual preview* — a separate axis the two halves legitimately
   disagree on, because the risk differs:
   - Colour edits recolour the app instantly (safe repaint; live feedback is the point of a
     colour picker).
   - Backdrop and interface-scale do **not** preview — previewing scale on every drag-tick
     re-lays the whole window out mid-drag, the exact convulsion Appearance was built to avoid
     (ADR-001 context; `AppearancePanel`'s draft model).

   Flattening either way regresses something: strip the colour picker's feedback, or reintroduce
   the scale thrash. The asymmetry tracks the risk, and the Apply-bar status line carries the
   honesty — *"Colours preview live; backdrop and scale apply on Apply."*

7. **The merged Apply bar keeps all three controls: Apply / Apply & restart / Discard.**
   - **Apply** commits everything: staged theme (`setActiveTheme`), colour edits (`saveTheme` +
     fork/rename cleanup), material (`localStorage` + `applyAppearance`). One press, both
     backends.
   - **Apply & restart** — same commit, then relaunch. It still earns its place: interface-scale
     lives in this panel, and a fresh launch is what re-measures the terminals cleanly. Sessions
     survive it (agent-hosted); the copy says so.
   - **Discard** reverts all of it: theme selection back to saved active, colour edits gone,
     material back to saved, live preview repainted to the saved look.

   The status line does double duty — dirty/preview state *and* the `theme.toml` hand-edit note
   (*"Saved to theme.toml; you can hand-edit it and the app repaints"*), which shouldn't vanish
   with the Palette header.

## Invariants this must not break

- **File-canonical palette (ADR-001 §4) is unchanged.** Apply still writes `theme.toml` through
  Rust; the file watcher still repaints on external hand-edits. Staging changes *when* the write
  happens (on Apply, not on click), not *where the truth lives*.
- **Cross-window sync survives.** Committing material still fires the `storage` event; committing
  the theme still bumps `rmux.theme.rev` / emits the theme event so every window re-derives. A
  staged preview must **not** cross windows — only the workbench that owns the Settings window
  previews, exactly as colour edits do today.
- **xterm and Monaco adopt previews too.** A colour preview already pushes into every live xterm
  and re-injects Monaco's theme; a staged *theme switch* now takes the same path, so previewing
  Nord recolours the terminals and editor, not just the chrome.
- **The Apply bar is the last child**, or `sticky bottom-0` silently stops pinning — the bug
  ADR-001's Appearance work already hit once (`AppearancePanel` comment) and must not regress in
  the merge.
- **Rule 0 / rule -1** are untouched — this is a Settings-structure change, no new colour rules.

## Consequences

- **One component owns the combined state.** The single Apply bar needs one dirty flag
  (`materialDirty || themeStaged || colourEdited`), so Palette's state (snapshot, staged
  selection, colour draft, origin) and Appearance's state (`draft`/`saved`) live in one parent
  that renders the sub-sections and the shared footer. `PalettePanel` stops being an independent
  panel with its own footer; its editor/chips become sub-sections of `AppearancePanel`.
- **The Settings nav loses an entry.** `Section` drops `"palette"`; the `PalettePanel` route in
  `Settings.tsx` is removed. One `ErrorBoundary` now wraps the merged panel.
- **Two Apply bars collapse to one.** The palette footer is deleted; its Apply/Discard logic
  moves into the shared bar's `commit`/`discard`.
- No Rust changes: the theme commands, the file watcher and `applyAppearance` are all reused
  as-is. This is a UI reorganisation over unchanged backends.

## Risks / open follow-ups

- **Two-click preset switch (from §2).** Flipping to a preset now needs Apply. If it grates in
  practice, a later option is a per-chip "apply on double-click," but a second interaction model
  is precisely what this ADR removes, so it's deferred, not planned.
- **A tall panel.** COLOURS-first puts the 23-well editor above material. Acceptable per §4; if
  it proves unwieldy, a collapsible editor disclosure is the escape hatch — not built in cut 1.
- **Combined dirty correctness.** Three independent staged inputs feeding one dirty flag is more
  state than either panel had alone; the test checklist pins Discard reverting *all three* and
  Apply committing *both* backends.
