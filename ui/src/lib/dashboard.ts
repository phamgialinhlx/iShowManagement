import { dayKey, dayTotals, hourTotals, readActivity, streak, type DayEntry } from "./activity";
import { noteTasks, type NoteTask } from "./note-tasks";

/**
 * The numbers behind the dashboard, gathered in one place.
 *
 * Pure over its inputs and free of React, so the same figures can be asserted
 * in a harness. Every field is traceable to something measured; nothing here
 * estimates. Where a number cannot be known it is absent rather than guessed —
 * a dashboard that invents a plausible figure is worse than one that admits a
 * gap, because nobody can tell which is which afterwards.
 */

/** A task found in some session's note. */
export type SessionTask = NoteTask & { sessionId: string; sessionName: string };

export type SessionRow = {
  id: string;
  name: string;
  /** From the local tally — only ever "since we started counting". */
  activity: DayEntry;
  tasks: { done: number; total: number };
};

export type Dashboard = {
  day: string;
  tasks: { done: number; total: number; items: SessionTask[] };
  totals: DayEntry;
  rows: SessionRow[];
  streak: number;
  /** First day the local counters ever recorded, for honest labelling. */
  countingSince?: string;
};

/** Read every session's note straight from storage. */
export function noteOf(sessionId: string): string {
  try {
    return localStorage.getItem(`rmux.note.${sessionId}`) ?? "";
  } catch {
    return "";
  }
}

/**
 * Everything today, across every session.
 *
 * Sessions are passed in rather than read from the store so this stays pure —
 * and so the dashboard shows the sessions that *exist*, not every note ever
 * left behind. A note whose session was closed is deliberately dropped: its
 * tasks are not work anyone is going to do, and counting them would make the
 * bar unreachable.
 */
export function buildDashboard(
  sessions: readonly { id: string; name: string }[],
  day: string = dayKey(),
): Dashboard {
  const perSession = dayTotals(day);

  const items: SessionTask[] = [];
  const rows: SessionRow[] = [];

  for (const session of sessions) {
    const tasks = noteTasks(noteOf(session.id));
    for (const task of tasks) {
      items.push({ ...task, sessionId: session.id, sessionName: session.name });
    }
    const activity: DayEntry = perSession[session.id] ?? {
      commands: 0,
      prompts: 0,
      seconds: 0,
      tasksDone: 0,
    };
    // A session with neither tasks nor activity today is not a row worth a
    // line: a dashboard listing every session you have ever opened, mostly
    // zeroes, hides the two that matter.
    if (tasks.length || activity.commands || activity.prompts || activity.seconds) {
      rows.push({
        id: session.id,
        name: session.name,
        activity,
        tasks: { done: tasks.filter((t) => t.done).length, total: tasks.length },
      });
    }
  }

  // Busiest first — the question a dashboard answers is "where did the day go",
  // and alphabetical order answers a different one.
  rows.sort((a, b) => b.activity.seconds - a.activity.seconds || b.tasks.total - a.tasks.total);

  const totals = Object.values(perSession).reduce<DayEntry>(
    (acc, e) => ({
      commands: acc.commands + (e.commands ?? 0),
      prompts: acc.prompts + (e.prompts ?? 0),
      seconds: acc.seconds + (e.seconds ?? 0),
      tasksDone: acc.tasksDone + (e.tasksDone ?? 0),
    }),
    { commands: 0, prompts: 0, seconds: 0, tasksDone: 0 },
  );

  return {
    day,
    tasks: { done: items.filter((t) => t.done).length, total: items.length, items },
    totals,
    rows,
    streak: streak(),
    countingSince: readActivity().since,
  };
}

/**
 * Milestones, and why they are counted rather than awarded.
 *
 * Each one is a threshold over a number already on screen, so a badge can
 * always be checked against the figure beside it. Nothing is granted for
 * opening the app or for time passing: a reward for showing up is a reward for
 * nothing, and it devalues the ones that mean something.
 */
export type Milestone = { id: string; label: string; hint: string; reached: boolean; progress: number };

export function milestones(d: Dashboard, allTimeTasks: number): Milestone[] {
  const at = (value: number, target: number) => Math.min(1, target ? value / target : 0);
  return [
    {
      id: "first-task",
      label: "FIRST TASK",
      hint: "Tick a checkbox in any note",
      reached: allTimeTasks >= 1,
      progress: at(allTimeTasks, 1),
    },
    {
      id: "clean-sweep",
      label: "CLEAN SWEEP",
      hint: "Finish every task on the board",
      reached: d.tasks.total > 0 && d.tasks.done === d.tasks.total,
      progress: d.tasks.total ? d.tasks.done / d.tasks.total : 0,
    },
    {
      id: "ten-today",
      label: "TEN TODAY",
      hint: "Ten tasks done in one day",
      reached: d.tasks.done >= 10,
      progress: at(d.tasks.done, 10),
    },
    {
      id: "week",
      label: "SEVEN DAYS",
      hint: "A week without a gap",
      reached: d.streak >= 7,
      progress: at(d.streak, 7),
    },
    {
      id: "century",
      label: "CENTURY",
      hint: "A hundred commands in a day",
      reached: d.totals.commands >= 100,
      progress: at(d.totals.commands, 100),
    },
    {
      id: "deep-work",
      label: "DEEP WORK",
      hint: "Four hours on one session in a day",
      reached: d.rows.some((r) => r.activity.seconds >= 4 * 3600),
      progress: at(Math.max(0, ...d.rows.map((r) => r.activity.seconds)), 4 * 3600),
    },
  ];
}

/** `3h 12m`, or `48m`, or `—` when nothing was recorded. */
export function humanDuration(seconds: number): string {
  if (seconds < 60) return seconds > 0 ? `${Math.round(seconds)}s` : "—";
  const mins = Math.round(seconds / 60);
  if (mins < 60) return `${mins}m`;
  return `${Math.floor(mins / 60)}h ${String(mins % 60).padStart(2, "0")}m`;
}

/** One day on the history chart. */
export type DayPoint = {
  day: string;
  /** Day of month, the only part a 14-column axis has room for. */
  label: string;
  tasksDone: number;
  commands: number;
  prompts: number;
  seconds: number;
  /** Whether anything at all happened, so a gap reads as a gap. */
  any: boolean;
};

/**
 * The last `count` days ending today, **including the empty ones**.
 *
 * Charting only the days with activity would silently close the gaps — a week
 * off would render as a continuous run and the shape of the fortnight would be a
 * lie. The x-axis is time, so every day gets a column whether or not it has a
 * bar.
 */
export function history(count = 14, end: Date = new Date()): DayPoint[] {
  const out: DayPoint[] = [];
  const activity = readActivity();
  for (let i = count - 1; i >= 0; i -= 1) {
    const at = new Date(end.getFullYear(), end.getMonth(), end.getDate() - i);
    const day = dayKey(at);
    const totals = Object.values(activity.days[day] ?? {}).reduce(
      (acc, e) => ({
        tasksDone: acc.tasksDone + (e.tasksDone ?? 0),
        commands: acc.commands + (e.commands ?? 0),
        prompts: acc.prompts + (e.prompts ?? 0),
        seconds: acc.seconds + (e.seconds ?? 0),
      }),
      { tasksDone: 0, commands: 0, prompts: 0, seconds: 0 },
    );
    out.push({
      day,
      label: String(at.getDate()),
      ...totals,
      any: totals.tasksDone + totals.commands + totals.prompts + totals.seconds > 0,
    });
  }
  return out;
}

/** One hour of the day on the working-hours chart. */
export type HourPoint = {
  /** 0..23, local. */
  hour: number;
  /** `09`, because a 24-column axis has room for two digits and no more. */
  label: string;
  seconds: number;
};

/**
 * When the work happens, summed over the given days.
 *
 * A *shape*, not a total: the interesting reading is where the mass sits, and
 * one day of it is too sparse to have a shape at all. Passing the day keys in
 * keeps this a pure function of the store and lets the page chart the same
 * fortnight it charts everywhere else, rather than a second range that quietly
 * disagrees.
 */
export function clock(days: readonly string[]): HourPoint[] {
  const totals = hourTotals(days);
  return totals.map((seconds, hour) => ({
    hour,
    label: String(hour).padStart(2, "0"),
    seconds,
  }));
}

/**
 * The busiest hour, or `undefined` when nothing has been recorded.
 *
 * Undefined rather than `0`, because hour zero is a real answer — someone who
 * works at midnight would be told their peak is midnight on a store with no
 * data in it at all.
 */
export function peakHour(points: readonly HourPoint[]): HourPoint | undefined {
  let best: HourPoint | undefined;
  for (const p of points) {
    if (p.seconds > 0 && (!best || p.seconds > best.seconds)) best = p;
  }
  return best;
}

/** The best single day recorded, for "personal best" style figures. */
export function personalBest(field: "tasksDone" | "commands" | "prompts" | "seconds"): number {
  const activity = readActivity();
  let best = 0;
  for (const sessions of Object.values(activity.days)) {
    const total = Object.values(sessions).reduce((n, e) => n + (e[field] ?? 0), 0);
    if (total > best) best = total;
  }
  return best;
}
