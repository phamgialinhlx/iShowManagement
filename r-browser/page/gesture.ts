/**
 * Whether a mouse event belongs to a pick that is already under way.
 *
 * Pulled out of the picker as a pure function for one reason: the bug it
 * encodes was invisible in place and is obvious here. On the single-pick path
 * the picker disarms on `pointerdown`, so by the time `mouseup` and `click`
 * arrive `active` is already false — and a guard written as "swallow while
 * active" let both through to the page. Picking a link navigated, which files
 * the note against a URL the operator was never describing and loses the page
 * they were. Multi-pick hid it completely, because there the picker is still
 * armed when the later events land.
 *
 * A DOM cannot be simulated honestly, so the picker's *rendering* is not
 * testable here. This decision is, and it is the half that was wrong.
 */
export interface GestureState {
  /** Epoch ms after which a gesture is considered abandoned. `0` for none. */
  openUntil: number
}

/**
 * One second, because it has to bound a case that produces no closing event
 * at all: if the pointer leaves the window between down and up, `click` never
 * fires, and a latch with no deadline would swallow some unrelated click
 * minutes later. A real press-and-release is tens of milliseconds; the next
 * deliberate action is seconds away. Anything in between is safe.
 */
export const GESTURE_MS = 1000

export function openGesture(now: number): GestureState {
  return { openUntil: now + GESTURE_MS }
}

export function isOpen(state: GestureState, now: number): boolean {
  return now < state.openUntil
}

/**
 * Should this event be suppressed before the page sees it?
 *
 * `active || open` and not `active` alone — that disjunction is the fix.
 */
export function shouldSwallow(active: boolean, state: GestureState, now: number): boolean {
  return active || isOpen(state, now)
}

/** `click` is the last of the four, so the latch drops rather than expiring. */
export function afterEvent(type: string, state: GestureState): GestureState {
  return type === 'click' ? { openUntil: 0 } : state
}
