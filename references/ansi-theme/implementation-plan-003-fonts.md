# Implementation plan — font customization (UI font + Mono font)

Derived from [ADR-003](./ADR-003-font-customization.md). UI-only over unchanged Rust: no
`theme.rs`, no watcher, no new IPC. Font is `localStorage` material beside `scale`, previews live
like a theme chip, commits on the shared Apply bar. Ordered so the app never ships a control that
restyles the chrome but skips the terminal/editor.

## Phase 0 — Bundle the faces (`fonts.css`, assets)

- Add `@fontsource` dependencies and imports for the five bundled families:
  `@fontsource/inter`, `@fontsource/ibm-plex-sans`, `@fontsource/jetbrains-mono`,
  `@fontsource/fira-code`, `@fontsource/cascadia-code` (weights 400/500/700 as available;
  IBM Plex Mono is already imported). All `@import` lines stay at the top of `fonts.css` before
  the `@font-face` blocks (CSS requires imports first — the file already notes this).
- Vendor **Lilex** (Zed's editor font, `.ZedMono`) into `ui/public/fonts/` as `.woff2` and add
  `@font-face` blocks mirroring the SFU Futura ones (`font-display: block`); copy its SIL OFL
  licence alongside. Source it from the Zed tree (`references/zed/assets/fonts/lilex`). Zed's UI
  font (`.ZedSans`) *is* IBM Plex Sans, already bundled via @fontsource, so no separate face is
  needed for it. (An earlier cut bundled the deprecated Iosevka "Zed Sans/Zed Mono" — removed.)
- **Verify offline:** these are npm/Vite assets and local files — grep the built bundle to
  confirm no `fonts.googleapis.com`/CDN URL slipped in (the invariant `fonts.css` guards).

## Phase 1 — The font registry (`ui/src/lib/fonts.ts`, new)

The single source of truth every other piece reads:

```
type FontRole = "ui" | "mono";
type FontProvider = "fontsource" | "bundled" | "system";
type FontDef = { id: string; label: string; role: FontRole; provider: FontProvider; stack: string };

export const UI_FONTS: FontDef[]   // SFU Futura (default), Inter, IBM Plex Sans, Zed Sans, System UI
export const MONO_FONTS: FontDef[] // IBM Plex Mono (default), JetBrains Mono, Cascadia Code, Fira Code, Zed Mono, SF Mono/Menlo
export const DEFAULT_UI_FONT   = "sfu-futura";
export const DEFAULT_MONO_FONT = "ibm-plex-mono";
export function resolveFont(id: string, role: FontRole): FontDef  // falls back to the role default on unknown id
```

- Each `stack` carries the same generic fallbacks the tokens have today (e.g. mono ends
  `…, ui-monospace, "SF Mono", Menlo, monospace`; UI ends `…, ui-sans-serif, system-ui, sans-serif`).
- The two `system` entries have no bundled asset — their `stack` is just native names
  (`ui-monospace, "SF Mono", Menlo, monospace` / `system-ui, sans-serif`).
- `resolveFont` returning the default on an unknown id makes an old/corrupt stored value safe.

## Phase 2 — Extend `Appearance` and `applyAppearance` (`AppearancePanel.tsx`)

- Add to the `Appearance` type and `DEFAULTS`:
  `uiFont: string` (= `DEFAULT_UI_FONT`), `monoFont: string` (= `DEFAULT_MONO_FONT`). `load()`'s
  `{ ...DEFAULTS, ...parsed }` spread already makes these safe for pre-existing stored settings.
- In `applyAppearance`, in the `// --- type and colour ---` region, resolve both ids and write the
  tokens on `:root`:
  ```
  root.style.setProperty("--font-display", resolveFont(a.uiFont, "ui").stack);
  root.style.setProperty("--font-body", "var(--font-display)");
  root.style.setProperty("--font-mono", resolveFont(a.monoFont, "mono").stack);
  ```
  The chrome and metric widgets read these tokens already — they follow with nothing else.
- **Push the mono stack into xterm + Monaco.** Reuse the appearance channel the colour theme
  already uses to push palettes into live terminals (see `terminal-theme.ts` / the appearance
  push in the xterm hosts). The mono stack must reach:
  - `Terminal.tsx` — replace the hard-coded `fontFamily: '"IBM Plex Mono", …'` with the resolved
    mono stack, and re-apply + re-fit on the appearance signal (xterm does not re-read CSS).
  - the Claude pane's xterm host — same.
  - `CodeEditor.tsx` — Monaco `fontFamily` from the resolved stack; update on the appearance
    signal (`editor.updateOptions({ fontFamily })`).
  This is the phase that makes the feature real; without it the terminal/editor keep IBM Plex Mono
  regardless of the picker (ADR-003 §4, the failure mode).

## Phase 3 — The TYPE section + live preview (`AppearancePanel.tsx`)

- Add a **TYPE** section between COLOURS and MATERIAL: two chip rows — **UI FONT** (from
  `UI_FONTS`) and **MONO FONT** (from `MONO_FONTS`). Each chip renders its own label *in its own
  face* (`style={{ fontFamily: def.stack }}`) so the list previews itself, and marks the staged
  selection with `aria-pressed` + the underline treatment the theme chips use.
- **Staging = the draft, exactly like the other material knobs.** Clicking a chip sets
  `draft.uiFont` / `draft.monoFont`. Because font previews live (ADR-003 §5), the click also calls
  `applyAppearance` with the *draft* immediately (chrome + xterm + Monaco repaint), while `saved`
  stays put — so `materialDirty` (already `JSON.stringify(draft) !== JSON.stringify(saved)`) flips
  and the Apply bar lights up. This differs from backdrop/scale, which mutate `draft` but do **not**
  call `applyAppearance` until Apply. Keep that distinction explicit in a comment: *font and colour
  preview on change; backdrop and scale wait for Apply.*
- `commit()` already does `applyAppearance(draft)` + persist + `setSaved(draft)` — fonts ride it
  unchanged. `discard()` already does `setDraft(saved)`; add an `applyAppearance(saved)` if the
  live preview needs repainting back (the material discard path may already re-apply — reuse it).
- **Reset** clears `uiFont`/`monoFont` to defaults alongside the material knobs.
- **Status line:** fonts join the "previews live" clause — keep the ADR-002 wording, extend to
  *"Colours and fonts preview live; backdrop and scale apply on Apply."*

## Phase 4 — Verify

- `pnpm exec tsc --noEmit` clean; any new `*-check.ts` added to `tsconfig.json` include.
- `pnpm exec vite build` bundles; grep the built assets for CDN font URLs → none (offline
  invariant).
- **Every mono surface changes:** pick JetBrains Mono → terminal, Claude pane, code editor **and**
  the metric widgets all switch; pick it back → all revert. This is the regression the ADR exists
  to prevent — test it explicitly, not just the chrome.
- **UI font:** pick Inter → labels/headings/body switch; terminal stays monospace.
- **Live preview + staging:** clicking a chip repaints immediately and lights the Apply bar;
  Discard repaints back to saved; a staged preview does **not** leak to the other window before
  Apply.
- **Persistence + cross-window:** Apply in Settings → workbench repaints with no restart; relaunch
  → the chosen fonts are still applied (defaults if never set).
- **Terminal re-fit:** switching the mono font re-fits the terminal once (cell metrics change) and
  sends one resize; it does not stream resizes (discrete click, not a slider).
- **Defaults unchanged:** a profile that never touches the picker renders in SFU Futura +
  IBM Plex Mono, byte-for-byte today.

## Explicitly deferred (ADR-003 risks)

- Per-role font **size** control (bigger code without bigger chrome).
- Font **file upload** / typing an arbitrary installed font name.
- A **ligature** toggle (and the terminal-alignment re-check it would require).
- Trimming bundled weights if app size proves a problem.
