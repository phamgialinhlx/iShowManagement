/**
 * One palette for every terminal in rmux.
 *
 * Shared because the two panes had drifted: the Claude pane defined only
 * `background`, `foreground` and `cursor`, so xterm filled the other fourteen
 * slots from *its* defaults — including a pure `#000000` black. That is
 * invisible in an opaque terminal and glaring in a transparent one.
 *
 * ## The dark slots are deliberately translucent
 *
 * This is the part that matters now the terminals are glass. Claude's TUI
 * paints an explicit background behind quoted text, code and its status line —
 * a subtle plate that, against a solid dark terminal, reads as barely-there
 * shading. Make the terminal transparent and that same plate becomes an opaque
 * black brick sitting on the wallpaper, boxed around every styled word.
 *
 * The plate is not a mistake to remove, though: it is how Claude separates
 * quoted material from its own prose. So `black` keeps doing its job at a
 * fraction of the alpha, which reads as the shading it was always meant to be
 * and lets the desktop through.
 *
 * `brightBlack` stays opaque, because it is used as a *foreground* for dimmed
 * text far more than as a background — fading it would make Claude's own
 * asides unreadable rather than subtle.
 */
export const TERMINAL_THEME = {
  /** Fully clear: the panel behind provides the tint. */
  background: "rgba(0, 0, 0, 0)",
  foreground: "#e8f4f2",
  cursor: "#e8e6e1",
  cursorAccent: "#0a0a0a",
  selectionBackground: "rgba(230, 59, 46, 0.30)",

  // See the note above — these two are the ones transparency exposes.
  black: "rgba(10, 10, 10, 0.42)",
  brightBlack: "#7e7b74",

  red: "#ff6b6b",
  green: "#5ef2b0",
  yellow: "#ffd166",
  blue: "#54b6ff",
  magenta: "#c792ff",
  cyan: "#54e6ff",
  white: "#e8e6e1",
  brightRed: "#ff8b8b",
  brightGreen: "#7ef5c4",
  brightYellow: "#ffdd8a",
  brightBlue: "#7cc7ff",
  brightMagenta: "#d9b0ff",
  brightCyan: "#8aefff",
  brightWhite: "#ffffff",
} as const;

/** The Claude pane differs in one thing only: red, because that is its cursor. */
export const CLAUDE_THEME = { ...TERMINAL_THEME, cursor: "#e63b2e" } as const;

/**
 * Whether terminals render on the GPU.
 *
 * A setting rather than a constant, because it is the *only* lever over a class
 * of rendering artefact we do not otherwise control. xterm's WebGL renderer
 * keeps its own glyph atlas, and a glyph cached with a background attribute can
 * come back with that background painted opaque — which on a glass terminal
 * reads as a black box around each styled word, with gaps where the spaces are.
 * The DOM renderer composites text normally and does not do this.
 *
 * **On by default and it should stay that way.** Without WebGL, xterm falls
 * back to the DOM renderer, and scrolling or dragging across a constantly
 * redrawing TUI is visibly slow — that was a real bug in this pane once. Turn it
 * off only if the artefact bothers you more than the latency does.
 */
const GPU_KEY = "rmux.terminal.gpu";

export function gpuRendering(): boolean {
  return localStorage.getItem(GPU_KEY) !== "0";
}

export function setGpuRendering(on: boolean): void {
  localStorage.setItem(GPU_KEY, on ? "1" : "0");
}
