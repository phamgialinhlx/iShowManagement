# ADR-001 — A configurable, file-backed ANSI theme system

Status: **Accepted** (grilled 2026-08-05)
Supersedes: the `localStorage`-only `rmux.appearance` colour handling for palette concerns.

## Context

rmux's look is governed by SIGNAL ROOM (`ui/src/styles/signal-room.css`): a monochrome
chrome (three greys derived from `--text`), one red accent (`--primary`, "you must act" —
rule 0), and amber for "working". Separately, the terminal carries a full 16-slot ANSI
palette (`ui/src/lib/terminal-theme.ts`) and Monaco a syntax theme (`ui/src/lib/monaco.ts`)
whose hues already mirror the terminal's.

The operator asked to "set up ANSI colours and only use those for the app, make it
configurable, save the config to a file, and give it a UI" — modelled on macOS Terminal.app's
**Text** preference pane (ANSI grid + Background / Text / Bold Text / Selection / Cursor wells).

Two facts framed the work:

- **No appearance state touches disk today.** `rmux.appearance`, the GPU flag and user CSS
  all live in `localStorage`, applied instantly across windows via the `storage` event. A
  config *file* is new plumbing, not a move.
- **The app is already ~token-driven.** Of the colour references in the UI, **506 inline
  styles already use `var(--…)`**; only ~19 literal hexes + ~20 literal `rgba()` are
  hard-coded in chrome (clustered in the metrics widgets). The 25 hexes in
  `terminal-theme.ts` and 17 in `monaco.ts` are palette *definitions*, not scattered debt.

## Decisions

1. **ANSI is the single source of truth; chrome derives from it.** Not a literal repaint of
   the chrome in 16 saturated colours (that would overturn rule 0 and the measured contrast
   ramp), and not terminal-only. Every colour in the app resolves from a named palette slot.

2. **A theme is 23 values** — the macOS Text-pane shape plus two SIGNAL ROOM roles:
   - **16 ANSI**: 8 normal + 8 bright.
   - **5 specials**: Background, Text (foreground), Bold Text, Selection, Cursor.
   - **2 roles**: **Accent** (rule 0 "act") and **Working** (amber). These exist because the
     app uses colours the macOS pane does not: the chrome accent `#e63b2e` is a *different
     red* from ANSI red `#ff6b6b`, and "working" amber `#f2a83c` is a *different yellow* from
     ANSI yellow `#ffd166`. Folding them into the ANSI slots would change the current look;
     leaving them hard-coded would break "only theme colours". Two extra wells is the only
     option that keeps both promises.

3. **The default seed is today's exact colours.** "Make the current colours the new theme":
   the built-in **SIGNAL ROOM** theme reproduces the shipping app pixel-for-pixel, so a first
   run has zero visual change. Accent stays `#e63b2e`, Working stays `#f2a83c`.

4. **The file is canonical.** Rust owns `theme.toml`, reads it *synchronously before the
   window shows* (as the scale setting already does, so no defaults-flash), and every edit
   writes through Rust. `localStorage` is no longer the store for palette state — a config
   file that isn't the source of truth reads as broken the first time it's hand-edited.

5. **Format & location: TOML at `<app config dir>/theme.toml`.** Hand-editability was part of
   the reason for choosing file-canonical; TOML is friendlier to edit and comment than the
   `serde_json` used for the machine-only keychain blob. Schema: a top-level `active = "…"`
   plus one `[themes.<name>]` table per theme.

6. **Full switcher.** Named themes with an active marker; UI list with new / duplicate /
   rename / delete.

7. **Built-ins are immutable and restorable.** Editing a colour while a built-in is active
   auto-forks to a user copy (`SIGNAL ROOM (copy)`); the baseline is never silently
   overwritten. A missing or corrupt file rebuilds the built-ins from code. User themes live
   in the file and are fully editable/deletable. Shipped built-ins: **SIGNAL ROOM** (default),
   **Nord**, **Solarized Dark**, **Gruvbox Dark** — three colourful schemes so the switcher
   has somewhere to go on day one and proves the derivation on non-monochrome palettes.

8. **A colourful preset re-skins the whole chrome**, via derivation — not just the terminal
   and editor. Switching to Nord genuinely re-skins the app. This is the payoff of "ANSI is
   the source", and it is what forces decision 10.

9. **Derivation map** (theme value → design tokens):

   | Theme slot   | Drives |
   |--------------|--------|
   | Background   | `--app-bg`, `--app-panel`, `--app-panel-2`, `--app-elev` (elevation ramp, mixed) |
   | Text         | `--text`; `--text-soft` / `--text-faint` (mixed toward Background — the code does this today) |
   | Bold Text    | `--text-bright`, xterm `brightWhite` |
   | Selection    | `::selection`, xterm selection |
   | Cursor       | caret-color, xterm cursor |
   | ANSI 16      | terminal theme, Monaco syntax hues, semantic gauge colours (load green→amber→red) |
   | Accent       | `--primary` (emitted as a bare `r g b` triplet, never hex — every use is `rgb(var(--primary) / α)`) |
   | Working      | `--busy`, `--warn` |

10. **The chrome sweep is bounded.** ~19 literal hexes + ~20 literal `rgba()` across ~12
    files (mostly `TopProcesses`, `HostStatus`, `ContextMeter`, `TokenSpend`, `SessionRail`)
    move onto tokens. Their semantic colours (a load gauge) route through the theme's ANSI
    green/yellow/red so the gauges follow the theme too. The 506 existing `var(--…)` refs
    re-skin for free.

11. **Editing semantics: switch = instant; colour edit = live-preview + Apply.** Switching the
    active theme writes the file and repaints every window at once. Opening a colour well
    previews on the live workbench as you drag, but is not written to `theme.toml` until
    Apply — matching the draft/Apply pattern `AppearancePanel` already uses, so a fumbled edit
    is one Discard from safe.

12. **A file watcher makes external hand-edits repaint instantly.** Rust watches `theme.toml`
    and re-applies on external change, so editing the file in a text editor updates the running
    app without a relaunch — the UI is a live editor over the canonical file, in both
    directions. Cost is negligible: event-driven (FSEvents via `notify`), ~zero idle, and a
    change only triggers the same cheap re-derive the switcher runs. The two things to get right
    are correctness, not performance:
    - **Suppress our own writes** (ignore-window or content-hash compare) so Apply/switch does
      not echo back through the watcher and re-apply what was just applied.
    - **Handle atomic-rename saves** — editors that write-temp-then-rename change the inode, so
      watch the *directory* (or re-arm after rename) or the watcher goes deaf after the first
      external edit.

## SIGNAL ROOM invariants this must not break

- **Rule -1 (legibility).** The derived `--text-soft` / `--text-faint` ramp must keep prose
  at ≥ the current contrast and 9px labels ≥ 4.5:1 over Background. The four built-ins are
  curated to satisfy this; an arbitrary *user* theme can invert it (see Risks).
- **Rule 0 (red = act).** Accent is the only "act" colour; each preset defines its Accent
  explicitly. `--primary` stays a triplet.
- **Rule 2 / rule 3 / rule 1** (blink, emoji, radius) are untouched — this is colour only.
- **xterm does not re-read CSS.** A theme switch must *push* the new palette into every live
  xterm on the appearance channel (both terminal and Claude panes), as `applyAppearance`
  already does.
- **Monaco injects its theme stylesheet only when an editor is constructed**
  (`ensureThemeStyles`). Re-theming on switch means re-defining the Monaco theme and
  re-triggering that injection, or transcripts render tokenised-but-grey.

## Consequences

- Cross-window sync with a canonical *file*: on switch/apply, Rust writes `theme.toml`, then a
  cheap signal (a `rmux.theme.rev` bump on `localStorage`, or a Tauri event) tells every window
  to re-fetch from Rust and re-apply. Keeps file-canonical *and* instant cross-window.
- A new IPC surface: `theme_load`, `theme_list`, `theme_save`, `theme_set_active`,
  `theme_delete`. All plugin/app-command ACL as usual.
- Migration: existing `rmux.appearance` `textColor` / `accent` overrides fold into the seeded
  SIGNAL ROOM theme (or a one-time user copy) so nobody's current override is lost.

## Risks / open follow-ups

- **User themes can break rule -1.** A user picking a Background and Text that don't contrast
  produces illegible 9px labels. Options for later: a contrast check in the editor that warns
  (non-blocking), à la the design-system note that measures greys. Not built in cut 1.
- **Colourful chrome vs rule 0.** Re-skinning the chrome means non-red hues now appear on
  chrome surfaces under Nord/Gruvbox. Rule 0 is about *the accent*, which each preset still
  reserves for "act" — but the visual quiet SIGNAL ROOM buys from monochrome is a preset
  choice now, and that's the operator's to make.
