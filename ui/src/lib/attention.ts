import { recordSeconds } from "./activity";
import { useWorkspace } from "./workspace";

/**
 * How long the operator actually spent on each session.
 *
 * ## Why this is one watcher, not a hook in each pane
 *
 * A per-pane timer would count every *mounted* pane, and in a 2x2 that is four
 * sessions accruing time at once — so "time spent" would exceed wall-clock by
 * the size of the grid and a 1x4 deck would inflate it fourfold. Attention is
 * singular by definition: one session is the active one. Counting it in one
 * place makes double-counting impossible rather than merely unlikely.
 *
 * ## Why it stops when the window loses focus
 *
 * Otherwise the number is "hours rmux was open", which is not a measurement of
 * anyone's day — leaving the app running overnight would report a heroic
 * sixteen-hour session on whatever pane happened to be active. `document`
 * visibility catches minimising and switching Spaces; `blur` catches the far
 * more common case of simply working in another app.
 *
 * ## Why focus alone is not enough
 *
 * A focused window is not a person. rmux is left frontmost across lunch, and
 * counting that reported hours nobody worked — the same over-count the focus
 * check was added to prevent, one level in. So the clock also needs *input*:
 * a keystroke, a scroll, a wheel or a mouse move, after which it runs for
 * `IDLE_MS` and then stops until something happens again.
 *
 * ## Why the accrual is idle-capped
 *
 * A machine that sleeps fires no timer, and on wake the elapsed wall-clock
 * would be credited in one lump. The cap means a gap longer than the tick is
 * treated as away-from-desk, which is what it almost always was.
 */

/** How often to bank time. Also the largest gap credited to one tick. */
const TICK_MS = 5_000;

/**
 * How long after the last keystroke, scroll or mouse move the clock keeps
 * running.
 *
 * A window can be focused for hours while nobody is at the desk, and "rmux was
 * frontmost" is not a measurement of anyone's day — it is the same
 * over-counting that made the window-focus check necessary in the first place,
 * one level in. Sixty seconds is a grace period rather than a threshold:
 * reading a long answer produces no input at all, and stopping the clock the
 * instant the hands go still would undercount the part of the work that is
 * thinking.
 */
const IDLE_MS = 60_000;

/**
 * How much of the interval since `last` is creditable, in milliseconds.
 *
 * Pure, and separated from the timer because this arithmetic is the part that
 * can be wrong in a way nobody notices — a dashboard reporting confident hours
 * cannot be checked against anything. Three rules it encodes:
 *
 *  - **Nothing while the window is unfocused.**
 *  - **Nothing after `lastActive + IDLE_MS`** — but everything up to it, so the
 *    grace period is counted rather than discarded when the tick that would
 *    have banked it arrives late.
 *  - **At most one tick per call.** A larger gap means the timer did not run —
 *    the machine slept, or the page was throttled — and crediting the whole gap
 *    would hand someone eight hours for closing their laptop.
 */
export function creditable(opts: {
  now: number;
  /** When time was last banked. */
  last: number;
  /** When the operator last did anything. */
  lastActive: number;
  focused: boolean;
  idleMs?: number;
  tickMs?: number;
}): number {
  const { now, last, lastActive, focused } = opts;
  const idleMs = opts.idleMs ?? IDLE_MS;
  const tickMs = opts.tickMs ?? TICK_MS;
  if (!focused) return 0;
  const until = Math.min(now, lastActive + idleMs);
  const elapsed = until - last;
  if (elapsed <= 0) return 0;
  return Math.min(elapsed, tickMs);
}

/**
 * The events that count as "still here".
 *
 * `keydown` rather than `keypress` so modifiers and arrows count; `wheel` and
 * `scroll` because reading is work; `mousemove` because moving to the pane you
 * are about to type in is the moment attention arrives. Listened for in the
 * **capture** phase on `window`, so a handler that stops propagation — xterm
 * does, constantly — cannot make the operator look idle while they type.
 */
const ACTIVITY = ["keydown", "mousedown", "mousemove", "wheel", "scroll", "touchstart"] as const;

export function startAttentionWatch(): () => void {
  let last = Date.now();
  let lastActive = Date.now();
  let focused = typeof document === "undefined" || document.visibilityState === "visible";

  const bank = () => {
    const now = Date.now();
    const ms = creditable({ now, last, lastActive, focused });
    last = now;
    if (ms <= 0) return;
    const active = useWorkspace.getState().activeSession;
    if (active) recordSeconds(active, ms / 1000);
  };

  // Only a timestamp write — no store read, no storage write, so this stays
  // cheap enough to sit on `mousemove`.
  const touch = () => {
    lastActive = Date.now();
  };

  const onVisibility = () => {
    // Bank what was earned *before* the state changes, then move the marker, or
    // time spent away is credited to the session on return.
    bank();
    focused = document.visibilityState === "visible";
    last = Date.now();
  };
  const onBlur = () => {
    bank();
    focused = false;
    last = Date.now();
  };
  const onFocus = () => {
    focused = true;
    last = Date.now();
    // Coming back to the window *is* activity — otherwise a session left idle
    // for an hour would still read as idle for the first minute after you
    // returned to it, and the clock would not restart until you touched
    // something.
    lastActive = Date.now();
  };

  const timer = window.setInterval(bank, TICK_MS);
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("blur", onBlur);
  window.addEventListener("focus", onFocus);
  for (const type of ACTIVITY) {
    window.addEventListener(type, touch, { capture: true, passive: true });
  }

  return () => {
    bank();
    window.clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("blur", onBlur);
    window.removeEventListener("focus", onFocus);
    for (const type of ACTIVITY) {
      window.removeEventListener(type, touch, { capture: true });
    }
  };
}
