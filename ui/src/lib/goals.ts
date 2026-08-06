/**
 * What you meant to get done today.
 *
 * A dashboard that only reports is a scoreboard for a game with no rules. A
 * target turns the same numbers into a question you can answer — am I on track —
 * which is the thing worth opening a page for.
 *
 * ## One target set, not one per day
 *
 * The goal is a *standing intention* ("about six tasks, about three hours"),
 * not a plan filed each morning. Per-day goals would mean an unset day is
 * ambiguous — no goal, or a goal of zero? — and would demand a ritual before the
 * dashboard said anything useful. A standing target applies to every day
 * including the ones already past, so history is immediately readable.
 *
 * ## Zero means "not tracking this"
 *
 * Rather than a separate enable switch. A goal of zero tasks is not a goal
 * anyone holds, so the value can carry its own off state and the UI stays two
 * fields instead of two fields and two checkboxes.
 */

const KEY = "rmux.goals";

export type Goals = {
  /** Tasks to tick in a day. 0 = not tracked. */
  tasks: number;
  /** Minutes of attention time in a day. 0 = not tracked. */
  minutes: number;
};

export const NO_GOALS: Goals = { tasks: 0, minutes: 0 };

/** Sane ceilings. A target of 900 tasks is a typo, and it flattens every bar. */
const MAX_TASKS = 200;
const MAX_MINUTES = 24 * 60;

const clampGoals = (g: Goals): Goals => ({
  tasks: Math.min(MAX_TASKS, Math.max(0, Math.round(g.tasks) || 0)),
  minutes: Math.min(MAX_MINUTES, Math.max(0, Math.round(g.minutes) || 0)),
});

export function readGoals(): Goals {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return NO_GOALS;
    const parsed = JSON.parse(raw) as Partial<Goals>;
    return clampGoals({ tasks: Number(parsed?.tasks) || 0, minutes: Number(parsed?.minutes) || 0 });
  } catch {
    return NO_GOALS;
  }
}

export function writeGoals(goals: Goals): Goals {
  const next = clampGoals(goals);
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
    window.dispatchEvent(new CustomEvent("rmux:goals-changed"));
  } catch {
    /* a full localStorage must not break setting a goal */
  }
  return next;
}

/**
 * How far through a goal a value is, 0..1.
 *
 * Capped at 1 so a bar cannot overflow its track, but callers are given the raw
 * ratio too — "180% of target" is worth saying, and a bar pinned at full is not
 * the same information.
 */
export function goalProgress(value: number, target: number): { ratio: number; clamped: number } {
  if (target <= 0) return { ratio: 0, clamped: 0 };
  const ratio = value / target;
  return { ratio, clamped: Math.min(1, ratio) };
}
