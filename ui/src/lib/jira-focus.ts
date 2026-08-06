import { dayKey } from "./activity";
import type { JiraIssue } from "./api";

/**
 * Which Jira issue each session is working on, and which ones today is for.
 *
 * ## Two different questions, two different stores
 *
 * *"What am I on right now, in this pane"* is a property of the **session** —
 * it is the same kind of fact as which folder the session is in, and it should
 * still be true tomorrow morning when the pane comes back. It has no date.
 *
 * *"What am I trying to finish today"* is a property of the **day**, and it
 * must expire: a plan filed on Tuesday shown unchanged on Friday is not a plan,
 * it is a stale list that quietly makes the progress bar meaningless.
 *
 * Storing both as one thing was the obvious shortcut and gets one of them
 * wrong whichever way it is folded.
 *
 * ## Nothing here is the truth about Jira
 *
 * Only *selections* are stored — issue keys. Status, summary and category are
 * always read back from the board, never cached, because they are edited by
 * other people in another system all day. A cached status is a claim rmux is
 * not in a position to make, and being confidently wrong about whether a
 * ticket is done is worse than a spinner.
 */

const FOCUS_KEY = "rmux.jira.focus";
const PLAN_KEY = "rmux.jira.plan";

/** Days of plans kept. Long enough to look back at a week, short enough to stay small. */
const RETAIN_DAYS = 30;

/** Fired whenever a selection changes, so the rail and Progress agree at once. */
export const JIRA_EVENT = "rmux:jira-changed";

const announce = () => window.dispatchEvent(new CustomEvent(JIRA_EVENT));

function read<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as T;
    return parsed && typeof parsed === "object" ? parsed : fallback;
  } catch {
    return fallback;
  }
}

function save(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
    announce();
  } catch {
    /* a full localStorage must not stop someone picking a ticket */
  }
}

// ── what this session is on ──────────────────────────────────────────────────

/** sessionId → issue key. */
export type Focus = Record<string, string>;

export function readFocus(): Focus {
  return read<Focus>(FOCUS_KEY, {});
}

export function focusOf(sessionId: string): string | undefined {
  return readFocus()[sessionId];
}

/** `null` clears it — "not on a ticket" is a real answer and must be settable. */
export function setFocus(sessionId: string, key: string | null): void {
  if (!sessionId) return;
  const focus = readFocus();
  if (key) focus[sessionId] = key;
  else delete focus[sessionId];
  save(FOCUS_KEY, focus);
}

// ── what today is for ────────────────────────────────────────────────────────

/** `YYYY-MM-DD` → issue keys chosen for that day. */
export type Plan = Record<string, string[]>;

export function readPlan(): Plan {
  return read<Plan>(PLAN_KEY, {});
}

export function planFor(day: string = dayKey()): string[] {
  return readPlan()[day] ?? [];
}

export function setPlanFor(keys: readonly string[], day: string = dayKey()): string[] {
  const plan = readPlan();
  // Deduplicated and order-preserving: the order is the operator's, and a
  // double-add from a double-click must not put the same ticket in twice.
  const next = [...new Set(keys)];
  if (next.length) plan[day] = next;
  else delete plan[day];

  const cutoff = dayKey(new Date(Date.now() - RETAIN_DAYS * 86_400_000));
  for (const d of Object.keys(plan)) {
    if (d < cutoff) delete plan[d];
  }
  save(PLAN_KEY, plan);
  return next;
}

/** In or out of today's plan. Returns the new list. */
export function togglePlan(key: string, day: string = dayKey()): string[] {
  const current = planFor(day);
  return setPlanFor(
    current.includes(key) ? current.filter((k) => k !== key) : [...current, key],
    day,
  );
}

// ── reading the board ────────────────────────────────────────────────────────

/**
 * Done means Jira's own `done` **category**, never a status name.
 *
 * "Done", "Closed", "Shipped", "Ready for QA" are all real status names and
 * mean different things on different boards. The category is the only part
 * that is defined the same way everywhere.
 */
export const isDone = (issue: Pick<JiraIssue, "statusCategory">): boolean =>
  (issue.statusCategory ?? "").toLowerCase() === "done";

export const isInProgress = (issue: Pick<JiraIssue, "statusCategory">): boolean =>
  (issue.statusCategory ?? "").toLowerCase() === "inprogress";

/** Today's chosen issues, resolved against the board, in the order chosen. */
export function planIssues(issues: readonly JiraIssue[], keys: readonly string[]): JiraIssue[] {
  const byKey = new Map(issues.map((i) => [i.key.toUpperCase(), i]));
  // A key that no longer resolves is dropped rather than shown as a stub: it
  // means the issue was unassigned, moved or deleted, and a row saying
  // "RMX-9 · unknown" invites clicking on something that is not there.
  return keys
    .map((k) => byKey.get(k.toUpperCase()))
    .filter((i): i is JiraIssue => !!i);
}

export type PlanProgress = { done: number; total: number };

export function planProgress(issues: readonly JiraIssue[]): PlanProgress {
  return { done: issues.filter(isDone).length, total: issues.length };
}
