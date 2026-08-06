/**
 * The hour buckets behind the working-hours chart.
 *
 * Open http://localhost:5273/activity-hours-check.html and read the console.
 *
 * Two things here are easy to get wrong in a way that still *looks* right on a
 * chart, which is why they are pinned rather than eyeballed:
 *
 *  - **The bucket is written at the moment the time is banked.** Deriving it
 *    later reads whatever hour the dashboard happened to open in, so the whole
 *    day's work would pile into one column and the chart would be confidently,
 *    silently wrong.
 *  - **The store is bounded in both dimensions.** `days` was pruned and `hours`
 *    was not, at first — a store that is bounded in one dimension reads as
 *    bounded right up until the quota it shares with the session list runs out.
 */
import { HOURS_IN_DAY, dayKey, hourTotals, readActivity, recordSeconds } from "./src/lib/activity";
import { clock, peakHour } from "./src/lib/dashboard";
import { creditable } from "./src/lib/attention";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) {
    console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  } else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

const KEY = "rmux.activity";
const saved = localStorage.getItem(KEY);
const today = dayKey();

/** Start from a known store, since these assert absolute numbers. */
const reset = () => localStorage.removeItem(KEY);

// ── nothing recorded ─────────────────────────────────────────────────────────

reset();
check("an empty store still yields 24 buckets", hourTotals([today]).length === HOURS_IN_DAY);
check("every bucket of an empty store is zero", hourTotals([today]).every((n) => n === 0));

// Hour zero is a real answer, so "no data" must not be reported as midnight —
// somebody who works late would be told their peak is the one hour they never
// worked.
check("peak hour of an empty day is undefined, not 00", peakHour(clock([today])) === undefined);

// ── recording ────────────────────────────────────────────────────────────────

reset();
recordSeconds("s1", 90);
const hour = new Date().getHours();
check("time lands in the current local hour", hourTotals([today])[hour] === 90);
check("no other hour is touched", hourTotals([today]).filter((n) => n > 0).length === 1);
check(
  "the session's own tally is updated in the same write",
  readActivity().days[today]?.["s1"]?.seconds === 90,
);

recordSeconds("s2", 30);
check("a second session adds to the same hour bucket", hourTotals([today])[hour] === 120);
check(
  "sessions keep their own tallies",
  readActivity().days[today]?.["s2"]?.seconds === 30 && readActivity().days[today]?.["s1"]?.seconds === 90,
);

// Sub-second ticks are floored — the watcher calls this several times a minute
// and a float per tick is a serialise of the whole store for a number shown to
// the nearest minute.
const before = hourTotals([today])[hour];
recordSeconds("s1", 0.4);
check("a sub-second tick banks nothing", hourTotals([today])[hour] === before);

// A tick with no active session is not attention anyone spent.
recordSeconds("", 60);
check("no session means no bucket", hourTotals([today])[hour] === before);

// ── reading across days ──────────────────────────────────────────────────────

reset();
recordSeconds("s1", 60);
const yesterday = dayKey(new Date(Date.now() - 86_400_000));
{
  // Write a synthetic yesterday directly: the recorder can only write *now*,
  // which is the property being relied on everywhere else.
  const activity = readActivity();
  const buckets = new Array<number>(HOURS_IN_DAY).fill(0);
  buckets[9] = 1_800;
  (activity.hours ??= {})[yesterday] = buckets;
  localStorage.setItem(KEY, JSON.stringify(activity));
}
const both = hourTotals([yesterday, today]);
check("hours sum across the days asked for", both[9]! >= 1_800);
check("a day not asked for is not included", hourTotals([yesterday])[hour] === (hour === 9 ? 1_800 : 0));
check("clock() reports 24 points, labelled", clock([today]).length === 24 && clock([today])[9]?.label === "09");
check("the busiest hour is found", peakHour(clock([yesterday]))?.hour === 9);

// ── the store stays bounded ──────────────────────────────────────────────────

{
  const activity = readActivity();
  const ancient = dayKey(new Date(Date.now() - 400 * 86_400_000));
  (activity.hours ??= {})[ancient] = new Array<number>(HOURS_IN_DAY).fill(5);
  activity.days[ancient] = { s1: { commands: 1, prompts: 0, seconds: 0, tasksDone: 0 } };
  localStorage.setItem(KEY, JSON.stringify(activity));

  // Any write prunes; this is what makes the bound structural rather than a
  // cleanup that might never run.
  recordSeconds("s1", 1);
  const after = readActivity();
  check("an ancient day's tally is pruned on write", !after.days[ancient]);
  check("an ancient day's hour buckets are pruned too", !after.hours?.[ancient]);
}

// ── the idle gate ────────────────────────────────────────────────────────────

// `creditable` decides how much of an interval counts as attention. It is
// pinned because a dashboard that reports confident hours cannot be checked
// against anything afterwards — a wrong constant here is invisible forever.
{
  const T = 5_000; // one tick
  const IDLE = 60_000;
  const base = { idleMs: IDLE, tickMs: T, focused: true };

  check(
    "an ordinary tick with recent activity credits the interval",
    creditable({ ...base, now: 10_000, last: 5_000, lastActive: 9_000 }) === 5_000,
  );
  check(
    "an unfocused window credits nothing",
    creditable({ ...base, focused: false, now: 10_000, last: 5_000, lastActive: 9_000 }) === 0,
  );

  // The point of the feature: rmux left frontmost over lunch is not work.
  check(
    "idle past the grace period credits nothing",
    creditable({ ...base, now: 200_000, last: 195_000, lastActive: 100_000 }) === 0,
  );
  // ...but the grace period itself is counted, and counted *fully* even when
  // the tick that should have banked it arrives after the deadline. Dropping
  // it would quietly lose up to a minute of every sitting.
  check(
    "the tail up to the deadline is credited, not discarded",
    creditable({ ...base, now: 63_000, last: 59_000, lastActive: 2_000 }) === 3_000,
  );
  check(
    "nothing is credited twice after the deadline has passed",
    creditable({ ...base, now: 70_000, last: 63_000, lastActive: 2_000 }) === 0,
  );

  // A gap longer than a tick means the timer never ran — the machine slept, or
  // the page was throttled. Crediting the whole gap would hand someone eight
  // hours for closing their laptop.
  check(
    "a long gap is capped at one tick",
    creditable({ ...base, now: 3_600_000, last: 0, lastActive: 3_599_000 }) === T,
  );
  check(
    "a zero-length interval credits nothing",
    creditable({ ...base, now: 10_000, last: 10_000, lastActive: 10_000 }) === 0,
  );
  // Clocks can go backwards (NTP, sleep). A negative interval must not become
  // a negative credit, which `record` would clamp to zero anyway but only after
  // corrupting the running total on the way.
  check(
    "a backwards clock credits nothing",
    creditable({ ...base, now: 5_000, last: 10_000, lastActive: 5_000 }) === 0,
  );
}

// ── restore ──────────────────────────────────────────────────────────────────

if (saved === null) localStorage.removeItem(KEY);
else localStorage.setItem(KEY, saved);

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
  failures ? "background:#e63b2e;color:#fff" : "background:#2b7;color:#000",
);
