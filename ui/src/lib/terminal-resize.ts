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
 * xterm still refits immediately, so the visible grid tracks the window and the
 * drag feels attached to the pointer. Only the *message to the far side* waits.
 * Debouncing both would make the terminal lag behind its own frame, which trades
 * one visible problem for another.
 *
 * The delay is short enough to feel instant when you let go of the mouse and
 * long enough to cover the gap between two `ResizeObserver` callbacks mid-drag.
 */
const SETTLE_MS = 120;

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

  const clear = () => {
    if (timer !== null) window.clearTimeout(timer);
    timer = null;
  };

  return {
    observe() {
      fit();
      clear();
      timer = window.setTimeout(() => {
        timer = null;
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
