/**
 * Resolved theme colours for the *canvas* widgets.
 *
 * Most of the app re-skins for free: 500-odd inline styles use `var(--…)`, which
 * follows the active theme the moment `deriveTokens` writes the tokens. The
 * metrics widgets are the exception — they paint to a `<canvas>`, and
 * `ctx.fillStyle` cannot take a `var()`. So they read the *resolved* value here
 * instead of hard-coding a hex, which is what makes a gauge follow the theme
 * rather than staying SIGNAL-ROOM red under Nord.
 *
 * Read at draw time. These widgets redraw on every sample and also on the
 * `rmux-theme` event (see `useThemeRedraw`), so a switch lands within a frame.
 */

import { useEffect, useState } from "react";

import { parseHex } from "./theme";

/**
 * A counter that bumps on every theme change. Canvas widgets put it in their
 * draw effect's dependency list so a switch repaints them at once rather than on
 * their next poll (up to a couple of seconds away).
 */
export function useThemeVersion(): number {
  const [v, setV] = useState(0);
  useEffect(() => {
    const bump = () => setV((n) => n + 1);
    window.addEventListener("rmux-theme", bump);
    return () => window.removeEventListener("rmux-theme", bump);
  }, []);
  return v;
}

function cssVar(name: string, fallback: string): string {
  if (typeof document === "undefined") return fallback;
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

/** A colour token resolved to a canvas-usable string (e.g. `--text` → `#e8e6e1`). */
export function paint(name: string, fallback = "#e8e6e1"): string {
  return cssVar(name, fallback);
}

/** The `--primary` triplet as `rgba(r,g,b,a)` — the "you must act" accent. */
export function accent(a = 1): string {
  const [r = 230, g = 59, b = 46] = cssVar("--primary", "230 59 46").split(/\s+/).map(Number);
  return `rgba(${r},${g},${b},${a})`;
}

/** A hex colour token at an alpha, as `rgba()` for canvas. */
export function tokenAlpha(name: string, a: number, fallback = "#e8e6e1"): string {
  const c = parseHex(cssVar(name, fallback)) ?? { r: 232, g: 230, b: 225 };
  return `rgba(${c.r},${c.g},${c.b},${a})`;
}

/**
 * A monochrome ramp of `steps` colours from `--text` down toward `--app-bg`, for
 * widgets that shade rows by rank (top processes). Follows the theme because both
 * ends are resolved tokens.
 */
export function textRamp(steps: number): string[] {
  const text = parseHex(cssVar("--text", "#e8e6e1")) ?? { r: 232, g: 230, b: 225 };
  const bg = parseHex(cssVar("--app-bg", "#060606")) ?? { r: 6, g: 6, b: 6 };
  const out: string[] = [];
  for (let i = 0; i < steps; i++) {
    // 0 → text, last → ~40% of the way to the background (still legible, never bg).
    const w = 1 - (i / Math.max(1, steps - 1)) * 0.72;
    const r = Math.round(text.r * w + bg.r * (1 - w));
    const g = Math.round(text.g * w + bg.g * (1 - w));
    const b = Math.round(text.b * w + bg.b * (1 - w));
    out.push(`rgb(${r},${g},${b})`);
  }
  return out;
}
