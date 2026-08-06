/**
 * The font registry (ADR-003).
 *
 * rmux lets the operator pick a **UI font** and a **Mono font** from a fixed,
 * bundled set — not system enumeration (WKWebView cannot list installed fonts)
 * and not file upload. Every option ships inside the app, so a choice renders
 * identically on every machine and with no network, the same invariant
 * `fonts.css` already enforces for the two defaults.
 *
 * The split is load-bearing: the terminal, the Claude TUI, the code editor and
 * every column-aligned metric readout *must* be monospace, so the two roles are
 * two separate pickers and a proportional face can never reach the terminal.
 *
 * A stored value is a stable **id** (`"jetbrains-mono"`), never a raw CSS stack.
 * This module is the one place that knows a font exists: the picker renders from
 * `UI_FONTS`/`MONO_FONTS`, `applyFonts` resolves ids to stacks and writes the
 * tokens, and the xterm/Monaco hosts read the applied stack back with
 * `readMonoStack`. Storing an id means a font's fallbacks can be tuned without
 * rewriting everyone's saved settings.
 */

import { THEME_EVENT } from "./theme-runtime";

export type FontRole = "ui" | "mono";

/**
 * How a face is provided. `fontsource` — a bundled npm package (`fonts.css`);
 * `bundled` — a raw `@font-face` over files in `public/fonts`; `system` — a
 * native face named by CSS generics, no bytes added and not identical across
 * OSes (the escape hatch for anyone who prefers their machine's own font).
 */
type Provider = "fontsource" | "bundled" | "system";

export type FontDef = {
  id: string;
  label: string;
  role: FontRole;
  provider: Provider;
  /** The full CSS `font-family` value, generic fallbacks included. */
  stack: string;
};

// The same generic tails the shipped tokens carry (`signal-room.css`), so an
// unbundled face still lands on a sensible native fallback.
//
// **Consolas and Cascadia Mono are in the tail for Windows**, and their absence
// was not cosmetic: `ui-monospace` resolves to nothing there, and `SF Mono` and
// `Menlo` are macOS faces, so the whole tail fell through to the generic
// `monospace` — which on Windows is **Courier New**, a thin serif face at the
// wrong weight for a terminal. Every stack ends in this, so it was the fallback
// behind all six mono options, not just the system one.
const MONO_TAIL = `ui-monospace, "SF Mono", Menlo, Consolas, "Cascadia Mono", monospace`;
const UI_TAIL = `ui-sans-serif, system-ui, sans-serif`;

export const DEFAULT_UI_FONT = "sfu-futura";
export const DEFAULT_MONO_FONT = "ibm-plex-mono";

/** What the machine's own monospace face is actually called, for the label. */
function systemMonoLabel(): string {
  const platform =
    typeof navigator === "undefined" ? "" : navigator.platform || navigator.userAgent;
  if (/mac/i.test(platform)) return "SF Mono / Menlo";
  if (/win/i.test(platform)) return "Consolas";
  return "System Mono";
}

export const MONO_FONTS: FontDef[] = [
  { id: "ibm-plex-mono", label: "IBM Plex Mono", role: "mono", provider: "fontsource", stack: `"IBM Plex Mono", ${MONO_TAIL}` },
  { id: "jetbrains-mono", label: "JetBrains Mono", role: "mono", provider: "fontsource", stack: `"JetBrains Mono", ${MONO_TAIL}` },
  { id: "cascadia-code", label: "Cascadia Code", role: "mono", provider: "fontsource", stack: `"Cascadia Code", ${MONO_TAIL}` },
  { id: "fira-code", label: "Fira Code", role: "mono", provider: "fontsource", stack: `"Fira Code", ${MONO_TAIL}` },
  // Zed's editor font — its `.ZedMono` alias resolves to Lilex. Bundled raw from
  // the Zed source tree; the "Zed" hint in the label is what makes it findable.
  { id: "lilex", label: "Lilex · Zed", role: "mono", provider: "bundled", stack: `"Lilex", ${MONO_TAIL}` },
  // No bundled bytes — resolves to whatever the OS calls its monospace face.
  // The label names the face the operator will actually get, so it has to be
  // read at runtime: "SF Mono / Menlo" is a promise Windows cannot keep, and a
  // picker offering a font the machine does not have is the "control that
  // cannot work" rule in miniature.
  {
    id: "system-mono",
    label: systemMonoLabel(),
    role: "mono",
    provider: "system",
    stack: MONO_TAIL,
  },
];

export const UI_FONTS: FontDef[] = [
  { id: "sfu-futura", label: "SFU Futura", role: "ui", provider: "bundled", stack: `"SFU Futura", "Space Grotesk", ${UI_TAIL}` },
  { id: "inter", label: "Inter", role: "ui", provider: "fontsource", stack: `"Inter", ${UI_TAIL}` },
  // Zed's UI font *is* IBM Plex Sans (its `.ZedSans` alias) — already bundled via
  // @fontsource, so this one entry is both "IBM Plex Sans" and "the Zed UI look".
  { id: "ibm-plex-sans", label: "IBM Plex Sans · Zed", role: "ui", provider: "fontsource", stack: `"IBM Plex Sans", ${UI_TAIL}` },
  // No bundled bytes — resolves to the OS UI face (SF Pro, Segoe, …).
  { id: "system-ui", label: "System UI", role: "ui", provider: "system", stack: UI_TAIL },
];

/** Resolve an id to its definition, falling back to the role's default for an
 *  unknown/old id so a stale stored value can never leave the app fontless. */
export function resolveFont(id: string, role: FontRole): FontDef {
  const list = role === "ui" ? UI_FONTS : MONO_FONTS;
  const fallback = role === "ui" ? DEFAULT_UI_FONT : DEFAULT_MONO_FONT;
  return list.find((f) => f.id === id) ?? list.find((f) => f.id === fallback)!;
}

/**
 * The mono stack currently applied to the document — what the xterm hosts and
 * Monaco read at construction and on every restyle.
 *
 * Reads the live `--font-mono` token rather than `localStorage`, so it reflects
 * a *staged preview* exactly as it reflects a committed value (a preview writes
 * the token but not storage). Falls back to the default stack in a non-DOM
 * context or before the first apply.
 */
export function readMonoStack(): string {
  const fallback = resolveFont(DEFAULT_MONO_FONT, "mono").stack;
  if (typeof document === "undefined") return fallback;
  const v = getComputedStyle(document.documentElement).getPropertyValue("--font-mono").trim();
  return v || fallback;
}

/**
 * Tell xterm/Monaco the fonts changed. They cache the family at construction and
 * do not watch CSS, so this event (their `rmux-theme` handlers restyle colour
 * *and* font) is what makes them re-read; the chrome and the metric widgets read
 * the tokens straight from CSS and follow for free.
 */
function notifyFontChange(): void {
  window.dispatchEvent(new Event(THEME_EVENT));
}

/** Write only the UI font onto the document root. One axis, so a UI-font pick
 *  can never disturb the mono choice (they are set from independent state). */
export function applyUiFont(uiFont: string): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  root.style.setProperty("--font-display", resolveFont(uiFont, "ui").stack);
  root.style.setProperty("--font-body", "var(--font-display)");
  notifyFontChange();
}

/** Write only the mono font onto the document root. */
export function applyMonoFont(monoFont: string): void {
  if (typeof document === "undefined") return;
  document.documentElement.style.setProperty("--font-mono", resolveFont(monoFont, "mono").stack);
  notifyFontChange();
}

/**
 * Apply both font choices — used by `applyAppearance` on startup, commit and the
 * cross-window `storage` sync, where the whole `Appearance` is authoritative.
 *
 * Fonts are one of the two **live-preview** axes (colour is the other): picking a
 * chip repaints immediately without persisting. The *preview* path uses the
 * single-axis setters above rather than this one, so staging one role never
 * drags the other — or a staged scale change — along with it.
 */
export function applyFonts(uiFont: string, monoFont: string): void {
  applyUiFont(uiFont);
  applyMonoFont(monoFont);
}
