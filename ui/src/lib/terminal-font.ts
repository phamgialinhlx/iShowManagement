/**
 * The terminal's font size, honouring TEXT SIZE.
 *
 * **Applied through xterm's own option, never through CSS.** That distinction
 * is the whole lesson of the interface-scale removal: xterm computes its cell
 * grid from the size it is *given*, then maps a mouse position against that
 * grid. Change the rendered size underneath it — with `zoom`, or with a CSS
 * `font-size` — and the grid it draws stops matching the grid it measures, so
 * clicks land on the wrong row and the error grows down the pane.
 *
 * Setting `options.fontSize` instead makes xterm re-measure, so the two stay
 * the same thing. It must be followed by a `fit()`, because the cell size
 * changed and the pty's dimensions are now wrong.
 */

/** What the operator chose, from the live token. 1 when unset. */
export function fontScale(): number {
  if (typeof window === "undefined") return 1;
  const raw = getComputedStyle(document.documentElement).getPropertyValue("--font-scale").trim();
  const n = Number(raw);
  // Clamped to the slider's range: a corrupt token must not produce a
  // one-pixel or thousand-pixel cell grid.
  return Number.isFinite(n) && n > 0 ? Math.min(2, Math.max(0.5, n)) : 1;
}

/** A base size, scaled and rounded to a whole pixel. */
export function scaledFontSize(base: number): number {
  // Whole pixels: a fractional cell width accumulates rounding across eighty
  // columns and leaves a ragged right edge.
  return Math.max(6, Math.round(base * fontScale()));
}
