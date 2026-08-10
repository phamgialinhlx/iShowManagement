import { useEffect, useRef } from "react";

import type { PaneRef, SessionStatus } from "./workspace-model";

/**
 * Hands-free: the keyboard follows whichever session next needs a person.
 *
 * Watching four Claude sessions in a grid means noticing one has stopped, aiming
 * at it, clicking, and only then typing — every time, all day. With this on, a
 * session that finishes or asks a question takes the cursor, so the next thing
 * typed goes to the session that was asking for it.
 *
 * ## It moves on the *edge*, never on the state
 *
 * Only a session that has **just** become answerable is a candidate. Acting on
 * "is idle" instead would mean the cursor was dragged to whichever idle pane
 * happened to sort first, over and over, for as long as the mode was on — an
 * idle session is the resting state of most panes most of the time.
 *
 * ## And never while someone is typing
 *
 * This is the one rule that decides whether the feature is usable. Taking the
 * keyboard out of a pane mid-sentence sends the rest of that sentence somewhere
 * else, which is the "nothing moves under the operator's hands" rule and is far
 * worse than the manual click it was meant to save.
 *
 * So a recent keystroke suppresses the move outright rather than deferring it.
 * Deferring sounds kinder and is not: the jump then arrives during the pause
 * where someone stopped to think, which is more startling than one that never
 * came. Nothing is lost by skipping — the rail still marks the session, which is
 * what the rail is for.
 */

/** How long after a keystroke the keyboard is left alone. */
export const QUIET_MS = 2500;

/** A status that wants a person: Claude has stopped, or is asking. */
const answerable = (s: SessionStatus | undefined): boolean => s === "idle" || s === "waiting";

/**
 * The session that just became answerable, and the tile it is in.
 *
 * Pure, so the decision can be tested without a grid, a store or a clock —
 * everything below it is timing and side effects.
 *
 * `waiting` outranks `idle` because it is a question with someone on the other
 * end of it; a session that merely finished can be looked at a moment later.
 */
export function nextHandsFreeTarget(opts: {
  panes: (PaneRef | null)[];
  /** Statuses as they were at the previous evaluation. */
  before: Record<string, SessionStatus | undefined>;
  after: Record<string, SessionStatus | undefined>;
  /** The session the operator is already in — never worth jumping to. */
  activeSession: string | null;
}): { id: string; cell: number } | null {
  const { panes, before, after, activeSession } = opts;

  // A plain loop rather than `forEach`: inside a callback TypeScript widens the
  // accumulator to `never` after the first assignment, and working around that
  // costs more clarity than the loop does.
  let bestId: string | null = null;
  let bestCell = -1;
  let bestRank = Number.POSITIVE_INFINITY;

  for (let cell = 0; cell < panes.length; cell += 1) {
    const pane = panes[cell];
    if (!pane || pane.kind !== "session") continue;
    const id = pane.id;
    if (id === activeSession) continue;

    const was = before[id];
    const now = after[id];
    // The edge, not the state. A session that was already answerable at the
    // previous evaluation has been sitting there, and moving to it now would be
    // moving to it every time this ran.
    if (!answerable(now) || answerable(was)) continue;
    // No previous status means this hook is seeing the session for the first
    // time — on mount, or when a pane was just filled. That is not an edge the
    // operator caused, and treating it as one makes the mode grab the keyboard
    // the instant it is switched on.
    if (was === undefined) continue;

    const rank = now === "waiting" ? 0 : 1;
    if (rank < bestRank) {
      bestId = id;
      bestCell = cell;
      bestRank = rank;
    }
  }

  return bestId === null ? null : { id: bestId, cell: bestCell };
}

/**
 * Drive the focus from status changes, while the mode is on.
 *
 * Deliberately reads the statuses it is given rather than subscribing to the
 * store itself: the caller already has them, and a second subscription here
 * would re-run this on every unrelated store write.
 */
export function useHandsFree(opts: {
  enabled: boolean;
  panes: (PaneRef | null)[];
  statuses: Record<string, SessionStatus | undefined>;
  activeSession: string | null;
  cells: number;
  onGo: (id: string, cell: number) => void;
}): void {
  const { enabled, panes, statuses, activeSession, cells, onGo } = opts;

  const previous = useRef<Record<string, SessionStatus | undefined>>({});
  const lastTyped = useRef(0);
  const go = useRef(onGo);
  go.current = onGo;

  // Capture, because xterm stops propagation on the keys it handles — which is
  // most of them, and precisely the ones that mean someone is mid-sentence.
  useEffect(() => {
    const typed = () => {
      lastTyped.current = Date.now();
    };
    window.addEventListener("keydown", typed, { capture: true });
    return () => window.removeEventListener("keydown", typed, { capture: true } as EventListenerOptions);
  }, []);

  useEffect(() => {
    const before = previous.current;
    previous.current = statuses;

    if (!enabled) return;
    // One tile is not a grid: there is nowhere to move the keyboard to, and the
    // pane is already in front of the operator.
    if (cells <= 1) return;
    if (Date.now() - lastTyped.current < QUIET_MS) return;

    const target = nextHandsFreeTarget({ panes, before, after: statuses, activeSession });
    if (target) go.current(target.id, target.cell);
  }, [enabled, panes, statuses, activeSession, cells]);
}

/** Remembered across launches: it is a way of working, not a per-visit choice. */
const KEY = "rmux.handsFree";

export function readHandsFree(): boolean {
  try {
    return localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

export function writeHandsFree(on: boolean): void {
  try {
    if (on) localStorage.setItem(KEY, "1");
    else localStorage.removeItem(KEY);
  } catch {
    /* a full localStorage must not stop a mode being toggled */
  }
}
