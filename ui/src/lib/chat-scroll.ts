/**
 * When the mobile conversation may move, and when it must not.
 *
 * The rule is CLAUDE.md's "nothing moves under the operator's hands". A re-read
 * of the tail does not merely append: once the transcript outgrows the tail
 * window, turns drop off the *front*, so applying it shifts everything above the
 * reader. So an update is applied only when the reader is at the bottom (and then
 * the view follows), held behind a "new messages" pill when they have scrolled
 * up, and not even fetched while they are selecting text.
 *
 *     node --experimental-strip-types ui/mobile-check.ts
 */

/** Within this many px of the end counts as "at the bottom". */
export const NEAR_BOTTOM_PX = 48;

export type ScrollMetrics = { scrollHeight: number; scrollTop: number; clientHeight: number };

export function isNearBottom(m: ScrollMetrics, px: number = NEAR_BOTTOM_PX): boolean {
  return m.scrollHeight - m.scrollTop - m.clientHeight <= px;
}

/**
 * What to do with a re-read that brought something new.
 *
 * - `apply-follow`: the reader is at the bottom — show it and stay at the end.
 * - `hold-pill`: they scrolled up — keep the screen as it is, offer the pill.
 * - `hold`: they are selecting — change nothing, say nothing new; the update is
 *   taken once the selection clears.
 */
export type UpdatePlan = "apply-follow" | "hold-pill" | "hold";

export function planUpdate(s: { atBottom: boolean; selecting: boolean }): UpdatePlan {
  if (s.selecting) return "hold";
  return s.atBottom ? "apply-follow" : "hold-pill";
}

/** May a refresh be *fetched* now? Not while text is selected (the fetch's
 *  re-render would wipe the selection) and not while the page is hidden. */
export function mayFetch(s: { selecting: boolean; hidden: boolean }): boolean {
  return !s.selecting && !s.hidden;
}
