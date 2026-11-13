/**
 * The theme model, the built-in seeds, and the derivation.
 *
 * ## One palette drives the whole app
 *
 * A `Theme` is the 23 colours macOS Terminal.app's Text pane exposes — the 16
 * ANSI slots plus Background / Text / Bold Text / Selection / Cursor — plus two
 * rmux *roles* that pane does not have: **accent** ("you must act", which drives
 * `--primary`) and **working** (in progress, which drives `--busy`).
 *
 * On top of that sits an optional **UI palette** (`ui`): the nineteen slots herdr
 * draws its chrome from — accent, panel/sidebar/active-row/selection backgrounds,
 * a surface ramp, two overlay greys, text/subtext, and seven named hues. herdr's
 * built-ins define every slot by hand, and rmux's built-ins copy those
 * assignments exactly (see `HERDR_*` below). A theme without `ui` — SIGNAL ROOM,
 * and every theme an older build saved to `theme.toml` — gets each slot
 * *derived* from its 23 colours by `slots()`, so nothing stored before this
 * existed fails to load, and any single slot may be overridden on its own.
 *
 * ## The values here are opaque; the glass alphas are applied on derivation
 *
 * A theme stores flat, opaque hex — that is what a colour well picks. rmux's
 * structural translucency (the transparent terminal background the panel tints
 * through, the 0.42 plate behind quoted text, borders at 0.14) is *not* stored in
 * the theme; it is re-applied by `terminalTheme`/`deriveTokens`. So the theme file
 * stays a plain list of colours a human can read, and the glass stays a rmux
 * concern rather than something an operator has to understand to change red.
 * That is also why every theme works under native Liquid Glass, a painted
 * backdrop or the desktop: the theme supplies colour, the material is rmux's.
 *
 * ## SIGNAL ROOM reproduces the app as it shipped
 *
 * The `SIGNAL_ROOM` seed is captured verbatim from the old `signal-room.css` and
 * `terminal-theme.ts`, and has no `ui` block, so every slot is derived and every
 * legacy token comes out byte-identical (`ui/theme-check.html` pins it). It also
 * keeps its square corners (`radius: 0`); every other theme is softened to 4px.
 */

/** herdr's nineteen chrome slots, in herdr's own order (`Palette` in herdr). */
export const SLOT_KEYS = [
  "accent",
  "panelBg",
  "sidebarBg",
  "activeRowBg",
  "selectionBg",
  "surface0",
  "surface1",
  "surfaceDim",
  "overlay0",
  "overlay1",
  "text",
  "subtext0",
  "mauve",
  "green",
  "yellow",
  "red",
  "blue",
  "teal",
  "peach",
] as const;

export type SlotKey = (typeof SLOT_KEYS)[number];
export type Slots = Record<SlotKey, string>;

/** A complete rmux colour theme. 23 colours + a name, plus optional extras. */
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
  /** "Text" in the macOS pane — the terminal's base foreground. */
  foreground: string;
  /** "Bold Text" — a step brighter, used by a TUI's bold constantly. */
  boldText: string;
  selection: string;
  cursor: string;

  // -- roles (colours the macOS pane has no slot for) ----------------------
  /** The one "you must act" colour. Drives `--primary`. */
  accent: string;
  /** In progress. Drives `--busy` / `--warn`. */
  working: string;

  // -- optional (absent in themes saved by older builds) -------------------
  /** herdr's UI palette. Any slot left out is derived — see `slots()`. */
  ui?: Partial<Slots>;
  /** Light or dark. Absent → inferred from the background's luminance. */
  appearance?: "light" | "dark";
  /** Corner radius in px for panels, menus, chips. Absent → 4. */
  radius?: number;
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
 * SIGNAL ROOM — the palette rmux shipped with, captured exactly.
 *
 * ANSI + boldText from the old `terminal-theme.ts`; background/foreground/cursor
 * from `signal-room.css`; selection and accent are the red the app reserved for
 * "act" (`#e63b2e`); working is the amber `--busy` (`#f2a83c`). No `ui` block:
 * every slot is derived, which is what keeps it byte-identical to the old app.
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
  appearance: "dark",
  radius: 0,
};

/**
 * Build a herdr-family theme: the official terminal palette for the ANSI and
 * special slots, herdr's slot assignments verbatim for `ui`. The two rmux roles
 * follow herdr's state colours — "act" is herdr's red (blocked), "working" is its
 * yellow — so the rail, the terminal and the alerts all agree.
 */
function herdr(
  name: string,
  appearance: "light" | "dark",
  term: Omit<Theme, "name" | "accent" | "working" | "ui" | "appearance" | "radius">,
  ui: Omit<Slots, "sidebarBg"> & { sidebarBg?: string },
): Theme {
  return {
    name,
    ...term,
    accent: ui.red,
    working: ui.yellow,
    appearance,
    // herdr's sidebar is `Color::Reset` — the terminal's own background.
    ui: { sidebarBg: term.background, ...ui },
  };
}

/* The ANSI blocks below are each project's published terminal palette
 * (Alacritty/kitty ports); the `ui` blocks are herdr's `Palette::*` (Apache-2.0),
 * transcribed from its `Color::Rgb` values. */

/** Catppuccin Mocha — https://catppuccin.com/palette. The default. */
export const CATPPUCCIN_MOCHA = herdr(
  "Catppuccin Mocha",
  "dark",
  {
    black: "#45475a", red: "#f38ba8", green: "#a6e3a1", yellow: "#f9e2af",
    blue: "#89b4fa", magenta: "#f5c2e7", cyan: "#94e2d5", white: "#bac2de",
    brightBlack: "#585b70", brightRed: "#f38ba8", brightGreen: "#a6e3a1", brightYellow: "#f9e2af",
    brightBlue: "#89b4fa", brightMagenta: "#f5c2e7", brightCyan: "#94e2d5", brightWhite: "#a6adc8",
    background: "#1e1e2e", foreground: "#cdd6f4", boldText: "#cdd6f4",
    selection: "#585b70", cursor: "#f5e0dc",
  },
  {
    accent: "#89b4fa", panelBg: "#181825", activeRowBg: "#1e1e2e", selectionBg: "#313244",
    surface0: "#313244", surface1: "#45475a", surfaceDim: "#1e1e2e", overlay0: "#6c7086",
    overlay1: "#7f849c", text: "#cdd6f4", subtext0: "#a6adc8", mauve: "#cba6f7",
    green: "#a6e3a1", yellow: "#f9e2af", red: "#f38ba8", blue: "#89b4fa", teal: "#94e2d5",
    peach: "#fab387",
  },
);

/** Catppuccin Latte — the light flavour. */
export const CATPPUCCIN_LATTE = herdr(
  "Catppuccin Latte",
  "light",
  {
    black: "#5c5f77", red: "#d20f39", green: "#40a02b", yellow: "#df8e1d",
    blue: "#1e66f5", magenta: "#ea76cb", cyan: "#179299", white: "#acb0be",
    brightBlack: "#6c6f85", brightRed: "#d20f39", brightGreen: "#40a02b", brightYellow: "#df8e1d",
    brightBlue: "#1e66f5", brightMagenta: "#ea76cb", brightCyan: "#179299", brightWhite: "#bcc0cc",
    background: "#eff1f5", foreground: "#4c4f69", boldText: "#4c4f69",
    selection: "#acb0be", cursor: "#dc8a78",
  },
  {
    accent: "#1e66f5", panelBg: "#eff1f5", activeRowBg: "#e6e9ef", selectionBg: "#bdd0f5",
    surface0: "#ccd0da", surface1: "#bcc0cc", surfaceDim: "#e6e9ef", overlay0: "#9ca0b0",
    overlay1: "#8c8fa1", text: "#4c4f69", subtext0: "#6c6f85", mauve: "#8839ef",
    green: "#40a02b", yellow: "#df8e1d", red: "#d20f39", blue: "#1e66f5", teal: "#179299",
    peach: "#fe640b",
  },
);

/** Tokyo Night (night) — https://github.com/folke/tokyonight.nvim. */
export const TOKYO_NIGHT = herdr(
  "Tokyo Night",
  "dark",
  {
    black: "#15161e", red: "#f7768e", green: "#9ece6a", yellow: "#e0af68",
    blue: "#7aa2f7", magenta: "#bb9af7", cyan: "#7dcfff", white: "#a9b1d6",
    brightBlack: "#414868", brightRed: "#f7768e", brightGreen: "#9ece6a", brightYellow: "#e0af68",
    brightBlue: "#7aa2f7", brightMagenta: "#bb9af7", brightCyan: "#7dcfff", brightWhite: "#c0caf5",
    background: "#1a1b26", foreground: "#c0caf5", boldText: "#c0caf5",
    selection: "#283457", cursor: "#c0caf5",
  },
  {
    accent: "#7aa2f7", panelBg: "#1a1b26", activeRowBg: "#232636", selectionBg: "#2d3650",
    surface0: "#24283b", surface1: "#414868", surfaceDim: "#1a1b26", overlay0: "#565f89",
    overlay1: "#697196", text: "#c0caf5", subtext0: "#a9b1d6", mauve: "#bb9af7",
    green: "#9ece6a", yellow: "#e0af68", red: "#f7768e", blue: "#7aa2f7", teal: "#7dcfff",
    peach: "#ff9e64",
  },
);

/** Tokyo Night Day — the light style. */
export const TOKYO_NIGHT_DAY = herdr(
  "Tokyo Night Day",
  "light",
  {
    black: "#e9e9ed", red: "#f52a65", green: "#587539", yellow: "#8c6c3e",
    blue: "#2e7de9", magenta: "#9854f1", cyan: "#007197", white: "#6172b0",
    brightBlack: "#a1a6c5", brightRed: "#f52a65", brightGreen: "#587539", brightYellow: "#8c6c3e",
    brightBlue: "#2e7de9", brightMagenta: "#9854f1", brightCyan: "#007197", brightWhite: "#3760bf",
    background: "#e1e2e7", foreground: "#3760bf", boldText: "#3760bf",
    selection: "#b7c1e3", cursor: "#3760bf",
  },
  {
    accent: "#2e7de9", panelBg: "#e1e2e7", activeRowBg: "#d2d3da", selectionBg: "#b6cae7",
    surface0: "#c4c8da", surface1: "#a8aecb", surfaceDim: "#d2d3da", overlay0: "#8990b3",
    overlay1: "#68709a", text: "#3760bf", subtext0: "#6172b0", mauve: "#7847bd",
    green: "#587539", yellow: "#8c6c3e", red: "#f52a65", blue: "#2e7de9", teal: "#118c74",
    peach: "#b15c00",
  },
);

/** Dracula — https://draculatheme.com/contribute (spec). */
export const DRACULA = herdr(
  "Dracula",
  "dark",
  {
    black: "#21222c", red: "#ff5555", green: "#50fa7b", yellow: "#f1fa8c",
    blue: "#bd93f9", magenta: "#ff79c6", cyan: "#8be9fd", white: "#f8f8f2",
    brightBlack: "#6272a4", brightRed: "#ff6e6e", brightGreen: "#69ff94", brightYellow: "#ffffa5",
    brightBlue: "#d6acff", brightMagenta: "#ff92df", brightCyan: "#a4ffff", brightWhite: "#ffffff",
    background: "#282a36", foreground: "#f8f8f2", boldText: "#ffffff",
    selection: "#44475a", cursor: "#f8f8f2",
  },
  {
    accent: "#bd93f9", panelBg: "#282a36", activeRowBg: "#373c52", selectionBg: "#463f5d",
    surface0: "#44475a", surface1: "#6272a4", surfaceDim: "#282a36", overlay0: "#6272a4",
    overlay1: "#828cb4", text: "#f8f8f2", subtext0: "#d2d2dc", mauve: "#ff79c6",
    green: "#50fa7b", yellow: "#f1fa8c", red: "#ff5555", blue: "#8be9fd", teal: "#8be9fd",
    peach: "#ffb86c",
  },
);

/** Nord — https://www.nordtheme.com. */
export const NORD = herdr(
  "Nord",
  "dark",
  {
    black: "#3b4252", red: "#bf616a", green: "#a3be8c", yellow: "#ebcb8b",
    blue: "#81a1c1", magenta: "#b48ead", cyan: "#88c0d0", white: "#e5e9f0",
    brightBlack: "#4c566a", brightRed: "#bf616a", brightGreen: "#a3be8c", brightYellow: "#ebcb8b",
    brightBlue: "#81a1c1", brightMagenta: "#b48ead", brightCyan: "#8fbcbb", brightWhite: "#eceff4",
    background: "#2e3440", foreground: "#d8dee9", boldText: "#eceff4",
    selection: "#434c5e", cursor: "#d8dee9",
  },
  {
    accent: "#88c0d0", panelBg: "#2e3440", activeRowBg: "#434c5e", selectionBg: "#40505d",
    surface0: "#3b4252", surface1: "#434c5e", surfaceDim: "#2e3440", overlay0: "#4c566a",
    overlay1: "#646e82", text: "#eceff4", subtext0: "#d8dee9", mauve: "#b48ead",
    green: "#a3be8c", yellow: "#ebcb8b", red: "#bf616a", blue: "#81a1c1", teal: "#8fbcbb",
    peach: "#d08770",
  },
);

/** Gruvbox Dark (medium) — https://github.com/morhetz/gruvbox. */
export const GRUVBOX_DARK = herdr(
  "Gruvbox Dark",
  "dark",
  {
    black: "#282828", red: "#cc241d", green: "#98971a", yellow: "#d79921",
    blue: "#458588", magenta: "#b16286", cyan: "#689d6a", white: "#a89984",
    brightBlack: "#928374", brightRed: "#fb4934", brightGreen: "#b8bb26", brightYellow: "#fabd2f",
    brightBlue: "#83a598", brightMagenta: "#d3869b", brightCyan: "#8ec07c", brightWhite: "#ebdbb2",
    background: "#282828", foreground: "#ebdbb2", boldText: "#fbf1c7",
    selection: "#504945", cursor: "#ebdbb2",
  },
  {
    accent: "#d79921", panelBg: "#282828", activeRowBg: "#323130", selectionBg: "#4b3f27",
    surface0: "#3c3836", surface1: "#504945", surfaceDim: "#282828", overlay0: "#928374",
    overlay1: "#a89984", text: "#ebdbb2", subtext0: "#d5c4a1", mauve: "#d3869b",
    green: "#b8bb26", yellow: "#fabd2f", red: "#fb4934", blue: "#83a598", teal: "#8ec07c",
    peach: "#fe8019",
  },
);

/** Gruvbox Light (medium). */
export const GRUVBOX_LIGHT = herdr(
  "Gruvbox Light",
  "light",
  {
    black: "#fbf1c7", red: "#cc241d", green: "#98971a", yellow: "#d79921",
    blue: "#458588", magenta: "#b16286", cyan: "#689d6a", white: "#7c6f64",
    brightBlack: "#928374", brightRed: "#9d0006", brightGreen: "#79740e", brightYellow: "#b57614",
    brightBlue: "#076678", brightMagenta: "#8f3f71", brightCyan: "#427b58", brightWhite: "#3c3836",
    background: "#fbf1c7", foreground: "#3c3836", boldText: "#282828",
    selection: "#d5c4a1", cursor: "#3c3836",
  },
  {
    accent: "#076678", panelBg: "#fbf1c7", activeRowBg: "#f2e5bc", selectionBg: "#ebdbb2",
    surface0: "#ebdbb2", surface1: "#d5c4a1", surfaceDim: "#f2e5bc", overlay0: "#928374",
    overlay1: "#7c6f64", text: "#3c3836", subtext0: "#504945", mauve: "#8f3f71",
    green: "#79740e", yellow: "#b57614", red: "#9d0006", blue: "#076678", teal: "#427b58",
    peach: "#af3a03",
  },
);

/** One Dark — Atom's One Dark syntax/terminal colours. */
export const ONE_DARK = herdr(
  "One Dark",
  "dark",
  {
    black: "#3f4451", red: "#e06c75", green: "#98c379", yellow: "#e5c07b",
    blue: "#61afef", magenta: "#c678dd", cyan: "#56b6c2", white: "#abb2bf",
    brightBlack: "#5c6370", brightRed: "#be5046", brightGreen: "#98c379", brightYellow: "#d19a66",
    brightBlue: "#61afef", brightMagenta: "#c678dd", brightCyan: "#56b6c2", brightWhite: "#ffffff",
    background: "#282c34", foreground: "#abb2bf", boldText: "#d7dae0",
    selection: "#3e4451", cursor: "#528bff",
  },
  {
    accent: "#61afef", panelBg: "#282c34", activeRowBg: "#313640", selectionBg: "#334659",
    surface0: "#2c313a", surface1: "#3e4451", surfaceDim: "#282c34", overlay0: "#5c6370",
    overlay1: "#737a87", text: "#abb2bf", subtext0: "#969ca8", mauve: "#c678dd",
    green: "#98c379", yellow: "#e5c07b", red: "#e06c75", blue: "#61afef", teal: "#56b6c2",
    peach: "#d19a66",
  },
);

/** One Light — Atom's One Light. */
export const ONE_LIGHT = herdr(
  "One Light",
  "light",
  {
    black: "#383a42", red: "#e45649", green: "#50a14f", yellow: "#c18401",
    blue: "#4078f2", magenta: "#a626a4", cyan: "#0184bc", white: "#a0a1a7",
    brightBlack: "#696c77", brightRed: "#ca1243", brightGreen: "#50a14f", brightYellow: "#986801",
    brightBlue: "#4078f2", brightMagenta: "#a626a4", brightCyan: "#0184bc", brightWhite: "#202227",
    background: "#fafafa", foreground: "#383a42", boldText: "#202227",
    selection: "#e5e5e6", cursor: "#526fff",
  },
  {
    accent: "#4078f2", panelBg: "#fafafa", activeRowBg: "#d8dbe2", selectionBg: "#cddbf8",
    surface0: "#f0f0f1", surface1: "#e5e5e6", surfaceDim: "#f5f5f6", overlay0: "#a0a1a7",
    overlay1: "#686b77", text: "#383a42", subtext0: "#686b77", mauve: "#a626a4",
    green: "#50a14f", yellow: "#c18401", red: "#e45649", blue: "#4078f2", teal: "#0184bc",
    peach: "#986801",
  },
);

/** Solarized Dark — https://ethanschoonover.com/solarized. */
export const SOLARIZED_DARK = herdr(
  "Solarized Dark",
  "dark",
  {
    black: "#073642", red: "#dc322f", green: "#859900", yellow: "#b58900",
    blue: "#268bd2", magenta: "#d33682", cyan: "#2aa198", white: "#eee8d5",
    // brightBlack is base01 rather than the spec's base03 (= the background),
    // so dim text a TUI paints in "bright black" stays readable.
    brightBlack: "#586e75", brightRed: "#cb4b16", brightGreen: "#586e75", brightYellow: "#657b83",
    brightBlue: "#839496", brightMagenta: "#6c71c4", brightCyan: "#93a1a1", brightWhite: "#fdf6e3",
    background: "#002b36", foreground: "#93a1a1", boldText: "#eee8d5",
    selection: "#073642", cursor: "#93a1a1",
  },
  {
    accent: "#268bd2", panelBg: "#002b36", activeRowBg: "#164b57", selectionBg: "#083e55",
    surface0: "#073642", surface1: "#586e75", surfaceDim: "#002b36", overlay0: "#586e75",
    overlay1: "#657b83", text: "#93a1a1", subtext0: "#839496", mauve: "#d33682",
    green: "#859900", yellow: "#b58900", red: "#dc322f", blue: "#268bd2", teal: "#2aa198",
    peach: "#cb4b16",
  },
);

/** Solarized Light. */
export const SOLARIZED_LIGHT = herdr(
  "Solarized Light",
  "light",
  {
    black: "#073642", red: "#dc322f", green: "#859900", yellow: "#b58900",
    blue: "#268bd2", magenta: "#d33682", cyan: "#2aa198", white: "#eee8d5",
    brightBlack: "#002b36", brightRed: "#cb4b16", brightGreen: "#586e75", brightYellow: "#657b83",
    brightBlue: "#839496", brightMagenta: "#6c71c4", brightCyan: "#93a1a1", brightWhite: "#073642",
    background: "#fdf6e3", foreground: "#657b83", boldText: "#586e75",
    selection: "#eee8d5", cursor: "#586e75",
  },
  {
    accent: "#268bd2", panelBg: "#fdf6e3", activeRowBg: "#eee8d5", selectionBg: "#c9dcdf",
    surface0: "#eee8d5", surface1: "#93a1a1", surfaceDim: "#eee8d5", overlay0: "#93a1a1",
    overlay1: "#586e75", text: "#657b83", subtext0: "#839496", mauve: "#d33682",
    green: "#859900", yellow: "#b58900", red: "#dc322f", blue: "#268bd2", teal: "#2aa198",
    peach: "#cb4b16",
  },
);

/** Kanagawa (wave) — https://github.com/rebelot/kanagawa.nvim. */
export const KANAGAWA = herdr(
  "Kanagawa",
  "dark",
  {
    black: "#090618", red: "#c34043", green: "#76946a", yellow: "#c0a36e",
    blue: "#7e9cd8", magenta: "#957fb8", cyan: "#6a9589", white: "#c8c093",
    brightBlack: "#727169", brightRed: "#e82424", brightGreen: "#98bb6c", brightYellow: "#e6c384",
    brightBlue: "#7fb4ca", brightMagenta: "#938aa9", brightCyan: "#7aa89f", brightWhite: "#dcd7ba",
    background: "#1f1f28", foreground: "#dcd7ba", boldText: "#dcd7ba",
    selection: "#2d4f67", cursor: "#c8c093",
  },
  {
    accent: "#7e9cd8", panelBg: "#1f1f28", activeRowBg: "#363646", selectionBg: "#32384b",
    surface0: "#2a2a37", surface1: "#363646", surfaceDim: "#1f1f28", overlay0: "#727169",
    overlay1: "#87867d", text: "#dcd7ba", subtext0: "#c8c3aa", mauve: "#957fb8",
    green: "#76946a", yellow: "#c0a36e", red: "#c34043", blue: "#7e9cd8", teal: "#7fb4ca",
    peach: "#ffa066",
  },
);

/** Kanagawa Lotus — the light variant. */
export const KANAGAWA_LOTUS = herdr(
  "Kanagawa Lotus",
  "light",
  {
    black: "#1f1f28", red: "#c84053", green: "#6f894e", yellow: "#77713f",
    blue: "#4d699b", magenta: "#b35b79", cyan: "#597b75", white: "#545464",
    brightBlack: "#8a8980", brightRed: "#d7474b", brightGreen: "#6e915f", brightYellow: "#836f4a",
    brightBlue: "#6693bf", brightMagenta: "#624c83", brightCyan: "#5e857a", brightWhite: "#43436c",
    background: "#f2ecbc", foreground: "#545464", boldText: "#43436c",
    selection: "#c9cbd1", cursor: "#43436c",
  },
  {
    accent: "#4d699b", panelBg: "#f2ecbc", activeRowBg: "#d5cea3", selectionBg: "#dcd5ac",
    surface0: "#dcd5ac", surface1: "#c9cbd1", surfaceDim: "#d5cea3", overlay0: "#a09cac",
    overlay1: "#8a8980", text: "#545464", subtext0: "#43436c", mauve: "#624c83",
    green: "#6f894e", yellow: "#77713f", red: "#c84053", blue: "#4d699b", teal: "#4e8ca2",
    peach: "#cc6d00",
  },
);

/** Rosé Pine (main) — https://rosepinetheme.com/palette. */
export const ROSE_PINE = herdr(
  "Rosé Pine",
  "dark",
  {
    black: "#26233a", red: "#eb6f92", green: "#31748f", yellow: "#f6c177",
    blue: "#9ccfd8", magenta: "#c4a7e7", cyan: "#ebbcba", white: "#e0def4",
    brightBlack: "#6e6a86", brightRed: "#eb6f92", brightGreen: "#31748f", brightYellow: "#f6c177",
    brightBlue: "#9ccfd8", brightMagenta: "#c4a7e7", brightCyan: "#ebbcba", brightWhite: "#e0def4",
    background: "#191724", foreground: "#e0def4", boldText: "#e0def4",
    selection: "#403d52", cursor: "#e0def4",
  },
  {
    accent: "#c4a7e7", panelBg: "#191724", activeRowBg: "#26233a", selectionBg: "#3b344b",
    surface0: "#1f1d2e", surface1: "#26233a", surfaceDim: "#26233a", overlay0: "#6e6a86",
    overlay1: "#908caa", text: "#e0def4", subtext0: "#c8c5dc", mauve: "#c4a7e7",
    green: "#31748f", yellow: "#f6c177", red: "#eb6f92", blue: "#31748f", teal: "#9ccfd8",
    peach: "#ea9a97",
  },
);

/** Rosé Pine Dawn — the light variant. */
export const ROSE_PINE_DAWN = herdr(
  "Rosé Pine Dawn",
  "light",
  {
    black: "#f2e9e1", red: "#b4637a", green: "#286983", yellow: "#ea9d34",
    blue: "#56949f", magenta: "#907aa9", cyan: "#d7827e", white: "#575279",
    brightBlack: "#9893a5", brightRed: "#b4637a", brightGreen: "#286983", brightYellow: "#ea9d34",
    brightBlue: "#56949f", brightMagenta: "#907aa9", brightCyan: "#d7827e", brightWhite: "#575279",
    background: "#faf4ed", foreground: "#575279", boldText: "#464261",
    selection: "#dfdad9", cursor: "#575279",
  },
  {
    accent: "#907aa9", panelBg: "#faf4ed", activeRowBg: "#e3d9cf", selectionBg: "#f2e9e1",
    surface0: "#f2e9e1", surface1: "#fffaf3", surfaceDim: "#f2e9e1", overlay0: "#9893a5",
    overlay1: "#797593", text: "#464261", subtext0: "#797593", mauve: "#907aa9",
    green: "#286983", yellow: "#ea9d34", red: "#b4637a", blue: "#286983", teal: "#56949f",
    peach: "#d7827e",
  },
);

/** Vesper — https://github.com/raunofreiberg/vesper. */
export const VESPER = herdr(
  "Vesper",
  "dark",
  {
    black: "#101010", red: "#f5a191", green: "#90b99f", yellow: "#e6b99d",
    blue: "#aca1cf", magenta: "#e29eca", cyan: "#ea83a5", white: "#a0a0a0",
    brightBlack: "#7e7e7e", brightRed: "#ff8080", brightGreen: "#99ffe4", brightYellow: "#ffc799",
    brightBlue: "#b9aeda", brightMagenta: "#ecaad6", brightCyan: "#f591b2", brightWhite: "#ffffff",
    background: "#101010", foreground: "#ffffff", boldText: "#ffffff",
    selection: "#343434", cursor: "#ffc799",
  },
  {
    accent: "#ffc799", panelBg: "#1a1a1a", activeRowBg: "#101010", selectionBg: "#232323",
    surface0: "#232323", surface1: "#282828", surfaceDim: "#101010", overlay0: "#5c5c5c",
    overlay1: "#7e7e7e", text: "#ffffff", subtext0: "#a0a0a0", mauve: "#ffd1a8",
    green: "#99ffe4", yellow: "#ffc799", red: "#ff8080", blue: "#b0b0b0", teal: "#66ddcc",
    peach: "#ffc799",
  },
);

/** The theme a first run (and an unknown or deleted name) lands on. */
export const DEFAULT_THEME = CATPPUCCIN_MOCHA;
/** Auto-switch defaults, as herdr ships them. */
export const DEFAULT_DARK_THEME = CATPPUCCIN_MOCHA.name;
export const DEFAULT_LIGHT_THEME = CATPPUCCIN_LATTE.name;

/**
 * The built-ins, in switcher order — herdr's order, with SIGNAL ROOM last.
 *
 * Code-defined so a missing or corrupt `theme.toml` rebuilds them; the file only
 * holds user themes plus which one is active (see `src-tauri/src/theme.rs`,
 * whose `BUILT_IN_NAMES` must list exactly these names).
 */
export const BUILT_INS: readonly Theme[] = [
  CATPPUCCIN_MOCHA,
  CATPPUCCIN_LATTE,
  TOKYO_NIGHT,
  TOKYO_NIGHT_DAY,
  DRACULA,
  NORD,
  GRUVBOX_DARK,
  GRUVBOX_LIGHT,
  ONE_DARK,
  ONE_LIGHT,
  SOLARIZED_DARK,
  SOLARIZED_LIGHT,
  KANAGAWA,
  KANAGAWA_LOTUS,
  ROSE_PINE,
  ROSE_PINE_DAWN,
  VESPER,
  SIGNAL_ROOM,
];

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

/** Shift by a flat sRGB step (negative darkens). */
function lighten(hex: string, step: number): string {
  const c = parseHex(hex) ?? { r: 0, g: 0, b: 0 };
  return toHex({ r: c.r + step, g: c.g + step, b: c.b + step });
}

/**
 * The elevation step: away from the background, toward the text. On a dark
 * theme that lightens (SIGNAL ROOM's #060606 → #0a0a0a → … is exactly +4/+8/+12);
 * on a light theme lightening saturates at white and the ramp vanishes, so it
 * darkens instead.
 */
function elevate(hex: string, step: number, light: boolean): string {
  return lighten(hex, light ? -step : step);
}

/**
 * Mix two hex colours in sRGB — what the browser's `color-mix(in srgb …)`
 * computes, so the check harness can predict it.
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

/** WCAG relative luminance of an opaque hex. */
export function luminance(hex: string): number {
  const c = parseHex(hex) ?? { r: 0, g: 0, b: 0 };
  const lin = (v: number) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
}

/** WCAG contrast ratio between two opaque hexes (1–21). */
export function contrast(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/**
 * The smallest move of `fg` toward black (on a light background) or white (on a
 * dark one) that clears `ratio` against `bg`. Returns `fg` untouched when it
 * already does — which is every built-in but a few light themes whose published
 * text sits just under the line (Solarized Light's base00 is 4.1:1 on base3).
 * A nudge of a few levels keeps the hue and buys a readable 9px label.
 */
export function ensureContrast(fg: string, bg: string, ratio: number): string {
  if (contrast(fg, bg) >= ratio) return fg;
  const target = luminance(bg) > 0.5 ? "#000000" : "#ffffff";
  for (let i = 1; i <= 100; i++) {
    const c = mix(target, fg, i / 100);
    if (contrast(c, bg) >= ratio) return c;
  }
  return target;
}

/** Light or dark — stated by the theme, else read off its background. */
export function isLight(t: Theme): boolean {
  if (t.appearance) return t.appearance === "light";
  return luminance(t.background) > 0.4;
}

/* ------------------------------------------------------------- derivation */

/**
 * The nineteen UI slots for any theme: the stated ones as-is, the rest derived
 * from the 23 colours. The derivation is chosen so a slot-less theme (SIGNAL
 * ROOM, or one saved by an older build) re-creates the tokens it always had:
 * the panel is the old `--app-panel` (+4), the elevated surface the old
 * `--app-elev` (+12), and the act/working roles feed red/yellow so the state
 * colours mean what they used to.
 */
export function slots(t: Theme): Slots {
  const light = isLight(t);
  const bg = t.background;
  const derived: Slots = {
    accent: t.accent,
    panelBg: elevate(bg, 4, light),
    sidebarBg: bg,
    activeRowBg: elevate(bg, 8, light),
    selectionBg: mix(t.selection, bg, 0.3),
    surface0: elevate(bg, 12, light),
    surface1: elevate(bg, 24, light),
    surfaceDim: elevate(bg, 6, light),
    overlay0: t.brightBlack,
    overlay1: mix(t.foreground, bg, 0.6),
    text: t.foreground,
    subtext0: mix(t.foreground, bg, 0.78),
    mauve: t.magenta,
    green: t.green,
    yellow: t.working,
    red: t.accent,
    blue: t.blue,
    teal: t.cyan,
    peach: mix(t.red, t.yellow, 0.5),
  };
  const out = { ...derived };
  for (const k of SLOT_KEYS) {
    const v = t.ui?.[k];
    if (v && parseHex(v)) out[k] = v;
  }
  return out;
}

/** The CSS custom-property name for a slot (`panelBg` → `--panel-bg`). */
export function slotVar(k: SlotKey): string {
  return `--${k.replace(/([A-Z])/g, "-$1").toLowerCase()}`;
}

/**
 * Turn a theme into the full design-token set.
 *
 * Returns CSS custom-property name → value. `applyTheme` writes these onto the
 * document root; every `var(--…)` in the app and in `signal-room.css` then
 * re-skins at once. The alpha and elevation constants are structural — they are
 * rmux's, not the operator's — so they stay fixed while the colour they ride on
 * comes from the theme.
 *
 * Two families come out:
 *  - **the herdr slots** (`--accent`, `--panel-bg`, … `--peach`, and the five
 *    agent-state colours `--st-*`) — what new chrome is written against;
 *  - **the legacy tokens** (`--primary`, `--app-panel`, `--text-soft`, …) —
 *    what every existing component already reads, now derived from the slots so
 *    they re-colour with no change at the call site.
 */
export function deriveTokens(t: Theme): Record<string, string> {
  const s = slots(t);
  const light = isLight(t);
  const hasUi = t.ui !== undefined;
  const bg = t.background;

  // Legibility floors on what the chrome actually paints text with. The stated
  // slot is used whenever it already clears the bar, so herdr's values pass
  // through unchanged except where a published palette sits just under it.
  const text = ensureContrast(s.text, bg, 4.5);
  const subtext0 = ensureContrast(s.subtext0, bg, 3);

  const tokens: Record<string, string> = {
    // -- herdr slots ---------------------------------------------------------
    "--accent": s.accent,
    "--accent-rgb": triplet(s.accent),
    "--panel-bg": s.panelBg,
    "--sidebar-bg": s.sidebarBg,
    "--active-row-bg": s.activeRowBg,
    "--selection-bg": s.selectionBg,
    "--surface0": s.surface0,
    "--surface1": s.surface1,
    "--surface-dim": s.surfaceDim,
    "--overlay0": s.overlay0,
    "--overlay1": s.overlay1,
    "--subtext0": subtext0,
    "--mauve": s.mauve,
    "--green": s.green,
    "--yellow": s.yellow,
    "--red": s.red,
    "--blue": s.blue,
    "--teal": s.teal,
    "--peach": s.peach,

    // Agent state, herdr's mapping: working = yellow, blocked (needs you) = red,
    // done (finished, unseen) = teal, idle = green, unknown = overlay0.
    "--st-working": s.yellow,
    "--st-blocked": s.red,
    "--st-done": s.teal,
    "--st-idle": s.green,
    "--st-unknown": s.overlay0,

    // -- legacy tokens -------------------------------------------------------
    // The "act" colour. Bare triplets: every use is `rgb(var(--primary) / a)`,
    // so a hex here would break every translucent accent at once.
    "--primary": triplet(t.accent),
    "--primary-soft": triplet(text),

    // Working. `--warn` is the same colour as `--busy`.
    "--busy": triplet(t.working),
    "--warn": t.working,

    // Background and its elevation ramp. For a slot-less theme this is exactly
    // the old +4/+8/+12; for a herdr theme the panel is herdr's panel_bg and the
    // raised surface its surface0.
    "--app-bg": bg,
    "--app-panel": s.panelBg,
    "--app-panel-2": hasUi ? mix(s.panelBg, s.surface0, 0.5) : elevate(bg, 8, light),
    "--app-elev": s.surface0,

    // Chrome that wants to sit *deeper* than the panel (the dock, the top bar).
    // A slot-less theme keeps its raw ANSI black, as before. A herdr theme gets
    // its panel a step darker — on a light theme ANSI black is near-black text
    // colour, and a top bar painted with it would be a black stripe.
    "--ansi-black": hasUi ? lighten(s.panelBg, -6) : t.black,

    // Borders and hover: the text at fixed alphas, so they stay hairlines over
    // glass and over a painted backdrop alike.
    "--border": alpha(text, 0.14),
    "--border-strong": alpha(text, 0.24),
    "--hover": alpha(text, 0.055),

    // A recessed well (inputs, code blocks): a darkening on dark themes, a faint
    // text-coloured wash on light ones, where black at 35% would be a grey brick.
    "--well": light ? alpha(text, 0.06) : "rgba(0, 0, 0, 0.35)",

    // Scrollbar thumb.
    "--scroll-thumb": hasUi ? s.surface0 : elevate(bg, 30, light),
    "--scroll-thumb-hover": hasUi ? s.surface1 : elevate(bg, 40, light),

    "--text": text,
    "--text-bright": t.boldText,

    // Text on an accent-filled control. A slot-less theme keeps its text (the
    // old red-hover button); a herdr theme takes whichever of its background or
    // text reads better on the accent — dark on Mocha's blue, light on Latte's.
    "--on-accent": hasUi
      ? contrast(bg, s.accent) >= contrast(text, s.accent)
        ? bg
        : text
      : text,

    // Corner radius: herdr's softer chrome, except SIGNAL ROOM's square edges.
    "--radius": `${Math.max(0, t.radius ?? 4)}px`,
  };

  if (hasUi) {
    // The three-level ramp from herdr's slots, floored for legibility. `faint`
    // is what the 9px `.micro` captions wear, so it takes the 4.5:1 that normal
    // text needs (the same floor SIGNAL ROOM's #7e7b74 was raised to); `soft`
    // stays a visible step above it at 5.5:1. Measured on Latte, overlay1 at its
    // published 3.0:1 made every rail caption read as disabled.
    tokens["--text-soft"] = ensureContrast(s.subtext0, bg, 5.5);
    tokens["--text-faint"] = ensureContrast(s.overlay1, bg, 4.5);
  } else {
    // A slot-less theme keeps the original derivation exactly — a real
    // `color-mix` the browser computes (`ui/theme-check.html` measures it).
    tokens["--text-soft"] = `color-mix(in srgb, ${text} 64%, ${bg})`;
    tokens["--text-faint"] = `color-mix(in srgb, ${text} 52%, ${bg})`;
  }
  return tokens;
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
 *  - `selection` is the Selection slot at 0.30 for a slot-less theme (SIGNAL
 *    ROOM's Selection is the act red, so this reproduces the shipped
 *    `rgba(230,59,46,0.30)`). A herdr-family theme's Selection is its published
 *    *neutral* selection colour, which at 0.30 over a transparent pane is barely
 *    there, so it is laid down at 0.70 instead.
 */
export function terminalTheme(t: Theme): Record<string, string> {
  return {
    background: "rgba(0, 0, 0, 0)",
    foreground: t.foreground,
    cursor: t.cursor,
    // The glyph *under* a block cursor — the panel shade, not the pure bg, so it
    // reads against the cursor fill (shipped `#0a0a0a`).
    cursorAccent: elevate(t.background, 4, isLight(t)),
    selectionBackground: alpha(t.selection, t.ui ? 0.7 : 0.3),
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
  base: "vs-dark" | "vs";
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
 * keyword=magenta, string=green, number=yellow, type=cyan, function/key=blue,
 * comment=brightBlack, and errors / unmatched brackets are the act colour. So a
 * string is the same green in the editor, a transcript and a shell, and it
 * re-skins with the terminal. The caret is the theme's UI accent (SIGNAL ROOM's
 * is its red, as it always was). Selection and bracket-nesting colours stay
 * *neutral* (foreground at low alpha), never the Selection slot: Monaco derives
 * a red selection block from a coloured one, which reads as an error on every
 * line the caret visits. Light themes build on Monaco's `vs` base.
 */
export function monacoTheme(t: Theme): MonacoThemeData {
  const light = isLight(t);
  const fg = t.foreground;
  const soft = mix(fg, t.background, 0.64); // == --text-soft
  const param = mix(fg, t.background, 0.82); // variable.parameter
  const op = mix(fg, t.background, 0.72); // operator
  const meta = mix(t.brightBlack, fg, 0.6); // comment.doc / meta
  const caret = slots(t).accent;
  const widget = elevate(t.background, 8, light);

  return {
    base: light ? "vs" : "vs-dark",
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
      "editorCursor.foreground": caret,
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
      "editorWidget.background": widget,
      "editorWidget.border": withA(fg, "24"),
      "editorSuggestWidget.background": widget,
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
      "scrollbarSlider.background": withA(elevate(t.background, 30, light), "99"),
      "scrollbarSlider.hoverBackground": withA(elevate(t.background, 40, light), "cc"),
      "scrollbarSlider.activeBackground": elevate(t.background, 40, light),
    },
  };
}
