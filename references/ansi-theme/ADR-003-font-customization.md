# ADR-003 — Font customization (UI font + Mono font)

Status: **Accepted** (grilled 2026-08-05)
Builds on: [ADR-002](./ADR-002-merge-palette-into-appearance.md), which merged Palette and
Appearance into one panel behind a single Apply bar. This ADR adds a **TYPE** section to that
panel.

## Context

rmux's typography is carried by three CSS custom properties in `ui/src/styles/signal-room.css`:

- `--font-display` — the UI/chrome face, **SFU Futura** (labels, headings, body).
- `--font-body` — currently just `var(--font-display)`.
- `--font-mono` — the data/code face, **IBM Plex Mono** (terminal, Claude pane, editor, every
  metric readout).

Two facts framed the work:

- **Fonts are bundled, never fetched** (`ui/src/styles/fonts.css`). The old app pulled Plex Mono
  from Google Fonts and rendered in a fallback face on every cold start — and wrong entirely on a
  machine with no network, which is a normal state for a tool whose whole job is talking to
  remote servers. This is an invariant, not a preference: **no font may load over the network.**
- **The terminal and editor do not read the CSS token.** `Terminal.tsx:91`, the Claude pane, and
  `CodeEditor.tsx:60` all hard-code `"IBM Plex Mono"` directly into their xterm/Monaco config.
  The metric widgets (`TopProcesses`, `TokenSpend`) *do* read `var(--font-mono)`. So changing the
  CSS variable alone would restyle the chrome and the widgets but silently skip the three
  surfaces the operator stares at all day — the exact trap the terminal-*colour* fix already hit
  (CLAUDE.md, "the terminals are part of the appearance system, not exempt from it").

The operator asked to "add Font so that this application can be customized with the font I want."

## Decisions

1. **Two independent controls: a UI font and a Mono font.** The CSS already splits display from
   mono, and the split is load-bearing: the terminal, the Claude TUI, the code editor and every
   column-aligned metric readout *must* be monospace or their rendering breaks. A single
   whole-app font control was rejected — it would let a proportional face reach the terminal and
   misalign every column. Two pickers keep the guarantee structural rather than advisory.

2. **A curated, bundled list — not system enumeration, not file upload.** The selectable fonts
   ship inside the app and are chosen from a fixed set. Rejected alternatives:
   - *Type any installed font name.* WKWebView cannot enumerate installed fonts
     (`queryLocalFonts()` is Chromium-only), so a typed name that isn't installed falls back
     silently with nothing to tell the operator it didn't take — and the result differs per
     machine.
   - *Upload a font file.* Portable, but a whole copy/store/cleanup path (à la the background
     picker) for a first cut.

   The accepted cost: "the font I want" must be a font we put on the list. The payoff is that
   every option renders **identically on every machine and with no network** — the same reason
   the colour palette is bundled and the same invariant `fonts.css` already enforces. Extending
   the list later is additive (one entry + one bundled face).

3. **Stored as `localStorage` material, beside interface scale — `theme.toml` stays colour-only.**
   This follows the settled convention of the field:
   - **VS Code** keeps `editor.fontFamily` / `terminal.integrated.fontFamily` in per-machine user
     settings, beside `window.zoomLevel`; colour themes contain *only* colours, never fonts.
   - **Ghostty / Alacritty / WezTerm / Kitty** keep `font-family` and `theme`/`colors` as
     independent top-level config keys — changing one never touches the other.
   - **Browsers, Obsidian, Notion** treat font as an Appearance setting decoupled from any theme.
   - The lone counter-example, **iTerm2/Terminal.app Profiles**, bundles font+colour because a
     *profile* is its atom of configuration — which rmux is not structured around.

   So font is a top-level appearance preference, not theme data. It lives beside `scale` in
   `rmux.appearance`, commits on the shared Apply bar, and syncs across windows via the `storage`
   event — exactly the mechanism ADR-001/002 already use. Keeping it out of `theme.toml` means no
   Rust schema change and no coupling where switching palette would silently change your font.

4. **One mono font drives every monospace surface.** The Mono picker sets terminal, Claude pane,
   code editor and data readouts together. A single `font-family` (Ghostty/Alacritty-style) was
   chosen over VS Code's separate editor-vs-terminal fonts: the latter doubles the controls and
   wiring for a distinction few people set differently. Because two of those surfaces hard-code
   the font, applying the choice is **two moves, not one**:
   - Set `--font-mono` / `--font-display` on `:root` — the chrome and the widgets follow for free
     (they already read the tokens).
   - **Push the resolved family into xterm and Monaco** on the same appearance channel the colour
     theme already uses to push palettes into live terminals. xterm does not re-read CSS; Monaco
     is configured in JS. Without this push the feature would visibly skip the terminal and
     editor — the ADR's whole failure mode.

5. **Font previews live, like a theme chip; it is staged and committed on Apply.** A font is
   picked from a list of chips — a *discrete* click, not a continuously-dragged slider — so it
   carries none of the per-tick layout thrash that made ADR-002 hold backdrop and interface-scale
   until Apply. It behaves exactly like clicking a theme chip (ADR-002 §2/§6): the click repaints
   chrome + terminal + editor at once so the operator sees it, the choice is *staged*, Apply
   persists it and Discard reverts it. Font therefore joins the **"colours preview live"** side of
   the Apply-bar status line; backdrop and scale remain the "apply on Apply" side.

   One consequence to honour: previewing a mono font re-fits the terminals (the cell metrics
   change), which sends one resize to the far side. That is fine — it is a single discrete event
   per click, not the continuous stream a dragged slider produces, so the existing resize
   debounce (`lib/terminal-resize.ts`) is not stressed. Discard re-fits back.

6. **The lists.** Defaults are unchanged — a first run looks exactly as it does today.
   - **Mono:** IBM Plex Mono *(default)*, JetBrains Mono, Cascadia Code, Fira Code, **Lilex · Zed**,
     and **SF Mono / Menlo** as a zero-bundle *system* option.
   - **UI:** SFU Futura *(default)*, Inter, **IBM Plex Sans · Zed**, and **System UI** as a
     zero-bundle *system* option.

   **The two "Zed" options are the fonts Zed actually ships.** Checked against the Zed source
   tree (`references/zed/assets/settings/default.json`): Zed's `ui_font_family` is `.ZedSans`,
   which aliases to **IBM Plex Sans**, and its `buffer_font_family` is `.ZedMono`, which aliases
   to **Lilex**. So the recognisable "Zed look" is IBM Plex Sans for the UI (already bundled via
   @fontsource, hence no separate face — the one entry is labelled *"IBM Plex Sans · Zed"*) and
   Lilex for code. An earlier cut bundled the *deprecated* Iosevka-based "Zed Sans/Zed Mono"
   (from the old `zed-fonts` 1.2.0 release), which looked nothing like current Zed and confused
   the operator — those were removed.

   Bundling route: `@fontsource/*` npm packages (Vite bundles them into the app, no CDN — the
   same mechanism as IBM Plex Mono today) for Inter, IBM Plex Sans, JetBrains Mono, Fira Code and
   Cascadia Code. **Lilex** is the one face not on @fontsource, so it is bundled as raw `.woff2`
   in `ui/public/fonts` with a hand-written `@font-face`, exactly like SFU Futura today — taken
   from the Zed source tree, SIL OFL 1.1, licence in `public/fonts/LILEX-LICENSE.txt`. The two
   *system* options (SF Mono/Menlo, System UI) add **no bytes** — they resolve to native faces
   via ordinary CSS names — at the cost of not being identical across OSes; they are offered as
   the escape hatch for anyone who prefers their machine's native face over byte-for-byte parity.

7. **Family only in this cut; size stays with interface scale.** A per-role font *size* (bigger
   code without bigger chrome, VS Code's `editor.fontSize`) is a real but separate want. The
   existing interface-scale (zoom) control already enlarges everything including the terminal, so
   family alone satisfies "the font I want" without a second slider and a second value to push
   into xterm/Monaco. Deferred, not planned.

8. **Placement: a new TYPE section, directly after COLOURS.** Panel order becomes
   **COLOURS → TYPE → MATERIAL → LIVE → Apply bar.** TYPE holds the two font pickers (UI font,
   then Mono font). This groups the two **live-preview identity axes** — colour and type —
   together, and leaves the **apply-on-Apply material knobs** (backdrop, glass, interface scale)
   in one block below. Layout then matches preview behaviour, so the status-line split
   ("previews live" vs "applies on Apply") lines up with where the controls physically sit,
   rather than cutting through the middle of a section. Folding the pickers into MATERIAL beside
   scale (where they are *stored*) was rejected for exactly that reason — it would mix live and
   staged controls in one block.

## The font model

A stored value is a **font id** (a stable string like `"jetbrains-mono"`), not a raw CSS stack.
A single registry maps each id to its display label, its full `font-family` stack (with the
same generic fallbacks the tokens carry today), its role (`ui` | `mono`), and how it is provided
(`fontsource` | `bundled` | `system`). The registry is the one place that knows a font exists;
the picker renders from it, `applyAppearance` resolves ids through it, and the xterm/Monaco push
reads the resolved stack from it. Storing an id rather than a stack means a font can be renamed
or its fallbacks tuned without rewriting everyone's saved settings.

`rmux.appearance` gains two fields: `uiFont: string` and `monoFont: string`, defaulting to the
ids of SFU Futura and IBM Plex Mono so an absent/old setting is exactly today's look.

## Invariants this must not break

- **No font loads over the network** (`fonts.css`). Every bundled option is a Vite/@font-face
  asset; the two system options resolve to native faces by name. Nothing fetches.
- **Every monospace surface actually changes.** The mono choice must reach xterm (terminal +
  Claude) and Monaco (editor) via the appearance push, not only the CSS token — or the feature
  ships broken on the surfaces that matter most (the terminal-colour lesson, CLAUDE.md).
- **Defaults are byte-for-byte today's look.** SFU Futura + IBM Plex Mono remain the defaults; an
  operator who never opens the picker sees no change.
- **Cross-window sync survives.** Committing a font fires the `storage` event; the workbench and
  Settings windows re-derive with no restart, and a *staged* preview does not leak across windows
  before Apply — exactly as colour edits behave.
- **Preview split (ADR-002 §6) is preserved.** Fonts preview live (discrete click, safe repaint);
  interface scale and backdrop still wait for Apply.
- **The Apply bar stays the last child** of the scroll container, or `sticky bottom-0` stops
  pinning (ADR-002 invariant).
- **Rule 0 / rule -1** untouched — this adds a typography axis, no new colour rules.
- **Reset clears fonts too.** The Appearance Reset returns `uiFont`/`monoFont` to their defaults
  along with the material knobs; a font left set with nothing in the UI able to reach it would be
  the same orphaned-state litter Reset already guards against for the background file.

## Consequences

- **`Appearance` gains `uiFont`/`monoFont`; `applyAppearance` resolves and applies them.** It sets
  `--font-display` and `--font-mono` on `:root` (they become dynamic rather than the static values
  in `signal-room.css`), then — inside the `isTauri()` block that already handles glass — nudges
  the appearance channel so live xterms and Monaco adopt the mono stack. The chrome and widgets
  need nothing further; they already read the tokens.
- **A font registry module** (`ui/src/lib/fonts.ts`) holds the id→stack table, `UI_FONTS` and
  `MONO_FONTS` lists for the pickers, and a resolver. It is the single source of truth the panel,
  `applyAppearance`, and the terminal/Monaco push all read.
- **`fonts.css` grows the bundled faces** — @fontsource imports for the five @fontsource families
  and `@font-face` blocks for Zed Mono/Zed Sans, all with `font-display: block` to match the
  existing anti-reflow policy.
- **The TYPE section** is added to `AppearancePanel` between the COLOURS and MATERIAL sections,
  wired to the same staged-draft/commit/discard machinery the material knobs already use.
- **No Rust change.** Colour stays in `theme.toml`; fonts ride the existing `localStorage` +
  `storage`-event path. The xterm/Monaco push reuses the appearance channel the colour theme
  already drives.

## Risks / open follow-ups

- **Bundle size.** Five bundled families (× a few weights) add megabytes to the app. Accepted as
  the price of offline byte-parity (decision 2); the two *system* options exist for anyone who
  would rather add nothing. If it bites, weights can be trimmed to 400/700 per family.
- **Lilex is a manual bundle.** Not on @fontsource, so its files and OFL licence are vendored
  into `ui/public/fonts` (from the Zed source tree) and kept current by hand — the one face
  without a package to track upstream.
- **Stale font ids fall back silently.** A setting saved by the earlier cut naming the removed
  `zed-sans`/`zed-mono` ids resolves through `resolveFont` to the role default (SFU Futura /
  IBM Plex Mono), so it reverts to the default rather than erroring — acceptable for a deprecated
  option nobody kept.
- **Ligature fonts (Fira Code, JetBrains Mono, Cascadia Code).** xterm has no ligature addon
  loaded and Monaco defaults `fontLigatures` off, so ligatures render as plain glyphs with **no
  column-alignment risk** today. If a ligature toggle is ever added, that assumption must be
  re-checked for the terminal specifically.
- **Per-role font size (from §7).** Deferred; would add a slider and a second value to push into
  xterm/Monaco.
