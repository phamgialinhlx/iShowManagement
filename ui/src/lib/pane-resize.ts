/**
 * Dragging a divider, in a window that is `zoom`ed and a pane that is not at the
 * window's edge.
 *
 * ## The bug this exists for
 *
 * The Files divider computed its new width as `e.clientX - 220`. Both halves of
 * that are wrong, and together they make the control look broken rather than
 * inaccurate:
 *
 * - **`- 220` is a guess at where the pane starts.** It was presumably the
 *   session rail's width on the day it was typed. But a session is frequently a
 *   *cell in a grid* — the whole point of the workbench — and the top-right cell
 *   of a 2x2 starts halfway across the window. `clientX - 220` is then several
 *   hundred pixels too large, so it clamps to the maximum on the first pointer
 *   move and stays pinned there however far you drag. The operator's report was
 *   "I can't drag this", and that is exactly right: a control that snaps to one
 *   end and then ignores you is not draggable.
 * - **`clientX` is in viewport pixels; the width is written into zoomed space.**
 *   `#root` carries `zoom: var(--ui-zoom)`, so a value assigned to `style.width`
 *   is multiplied by the scale before it lands on screen. At 125% the pane then
 *   moved 1.25px for every pixel of pointer travel and slid out from under the
 *   cursor. This is the same trap the context menu hit, and it is measured the
 *   same way rather than read from the CSS variable — one source of truth, and
 *   it stays right if anything else ever scales a subtree.
 *
 * Keeping the arithmetic here rather than inline in the component is what lets
 * `ui/resize-check.html` drive it against a real zoomed element, which is the
 * only way to prove the second half.
 */

/** The tree may not become a strip of ellipses, nor eat the editor. */
export const TREE_MIN = 150;
export const TREE_MAX = 560;
export const TREE_DEFAULT = 260;

/**
 * How many device-ish pixels one layout pixel of this element occupies.
 *
 * **Measured, never read from `--ui-zoom`.** `getBoundingClientRect` reports the
 * post-zoom box while `offsetWidth` reports the pre-zoom one, so their ratio is
 * the scale actually in force — including any zoom an ancestor applies that this
 * code does not know about. Reading the variable would be trusting a second copy
 * of the fact.
 *
 * Falls back to 1 for an unlaid-out or hidden element (`offsetWidth === 0`),
 * because dividing by zero here would set the pane's width to `NaN` and blank the
 * whole view.
 */
export function measureScale(el: HTMLElement): number {
  const width = el.offsetWidth;
  if (!width) return 1;
  const scale = el.getBoundingClientRect().width / width;
  return Number.isFinite(scale) && scale > 0 ? scale : 1;
}

/**
 * The tree width a pointer at `clientX` is asking for.
 *
 * `left` is the pane's own left edge from `getBoundingClientRect` — in the same
 * viewport pixels as `clientX`, which is what makes the subtraction meaningful
 * wherever the pane happens to sit. The division into layout pixels happens
 * once, after it.
 */
export function widthFromPointer(
  clientX: number,
  left: number,
  scale: number,
  min = TREE_MIN,
  max = TREE_MAX,
): number {
  const layout = (clientX - left) / scale;
  return Math.round(Math.min(Math.max(layout, min), max));
}
