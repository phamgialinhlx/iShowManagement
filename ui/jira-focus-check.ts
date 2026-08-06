/**
 * Which ticket each session is on, and which ones today is for.
 *
 * Open http://localhost:5273/jira-focus-check.html and read the console.
 *
 * The one that matters most here is **done is a category, never a name**. Every
 * board renames its statuses — "Shipped", "Closed", "Ready for QA" are all real
 * — so a progress bar built on names is right on the board it was written
 * against and quietly wrong on every other one. That failure looks exactly like
 * success, which is why it is pinned rather than trusted.
 */
import type { JiraIssue } from "./src/lib/api";
import {
  focusOf,
  isDone,
  isInProgress,
  planFor,
  planIssues,
  planProgress,
  readPlan,
  setFocus,
  setPlanFor,
  togglePlan,
} from "./src/lib/jira-focus";
import { dayKey } from "./src/lib/activity";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) {
    console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  } else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

const savedFocus = localStorage.getItem("rmux.jira.focus");
const savedPlan = localStorage.getItem("rmux.jira.plan");
const reset = () => {
  localStorage.removeItem("rmux.jira.focus");
  localStorage.removeItem("rmux.jira.plan");
};

// ── done is a category, not a name ───────────────────────────────────────────

const issue = (key: string, status: string, statusCategory: string): JiraIssue => ({
  key,
  summary: `the ${key} work`,
  status,
  statusCategory,
});

// A board whose "done" column is called something else.
check("a renamed done column still counts as done", isDone(issue("RMX-1", "Shipped", "done")));
// And the trap in the other direction: a column *called* Done that Jira does
// not consider done — a real configuration, and the one names get wrong.
check("a status named Done in an unfinished category is not done", !isDone(issue("RMX-2", "Done", "inprogress")));
check("in progress is its own category", isInProgress(issue("RMX-3", "Reviewing", "inprogress")));
check("a missing category is not treated as done", !isDone({}));

// ── what this session is on ──────────────────────────────────────────────────

reset();
check("a session with no pick has none", focusOf("s1") === undefined);
setFocus("s1", "RMX-1");
setFocus("s2", "RMX-9");
check("a pick is remembered per session", focusOf("s1") === "RMX-1" && focusOf("s2") === "RMX-9");
setFocus("s1", "RMX-2");
check("re-picking replaces rather than appends", focusOf("s1") === "RMX-2");
// "Not on a ticket" is a real answer and has to be settable, or the only way
// out of a pick is another pick.
setFocus("s1", null);
check("clearing a pick removes it", focusOf("s1") === undefined && focusOf("s2") === "RMX-9");

// ── today's list ─────────────────────────────────────────────────────────────

reset();
check("an unset day has an empty list", planFor().length === 0);
togglePlan("RMX-1");
togglePlan("RMX-2");
check("toggling adds, in the order chosen", JSON.stringify(planFor()) === '["RMX-1","RMX-2"]');
togglePlan("RMX-1");
check("toggling again removes", JSON.stringify(planFor()) === '["RMX-2"]');
setPlanFor(["RMX-3", "RMX-3", "RMX-4"]);
check("a duplicate cannot get in", JSON.stringify(planFor()) === '["RMX-3","RMX-4"]');

// A plan belongs to its day. Yesterday's list showing up as today's is the
// failure that makes the whole bar meaningless.
const yesterday = dayKey(new Date(Date.now() - 86_400_000));
setPlanFor(["RMX-9"], yesterday);
check("yesterday's list is not today's", !planFor().includes("RMX-9"));
check("but it is still there under its own day", planFor(yesterday)[0] === "RMX-9");

// Old plans are dropped on write, so the store is bounded by construction.
const ancient = dayKey(new Date(Date.now() - 400 * 86_400_000));
setPlanFor(["RMX-OLD"], ancient);
setPlanFor(["RMX-3"]);
check("an ancient plan is pruned on the next write", !readPlan()[ancient]);

// ── resolving against the board ──────────────────────────────────────────────

const board: JiraIssue[] = [
  issue("RMX-1", "Shipped", "done"),
  issue("RMX-2", "In Review", "inprogress"),
  issue("RMX-3", "To Do", "todo"),
];

check(
  "the plan resolves in the order it was chosen, not the board's",
  planIssues(board, ["RMX-3", "RMX-1"]).map((i) => i.key).join() === "RMX-3,RMX-1",
);
// A key that no longer resolves means the issue was unassigned, moved or
// deleted — a row for it would invite clicking on something that is not there.
check("a key the board no longer has is dropped", planIssues(board, ["RMX-1", "RMX-404"]).length === 1);
check("matching is case-insensitive", planIssues(board, ["rmx-2"])[0]?.key === "RMX-2");
check(
  "progress counts done against the chosen, not the whole board",
  JSON.stringify(planProgress(planIssues(board, ["RMX-1", "RMX-3"]))) === '{"done":1,"total":2}',
);
check("an empty plan is not 100% done", JSON.stringify(planProgress([])) === '{"done":0,"total":0}');

// ── restore ──────────────────────────────────────────────────────────────────

reset();
if (savedFocus !== null) localStorage.setItem("rmux.jira.focus", savedFocus);
if (savedPlan !== null) localStorage.setItem("rmux.jira.plan", savedPlan);

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
  failures ? "background:#e63b2e;color:#fff" : "background:#2b7;color:#000",
);
