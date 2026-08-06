/**
 * Telling a remote program its new size — once the size has stopped changing.
 *
 * ## The bug this exists for
 *
 * `ResizeObserver` fires continuously while a window is dragged. Both xterm
 * hosts used to refit and send the new dimensions on *every* callback, so a
 * two-second drag told the far side about forty different widths. A TUI redraws
 * itself completely on each one — and because rmux runs Claude **inline** rather
 * than on the alternate screen, every one of those redraws stays in the
 * scrollback. The result on screen is the same question printed four or five
 * times at four or five widths, which reads as the app having lost its mind.
 * Reported exactly that way: "it look weird like this, have repeated render".
 *
 * ## Why the local fit is not debounced with it
 *
 * xterm still refits promptly, so the visible grid tracks the window and the
 * drag feels attached to the pointer. Only the *message to the far side* waits.
 * Debouncing both would make the terminal lag behind its own frame, which trades
 * one visible problem for another.
 *
 * The local fit **is throttled**, though. A `fit()` reflows the whole cell
 * grid, and an animated layout change — the rail collapsing under a spring —
 * fires the observer every frame for every mounted pane: in a 4×4 grid that
 * was 16 reflows per frame for the length of the animation. The first callback
 * fits immediately (a single-step resize feels instant); a continuous stream
 * fits at most every `FIT_MS`; and the settle always ends with a final fit, so
 * the grid lands exactly right no matter what was skipped in between.
 *
 * The delay is short enough to feel instant when you let go of the mouse and
 * long enough to cover the gap between two `ResizeObserver` callbacks mid-drag.
 */
const SETTLE_MS = 120;

/** Minimum spacing between local refits during a continuous resize. */
const FIT_MS = 80;

export type Settle = {
  /** Fit now; tell the far side once the size stops changing. */
  observe: () => void;
  /** Send immediately — for a resize that is already known to be final. */
  flush: () => void;
  dispose: () => void;
};

/**
 * @param fit      refit the local grid. Runs on every call.
 * @param notify   send the settled size. Runs once the size stops changing.
 */
export function settleResize(fit: () => void, notify: () => void): Settle {
  let timer: number | null = null;
  let lastFit = 0;

  const clear = () => {
    if (timer !== null) window.clearTimeout(timer);
    timer = null;
  };

  return {
    observe() {
      const now = performance.now();
      if (now - lastFit >= FIT_MS) {
        lastFit = now;
        fit();
      }
      clear();
      timer = window.setTimeout(() => {
        timer = null;
        // The trailing fit makes the throttle safe: whatever callbacks were
        // skipped, the grid ends at the true final size before the far side
        // hears about it.
        fit();
        notify();
      }, SETTLE_MS);
    },
    flush() {
      clear();
      fit();
      notify();
    },
    dispose: clear,
  };
}
