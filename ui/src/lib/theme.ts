/**
 * The ANSI theme model, the built-in seeds, and the derivation.
 *
 * ## One palette drives the whole app
 *
 * A `Theme` is the 23 colours macOS Terminal.app's Text pane exposes — the 16
 * ANSI slots plus Background / Text / Bold Text / Selection / Cursor — with two
 * extra *roles* SIGNAL ROOM needs and that pane does not have: **accent** (rule 0,
 * "you must act") and **working** (the in-progress amber). Every other colour in
 * rmux is *derived* from these 23 by `deriveTokens` — the elevation ramp off
 * Background, the three-level text ramp off Text, borders and hover as Text at
 * fixed alphas, the terminal palette, Monaco's syntax hues. There are 23 knobs,
 * not 150; the rest is computed, which is what keeps the contrast ramp (rule -1)
 * from being inverted by three independent greys.
 *
 * ## The values here are opaque; the glass alphas are applied on derivation
 *
 * A theme stores flat, opaque hex — that is what a colour well picks. rmux's
 * structural translucency (the transparent terminal background the panel tints
 * through, the 0.42 plate behind quoted text, borders at 0.14) is *not* stored in
 * the theme; it is re-applied by `terminalTheme`/`deriveTokens`. So the theme file
 * stays a plain list of colours a human can read, and the glass stays a rmux
 * concern rather than something an operator has to understand to change red.
 *
 * ## SIGNAL ROOM reproduces the shipping app
 *
 * The `SIGNAL_ROOM` seed is captured verbatim from `signal-room.css` and
 * `terminal-theme.ts`, so a first run with it active is visually the app as it
 * shipped. The only place the derivation is *approximate* rather than exact is the
 * two dim greys `--text-soft` / `--text-faint`, which come out within ~3 levels of
 * the hand-tuned defaults — the identical tradeoff `[data-custom-text]` in
 * signal-room.css already makes, and imperceptible at the sizes they wear.
 * `ui/theme-check.html` measures it.
 */

/** A complete rmux colour theme. 23 colours + a name. */
export type Theme = {
  name: string;

  // -- ANSI 16 -------------------------------------------------------------
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;

  // -- specials (macOS Terminal's Text pane) -------------------------------
  background: string;
  /** "Text" in the macOS pane — the base foreground. */
  foreground: string;
  /** "Bold Text" — a step brighter, used by a TUI's bold constantly. */
  boldText: string;
  selection: string;
  cursor: string;

  // -- roles (SIGNAL ROOM colours the macOS pane has no slot for) ----------
  /** Rule 0: the one "you must act" colour. Drives `--primary`. */
  accent: string;
  /** In-progress amber. Drives `--busy` / `--warn`. */
  working: string;
};

/** The colour fields, in editor order (ANSI grid, then specials, then roles). */
export const ANSI_KEYS = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow",
  "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
] as const;

export const SPECIAL_KEYS = ["background", "foreground", "boldText", "selection", "cursor"] as const;
export const ROLE_KEYS = ["accent", "working"] as const;

/* ------------------------------------------------------------------ seeds */

/**
 * SIGNAL ROOM — the shipping palette, captured exactly.
 *
 * ANSI + boldText from `terminal-theme.ts`; background/foreground/cursor from
 * `signal-room.css`; selection and accent are the same red the app reserves for
 * "act" (`#e63b2e`), which is why the current terminal selection is a 30%-alpha
 * red rather than a neutral wash; working is the amber `--busy` (`#f2a83c`).
 */
export const SIGNAL_ROOM: Theme = {
  name: "SIGNAL ROOM",
  black: "#0a0a0a",
  red: "#ff6b6b",
  green: "#5ef2b0",
  yellow: "#ffd166",
  blue: "#54b6ff",
  magenta: "#c792ff",
  cyan: "#54e6ff",
  white: "#e8e6e1",
  brightBlack: "#7e7b74",
  brightRed: "#ff8b8b",
  brightGreen: "#7ef5c4",
  brightYellow: "#ffdd8a",
  brightBlue: "#7cc7ff",
  brightMagenta: "#d9b0ff",
  brightCyan: "#8aefff",
  brightWhite: "#ffffff",
  background: "#060606",
  foreground: "#e8e6e1",
  boldText: "#ffffff",
  selection: "#e63b2e",
  cursor: "#e8e6e1",
  accent: "#e63b2e",
  working: "#f2a83c",
};

/** Nord — https://www.nordtheme.com. A cool, low-saturation scheme. */
export const NORD: Theme = {
  name: "Nord",
  black: "#3b4252",
  red: "#bf616a",
  green: "#a3be8c",
  yellow: "#ebcb8b",
  blue: "#81a1c1",
  magenta: "#b48ead",
  cyan: "#88c0d0",
  white: "#e5e9f0",
  brightBlack: "#4c566a",
  brightRed: "#bf616a",
  brightGreen: "#a3be8c",
  brightYellow: "#ebcb8b",
  brightBlue: "#81a1c1",
  brightMagenta: "#b48ead",
  brightCyan: "#8fbcbb",
  brightWhite: "#eceff4",
  background: "#2e3440",
  foreground: "#d8dee9",
  boldText: "#eceff4",
  selection: "#434c5e",
  cursor: "#d8dee9",
  accent: "#bf616a",
  working: "#ebcb8b",
};

/** Solarized Dark — https://ethanschoonover.com/solarized. */
export const SOLARIZED_DARK: Theme = {
  name: "Solarized Dark",
  black: "#073642",
  red: "#dc322f",
  green: "#859900",
  yellow: "#b58900",
  blue: "#268bd2",
  magenta: "#d33682",
  cyan: "#2aa198",
  white: "#eee8d5",
  brightBlack: "#586e75",
  brightRed: "#cb4b16",
  brightGreen: "#586e75",
  brightYellow: "#657b83",
  brightBlue: "#839496",
  brightMagenta: "#6c71c4",
  brightCyan: "#93a1a1",
  brightWhite: "#fdf6e3",
  background: "#002b36",
  foreground: "#93a1a1",
  boldText: "#eee8d5",
  selection: "#073642",
  cursor: "#93a1a1",
  accent: "#dc322f",
  working: "#b58900",
};

/** Gruvbox Dark — https://github.com/morhetz/gruvbox. Warm, high-contrast. */
export const GRUVBOX_DARK: Theme = {
  name: "Gruvbox Dark",
  black: "#282828",
  red: "#cc241d",
  green: "#98971a",
  yellow: "#d79921",
  blue: "#458588",
  magenta: "#b16286",
  cyan: "#689d6a",
  white: "#a89984",
  brightBlack: "#928374",
  brightRed: "#fb4934",
  brightGreen: "#b8bb26",
  brightYellow: "#fabd2f",
  brightBlue: "#83a598",
  brightMagenta: "#d3869b",
  brightCyan: "#8ec07c",
  brightWhite: "#ebdbb2",
  background: "#1d2021",
  foreground: "#ebdbb2",
  boldText: "#fbf1c7",
  selection: "#504945",
  cursor: "#ebdbb2",
  accent: "#fb4934",
  working: "#d79921",
};

/**
 * The built-ins, in switcher order. SIGNAL ROOM first and default.
 *
 * Code-defined so a missing or corrupt `theme.toml` rebuilds them; the file only
 * holds user themes plus which one is active (see `src-tauri/src/theme.rs`).
 */
export const BUILT_INS: readonly Theme[] = [SIGNAL_ROOM, NORD, SOLARIZED_DARK, GRUVBOX_DARK];

/** Names that cannot be edited, renamed or deleted. */
export const BUILT_IN_NAMES: ReadonlySet<string> = new Set(BUILT_INS.map((t) => t.name));

export function isBuiltIn(name: string): boolean {
  return BUILT_IN_NAMES.has(name);
}

/* ------------------------------------------------------------- colour math */

type Rgb = { r: number; g: number; b: number };

/** Parse `#rrggbb` (or `#rgb`). Returns null for anything else, never a guess. */
export function parseHex(hex: string): Rgb | null {
  const s = hex.trim().replace(/^#/, "");
  if (/^[0-9a-fA-F]{3}$/.test(s)) {
    const [r, g, b] = [s.charAt(0), s.charAt(1), s.charAt(2)];
    return {
      r: parseInt(r + r, 16),
      g: parseInt(g + g, 16),
      b: parseInt(b + b, 16),
    };
  }
  if (/^[0-9a-fA-F]{6}$/.test(s)) {
    const n = parseInt(s, 16);
    return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
  }
  return null;
}

const clamp = (n: number) => Math.max(0, Math.min(255, Math.round(n)));
const hex2 = (n: number) => clamp(n).toString(16).padStart(2, "0");

function toHex({ r, g, b }: Rgb): string {
  return `#${hex2(r)}${hex2(g)}${hex2(b)}`;
}

/** `r g b`, the space-separated triplet `--primary` is composed with. */
function triplet(hex: string): string {
  const c = parseHex(hex) ?? { r: 0, g: 0, b: 0 };
  return `${c.r} ${c.g} ${c.b}`;
}

/** `rgba(r, g, b, a)` from an opaque hex. */
function alpha(hex: string, a: number): string {
  const c = parseHex(hex) ?? { r: 0, g: 0, b: 0 };
  return `rgba(${c.r}, ${c.g}, ${c.b}, ${a})`;
}

/** Lighten toward white by a flat sRGB step — the panel elevation ramp. */
function lighten(hex: string, step: number): string {
  const c = parseHex(hex) ?? { r: 0, g: 0, b: 0 };
  return toHex({ r: c.r + step, g: c.g + step, b: c.b + step });
}

/**
 * Mix two hex colours in sRGB. Used for the two dim greys, so the check harness
 * can compute what the browser's `color-mix` would — the applied stylesheet uses
 * a real `color-mix()` string (below), this mirrors it for testing.
 */
export function mix(aHex: string, bHex: string, aWeight: number): string {
  const a = parseHex(aHex) ?? { r: 0, g: 0, b: 0 };
  const b = parseHex(bHex) ?? { r: 0, g: 0, b: 0 };
  const w = Math.max(0, Math.min(1, aWeight));
  return toHex({
    r: a.r * w + b.r * (1 - w),
    g: a.g * w + b.g * (1 - w),
    b: a.b * w + b.b * (1 - w),
  });
}

/* ------------------------------------------------------------- derivation */

/**
 * Turn a theme's 23 colours into the full design-token set.
 *
 * Returns CSS custom-property name → value. `applyTheme` (Phase 3) writes these
 * onto the document root; every `var(--…)` in the app and in `signal-room.css`
 * then re-skins at once. The alpha and elevation constants are structural — they
 * are rmux's, not the operator's — so they stay fixed while the base colour they
 * ride on comes from the theme.
 */
export function deriveTokens(t: Theme): Record<string, string> {
  return {
    // Accent + its soft companion. Bare triplets: every use is
    // `rgb(var(--primary) / <alpha>)`, so a hex here would break every
    // translucent accent at once.
    "--primary": triplet(t.accent),
    "--primary-soft": triplet(t.foreground),

    // Working amber. `--warn` is the same amber as `--busy`; the shipped
    // `#e0a44a`/`#f2a83c` split was two hand-picked ambers for one meaning, and
    // collapsing them onto the single Working slot is imperceptible and honest.
    "--busy": triplet(t.working),
    "--warn": t.working,

    // Background and its elevation ramp. SIGNAL ROOM's #060606 → #0a0a0a →
    // #0e0e0e → #121212 is exactly +4/+8/+12, so this reproduces it to the byte.
    "--app-bg": t.background,
    "--app-panel": lighten(t.background, 4),
    "--app-panel-2": lighten(t.background, 8),
    "--app-elev": lighten(t.background, 12),

    // The raw ANSI Black slot, exposed so chrome that wants to read *darker* than
    // the derived panel elevation (the dock, the top bar) can sit on it directly.
    // Kept a plain colour; the tint/glass translucency is applied at the usage
    // site, exactly like `--app-panel`.
    "--ansi-black": t.black,

    // Borders and hover: the foreground at fixed alphas (the shipped
    // rgba(232,230,225,·) values are #e8e6e1 = Text, so this is exact).
    "--border": alpha(t.foreground, 0.14),
    "--border-strong": alpha(t.foreground, 0.24),
    "--hover": alpha(t.foreground, 0.055),

    // Text and the two derived dimmer levels. Real `color-mix` here (not the JS
    // `mix` above) so the browser computes it; `mix` mirrors it for the check.
    "--text": t.foreground,
    "--text-soft": `color-mix(in srgb, ${t.foreground} 64%, ${t.background})`,
    "--text-faint": `color-mix(in srgb, ${t.foreground} 52%, ${t.background})`,
    "--text-bright": t.boldText,
  };
}

/**
 * The xterm theme for a plain terminal.
 *
 * The 16 ANSI slots pass through opaque. The three values transparency exposes
 * are re-glassed here rather than stored translucent in the theme:
 *  - `background` is fully clear — the panel behind provides the tint.
 *  - `black` is the theme's black at 0.42, because Claude paints an explicit plate
 *    behind quoted text and code; opaque, that plate is a black brick on the
 *    wallpaper. `brightBlack` stays opaque — it is a *foreground* for dim text.
 *  - `selection` is the Selection slot at 0.30 (SIGNAL ROOM's Selection is the
 *    accent red, so this reproduces the shipped `rgba(230,59,46,0.30)`).
 */
export function terminalTheme(t: Theme): Record<string, string> {
  return {
    background: "rgba(0, 0, 0, 0)",
    foreground: t.foreground,
    cursor: t.cursor,
    // The glyph *under* a block cursor — the panel shade, not the pure bg, so it
    // reads against the cursor fill (shipped `#0a0a0a`).
    cursorAccent: lighten(t.background, 4),
    selectionBackground: alpha(t.selection, 0.3),
    black: alpha(t.black, 0.42),
    brightBlack: t.brightBlack,
    red: t.red,
    green: t.green,
    yellow: t.yellow,
    blue: t.blue,
    magenta: t.magenta,
    cyan: t.cyan,
    // `white` follows the foreground, not the ANSI white slot — a TUI uses
    // default-coloured text far more than an explicit ANSI white, and it should
    // match the app's Text. Bold (`brightWhite`) rides Bold Text for the same
    // reason it is a step brighter everywhere else.
    white: t.foreground,
    brightRed: t.brightRed,
    brightGreen: t.brightGreen,
    brightYellow: t.brightYellow,
    brightBlue: t.brightBlue,
    brightMagenta: t.brightMagenta,
    brightCyan: t.brightCyan,
    brightWhite: t.boldText,
  };
}

/** The Claude pane: identical to the terminal, cursor included. It once forced
 *  the cursor to the accent (red), but that made the composer caret the loudest
 *  thing in the pane; it now follows `t.cursor` like every other terminal. */
export function claudeTheme(t: Theme): Record<string, string> {
  return terminalTheme(t);
}

/* ----------------------------------------------------------------- monaco */

/** A Monaco token rule. `foreground` is a 6-digit hex *without* the `#`. */
type MonacoRule = { token: string; foreground: string; fontStyle?: string };

/** What `monaco.editor.defineTheme` wants, produced from a `Theme`. */
export type MonacoThemeData = {
  base: "vs-dark";
  inherit: true;
  rules: MonacoRule[];
  colors: Record<string, string>;
};

const noHash = (hex: string) => hex.replace(/^#/, "");
/** `#rrggbb` + a 2-char alpha → Monaco's `#rrggbbaa`. */
const withA = (hex: string, aa: string) => `${hex}${aa}`;

/**
 * The syntax theme, derived from a theme's ANSI hues.
 *
 * The mapping is `monaco.ts`'s, now sourced from the palette rather than
 * hard-coded: keyword=magenta, string=green, number=yellow, type=cyan,
 * function/key=blue, comment=brightBlack, and the one red — errors, unmatched
 * brackets, the caret — is the accent. So a string is the same green in the
 * editor, a transcript and a shell, and it re-skins with the terminal. Selection
 * and bracket-nesting colours stay *neutral* (foreground at low alpha), never the
 * Selection slot: Monaco derives a red selection block from a coloured one, which
 * reads as an error on every line the caret visits.
 */
export function monacoTheme(t: Theme): MonacoThemeData {
  const fg = t.foreground;
  const soft = mix(fg, t.background, 0.64); // == --text-soft
  const param = mix(fg, t.background, 0.82); // variable.parameter
  const op = mix(fg, t.background, 0.72); // operator
  const meta = mix(t.brightBlack, fg, 0.6); // comment.doc / meta

  return {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "", foreground: noHash(fg) },
      { token: "comment", foreground: noHash(t.brightBlack), fontStyle: "italic" },
      { token: "comment.doc", foreground: noHash(meta), fontStyle: "italic" },
      { token: "keyword", foreground: noHash(t.magenta) },
      { token: "keyword.flow", foreground: noHash(t.magenta) },
      { token: "keyword.json", foreground: noHash(t.magenta) },
      { token: "storage", foreground: noHash(t.magenta) },
      { token: "tag", foreground: noHash(t.magenta) },
      { token: "string", foreground: noHash(t.green) },
      { token: "string.escape", foreground: noHash(t.brightCyan) },
      { token: "string.key", foreground: noHash(t.blue) },
      { token: "string.value", foreground: noHash(t.green) },
      { token: "attribute.value", foreground: noHash(t.green) },
      { token: "number", foreground: noHash(t.yellow) },
      { token: "constant", foreground: noHash(t.yellow) },
      { token: "regexp", foreground: noHash(t.yellow) },
      { token: "annotation", foreground: noHash(t.yellow) },
      { token: "type", foreground: noHash(t.cyan) },
      { token: "type.identifier", foreground: noHash(t.cyan) },
      { token: "namespace", foreground: noHash(t.cyan) },
      { token: "class", foreground: noHash(t.cyan) },
      { token: "struct", foreground: noHash(t.cyan) },
      { token: "interface", foreground: noHash(t.cyan) },
      { token: "function", foreground: noHash(t.blue) },
      { token: "support.function", foreground: noHash(t.blue) },
      { token: "variable.predefined", foreground: noHash(t.blue) },
      { token: "variable.parameter", foreground: noHash(param) },
      { token: "attribute.name", foreground: noHash(t.blue) },
      { token: "identifier", foreground: noHash(fg) },
      { token: "variable", foreground: noHash(fg) },
      { token: "delimiter", foreground: noHash(soft) },
      { token: "operator", foreground: noHash(op) },
      { token: "meta", foreground: noHash(meta) },
      { token: "invalid", foreground: noHash(t.accent) },
    ],
    colors: {
      "editor.background": "#00000000",
      "editor.foreground": fg,
      "editorLineNumber.foreground": mix(fg, t.background, 0.36),
      "editorLineNumber.activeForeground": soft,
      "editorCursor.foreground": t.accent,
      "editor.selectionBackground": withA(fg, "33"),
      "editor.inactiveSelectionBackground": withA(fg, "1a"),
      "editor.selectionHighlightBackground": withA(fg, "14"),
      "editor.wordHighlightBackground": withA(fg, "14"),
      "editor.wordHighlightStrongBackground": withA(fg, "1f"),
      "editor.findMatchBackground": withA(t.working, "47"),
      "editor.findMatchHighlightBackground": withA(t.working, "29"),
      "editor.lineHighlightBackground": withA(fg, "0a"),
      "editorIndentGuide.background1": withA(fg, "14"),
      "editorIndentGuide.activeBackground1": withA(fg, "24"),
      "editorWidget.background": lighten(t.background, 8),
      "editorWidget.border": withA(fg, "24"),
      "editorSuggestWidget.background": lighten(t.background, 8),
      "editorSuggestWidget.selectedBackground": withA(fg, "14"),
      // Muted nesting cues: neutral greys with a hint of the palette's own hues,
      // never loud enough to out-shout a control.
      "editorBracketHighlight.foreground1": soft,
      "editorBracketHighlight.foreground2": mix(t.magenta, soft, 0.3),
      "editorBracketHighlight.foreground3": mix(t.green, soft, 0.3),
      "editorBracketHighlight.foreground4": mix(t.yellow, soft, 0.35),
      "editorBracketHighlight.foreground5": op,
      "editorBracketHighlight.foreground6": mix(fg, t.background, 0.48),
      "editorBracketHighlight.unexpectedBracket.foreground": t.accent,
      "editorBracketPairGuide.activeBackground1": withA(fg, "24"),
      "editorError.foreground": t.accent,
      "editorWarning.foreground": t.working,
      "scrollbarSlider.background": withA(lighten(t.background, 30), "99"),
      "scrollbarSlider.hoverBackground": withA(lighten(t.background, 40), "cc"),
      "scrollbarSlider.activeBackground": lighten(t.background, 40),
    },
  };
}
