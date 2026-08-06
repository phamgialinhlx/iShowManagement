import { terminalTheme as themeTerminal, claudeTheme as themeClaude } from "./theme";
import { activeTheme } from "./theme-runtime";

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
/**
 * The palette, resolved against the *active theme*.
 *
 * These used to read design tokens off the DOM. Now the ANSI theme system is the
 * source of truth (`lib/theme.ts` + `lib/theme-runtime.ts`), so both delegate to
 * the pure derivation applied to whichever theme is active — the terminal and the
 * chrome finally agree on every colour, and switching a theme re-skins them
 * together. The no-arg shape is kept so the xterm hosts do not change their call
 * sites; they re-read on the `rmux-theme` event when the active theme changes.
 */
export function terminalTheme(): Record<string, string> {
  return themeTerminal(activeTheme());
}

/** The Claude pane now matches the terminal exactly, cursor included. */
export function claudeTheme(): Record<string, string> {
  return themeClaude(activeTheme());
}

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

/** Kept for callers that want the static defaults; the live one is `claudeTheme()`.
 *  Identical to `TERMINAL_THEME` now — the Claude pane no longer reddens its cursor. */
export const CLAUDE_THEME = { ...TERMINAL_THEME } as const;

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

/**
 * The interface scale in force — what AppearancePanel wrote to `--ui-zoom`.
 *
 * The terminal hosts counter-zoom themselves (`.term-device-px` in
 * signal-room.css) so the xterm canvas rasterises at device resolution instead
 * of being scaled up as a bitmap, which blurred every glyph at any scale but
 * 100%. The font size must then be multiplied back, or raising the interface
 * scale would visibly *shrink* every terminal.
 */
export function uiZoom(): number {
  const raw = getComputedStyle(document.documentElement).getPropertyValue("--ui-zoom");
  const z = Number.parseFloat(raw);
  return Number.isFinite(z) && z > 0 ? z : 1;
}

/** A terminal font size compensated for the counter-zoom, in whole pixels —
 *  a fractional size defeats the point by landing glyphs between pixels. */
export function termFontSize(base: number): number {
  return Math.max(8, Math.round(base * uiZoom()));
}
