/**
 * What the operator actually did today.
 *
 * ## Why any of this is stored at all
 *
 * Most of the dashboard is *derived* — prompts, tool calls, tokens and session
 * timespans all come out of Claude's own transcripts, which are the real record
 * and go back as far as the conversation does. Deriving beats storing: a counter
 * we keep can drift from the truth, and a transcript cannot.
 *
 * Two things have no such record, so they are counted here:
 *
 *  - **terminal commands**, because a shell's history belongs to the shell and
 *    reading it back over SSH per session per refresh is a poll of somebody
 *    else's file for a number on a dashboard.
 *  - **attention time**, meaning the wall-clock a session was the one you were
 *    looking at. Nothing else knows that; a transcript records when Claude
 *    replied, not when you were watching.
 *
 * ## The honesty rule
 *
 * These counters start the day this ships and know nothing before it. The
 * dashboard therefore labels them as counted-from rather than showing a
 * confident lifetime total that is really "since Tuesday" — a number that looks
 * historical but is not is worse than an absent one.
 *
 * ## Why `localStorage` and not a file on a host
 *
 * It is about the operator, not about any one machine: terminal commands span
 * every server, and writing a usage log into someone's repository because they
 * ran `ls` would be a surprising side effect that also wants committing. The
 * whole store is bounded (see `RETAIN_DAYS`) so it cannot grow into the quota
 * the session list shares.
 */

/** Days of history kept. A month is enough for a streak and stays small. */
const RETAIN_DAYS = 60;
const KEY = "rmux.activity";

/** One day's tally for one session. */
export type DayEntry = {
  /** Terminal commands submitted (Enter pressed on a non-empty line). */
  commands: number;
  /** Prompts submitted to Claude from rmux's own composer. */
  prompts: number;
  /** Seconds this session was the active one, with the window focused. */
  seconds: number;
  /**
   * Tasks ticked on this day.
   *
   * **Recorded as it happens, because a note cannot be asked afterwards.** A
   * note stores only the *current* state of its checkboxes — `- [x]` says a task
   * is done, never when. So "tasks finished on Tuesday" is unknowable from the
   * notes alone, and any chart over time had to either invent it or not exist.
   * Un-ticking decrements, so the count stays a description of the board rather
   * than a score that only goes up.
   */
  tasksDone: number;
};

export type Activity = {
  /** `YYYY-MM-DD` → sessionId → tally. */
  days: Record<string, Record<string, DayEntry>>;
  /**
   * `YYYY-MM-DD` → 24 buckets of attention seconds, by **local** hour.
   *
   * Kept beside `days` rather than inside a `DayEntry` because the question it
   * answers — *when* in the day does work happen — is about the operator, not
   * about any one session. Per-session buckets would be 24 numbers times every
   * session times every retained day, for a chart that sums them all back
   * together on the way out.
   *
   * Optional, because every store written before this existed lacks it.
   */
  hours?: Record<string, number[]>;
  /** First day ever recorded, so the UI can say what "total" really covers. */
  since?: string;
};

/** Buckets in a day. Named, because `24` appears in three files otherwise. */
export const HOURS_IN_DAY = 24;

const empty = (): DayEntry => ({ commands: 0, prompts: 0, seconds: 0, tasksDone: 0 });

/**
 * Local date, not UTC.
 *
 * A day boundary at UTC midnight would end the working day mid-afternoon in
 * Asia/Saigon and split one sitting across two rows, which makes a streak
 * meaningless. `toISOString` is UTC, hence the manual assembly.
 */
export function dayKey(at: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
}

export function readActivity(): Activity {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { days: {} };
    const parsed = JSON.parse(raw) as Activity;
    return parsed && typeof parsed === "object" && parsed.days ? parsed : { days: {} };
  } catch {
    // A corrupt tally is not worth failing over — it is a dashboard, not the work.
    return { days: {} };
  }
}

function write(activity: Activity): void {
  // Drop anything past the window *before* writing, so the store is bounded by
  // construction rather than by a cleanup that might never run.
  const cutoff = dayKey(new Date(Date.now() - RETAIN_DAYS * 86_400_000));
  for (const day of Object.keys(activity.days)) {
    if (day < cutoff) delete activity.days[day];
  }
  // The hour buckets are pruned on the same cutoff. Forgetting this half would
  // leave a store that is bounded in one dimension and unbounded in the other,
  // which reads as bounded right up until it is not.
  for (const day of Object.keys(activity.hours ?? {})) {
    if (day < cutoff) delete activity.hours![day];
  }
  try {
    localStorage.setItem(KEY, JSON.stringify(activity));
    window.dispatchEvent(new CustomEvent("rmux:activity-changed"));
  } catch {
    // A full localStorage must not break typing or the terminal.
  }
}

/**
 * Read, change, write — once.
 *
 * Every writer goes through here so the whole store is serialised a single time
 * per event. `recordSeconds` updates two places at once and must not pay for
 * two full `JSON.stringify`s of the store to do it; it runs on a timer.
 */
function mutate(change: (activity: Activity, today: string) => void): void {
  const activity = readActivity();
  const today = dayKey();
  activity.since ??= today;
  change(activity, today);
  write(activity);
}

/** Add to one session's tally for today. */
export function record(sessionId: string, field: keyof DayEntry, amount = 1): void {
  // Un-ticking passes -1, so a zero guard would silently drop it. Only a
  // *no-op* is refused.
  if (!sessionId || amount === 0) return;
  mutate((activity, today) => {
    const day = (activity.days[today] ??= {});
    const entry = (day[sessionId] ??= empty());
    // Older records predate `tasksDone`, so the field can be absent on a shape
    // that is otherwise valid. `NaN` would poison every total downstream.
    entry[field] = Math.max(0, (entry[field] ?? 0) + amount);
  });
}

/**
 * Seconds are accumulated in *whole* units, and that is deliberate.
 *
 * A ticker writing sub-second floats to `localStorage` several times a second
 * is a serialise-and-parse of the whole store on every tick, for a number
 * displayed to the nearest minute. The caller batches and calls this once a
 * second at most.
 */
export function recordSeconds(sessionId: string, seconds: number): void {
  const whole = Math.floor(seconds);
  if (!sessionId || whole <= 0) return;
  // The hour is read *now* rather than derived later: a bucket is where the
  // time was spent, and by the time anything reads the store it is a different
  // hour. This is also why a tick crossing an hour boundary lands wholly in the
  // hour it was banked in — five seconds of misattribution once an hour, against
  // splitting every tick to avoid it.
  const hour = new Date().getHours();
  mutate((activity, today) => {
    const day = (activity.days[today] ??= {});
    const entry = (day[sessionId] ??= empty());
    entry.seconds = Math.max(0, (entry.seconds ?? 0) + whole);

    const buckets = ((activity.hours ??= {})[today] ??= new Array<number>(HOURS_IN_DAY).fill(0));
    buckets[hour] = (buckets[hour] ?? 0) + whole;
  });
}

/**
 * Attention seconds by hour of day, summed over the given days.
 *
 * Always 24 entries — an hour with nothing in it is a zero, not a gap. A chart
 * that dropped the quiet hours would compress the axis and put lunch next to
 * midnight.
 */
export function hourTotals(days: readonly string[]): number[] {
  const activity = readActivity();
  const out = new Array<number>(HOURS_IN_DAY).fill(0);
  for (const day of days) {
    const buckets = activity.hours?.[day];
    if (!buckets) continue;
    for (let h = 0; h < HOURS_IN_DAY; h += 1) out[h] = (out[h] ?? 0) + (buckets[h] ?? 0);
  }
  return out;
}

/** Everything recorded for one day, session by session. */
export function dayTotals(day: string = dayKey()): Record<string, DayEntry> {
  return readActivity().days[day] ?? {};
}

/** One day's totals across every session. */
export function daySummary(day: string = dayKey()): DayEntry {
  return Object.values(dayTotals(day)).reduce(
    (acc, e) => ({
      commands: acc.commands + (e.commands ?? 0),
      prompts: acc.prompts + (e.prompts ?? 0),
      seconds: acc.seconds + (e.seconds ?? 0),
      tasksDone: acc.tasksDone + (e.tasksDone ?? 0),
    }),
    empty(),
  );
}

/** Days with any recorded work, newest first. */
export function activeDays(activity: Activity = readActivity()): string[] {
  return Object.entries(activity.days)
    .filter(([, sessions]) =>
      Object.values(sessions).some((e) => (e.commands ?? 0) + (e.prompts ?? 0) + (e.seconds ?? 0) > 0),
    )
    .map(([day]) => day)
    .sort()
    .reverse();
}

/**
 * Consecutive days worked, counting back from today.
 *
 * **Yesterday still counts as an unbroken streak.** Checking only "does today
 * have work" would report a broken streak every morning before the first
 * command, which punishes someone for reading their dashboard early — the exact
 * moment they are most likely to look at it.
 */
export function streak(activity: Activity = readActivity()): number {
  const days = new Set(activeDays(activity));
  if (!days.size) return 0;

  const today = dayKey();
  const dayBefore = (key: string) => {
    const [y, m, d] = key.split("-").map(Number);
    return dayKey(new Date((y ?? 0), (m ?? 1) - 1, (d ?? 1) - 1));
  };

  let cursor = days.has(today) ? today : dayBefore(today);
  if (!days.has(cursor)) return 0;

  let count = 0;
  while (days.has(cursor)) {
    count += 1;
    cursor = dayBefore(cursor);
  }
  return count;
}
