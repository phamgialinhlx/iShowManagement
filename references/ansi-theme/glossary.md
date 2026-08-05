# Glossary — ANSI theme system

Domain terms as used in ADR-001 and the implementation. One meaning each; when code and prose
disagree, this file wins.

- **Theme** — a named set of **23 colour values** that fully determines every colour in rmux.
  Persisted as one `[themes.<name>]` table in `theme.toml`.

- **Palette** — informal synonym for the colour values of a theme. Prefer "theme" for the
  named, persisted thing; "palette" for the raw 23 values.

- **ANSI 16** — the eight normal + eight bright terminal colours (black, red, green, yellow,
  blue, magenta, cyan, white, and their `bright*` variants). The *accent/state* half of a
  theme. Seeded for SIGNAL ROOM from `terminal-theme.ts`.

- **Special** — a theme slot that is not one of the ANSI 16, taken from macOS Terminal.app's
  Text pane: **Background**, **Text** (foreground), **Bold Text**, **Selection**, **Cursor**.

- **Role** — a theme slot that exists only because SIGNAL ROOM needs a colour the macOS pane
  doesn't have: **Accent** and **Working**. See below.

- **Accent** — the "you must act" colour. Drives `--primary`. Rule 0 reserves it for waiting
  prompts, errors, overdue, the caret, `.btn-primary:hover`. Seeded `#e63b2e` — deliberately a
  *different red* from ANSI red (`#ff6b6b`).

- **Working** — the "in progress" amber. Drives `--busy` / `--warn`. Seeded `#f2a83c` — a
  *different yellow* from ANSI yellow (`#ffd166`).

- **Special vs Role vs ANSI** — the three kinds of slot in a theme. 16 ANSI + 5 specials +
  2 roles = 23.

- **Derivation** — the rule that turns 23 theme values into the full design-token set
  (`--app-panel`, `--text-faint`, `--border`, …). E.g. Background → a 4-step elevation ramp;
  Text → a 3-level text ramp mixed toward Background. Deterministic; see ADR §9.

- **Seed** — the built-in default values captured from today's shipping colours, so the
  SIGNAL ROOM theme reproduces the current app exactly.

- **Built-in** — an immutable, code-defined theme (SIGNAL ROOM, Nord, Solarized Dark, Gruvbox
  Dark). Restorable; rebuilt if the file is missing/corrupt.

- **User theme** — a mutable theme in `theme.toml`, created by duplicating or auto-forking a
  built-in.

- **Auto-fork** — editing a colour while a built-in is active creates a `<name> (copy)` user
  theme so the built-in is never overwritten.

- **Active theme** — the one currently applied, named by `active` in `theme.toml`.

- **Re-skin** — apply a theme to the *whole chrome* (not just terminal/editor) by re-deriving
  every token. What makes switching to Nord change the app, not only the terminal.

- **Canonical file** — `theme.toml` in the app config dir is the source of truth. `localStorage`
  is no longer the store for palette state; at most it carries a `rev` ping so other windows
  know to re-fetch.

- **Sweep** — the one-time conversion of the ~40 hard-coded chrome colours onto tokens so
  re-skin has no broken patches. Distinct from the palette *definitions* in `terminal-theme.ts`
  / `monaco.ts`, which become seed/derivation inputs rather than being swept.
