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

/**
 * Re-measure a freshly-opened terminal once its font has finished loading.
 *
 * xterm measures its cell grid at construction. The bundled mono faces load
 * asynchronously and `font-display: block` means xterm measures against the
 * *fallback's* metrics during the block period — so when the real face swaps in,
 * its glyphs no longer fit the cells and text overlaps or eats its own spaces
 * (the "TUI looks garbled" report). `document.fonts.ready` resolves once the
 * document's font loads settle; re-setting `fontFamily` invalidates xterm's
 * char-size cache — the same re-measure `scaledFontSize` documents — and
 * `fit()` re-lays the grid to match, after which the pty is told the new size by
 * the caller's normal resize path.
 *
 * Cheap when the font was already loaded: the promise resolves at once and the
 * remeasure is a single pass. `isAlive` guards a terminal disposed before the
 * font settled.
 */
export function remeasureOnFontLoad(
  xterm: { options: { fontFamily?: string } },
  fit: { fit: () => void },
  fontFamily: () => string,
  isAlive: () => boolean,
): void {
  const fonts = typeof document !== "undefined" ? document.fonts : undefined;
  if (!fonts?.ready) return;
  void fonts.ready
    .then(() => {
      if (!isAlive()) return;
      // Setting fontFamily (even to the same value) forces xterm to re-measure.
      xterm.options.fontFamily = fontFamily();
      try {
        fit.fit();
      } catch {
        // A pane still laying out measures 0x0; its next resize refits.
      }
    })
    .catch(() => {
      // `document.fonts.ready` does not reject in practice; swallow to be safe.
    });
}
