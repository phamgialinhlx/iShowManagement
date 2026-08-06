/**
 * Checkbox parsing, toggling and list continuation.
 *
 * Open http://localhost:5273/note-tasks-check.html and read the console.
 *
 * These are pure string functions, and they are the definition of "what is a
 * task" that both the note widget and the dashboard depend on. Two things make
 * them worth pinning: the toggle rewrites the operator's own text, so an
 * off-by-one corrupts a note rather than merely miscounting; and the count
 * drives a progress bar, where being quietly wrong looks exactly like being
 * right.
 */
import { continueList, noteTasks, taskProgress, toggleTask } from "./src/lib/note-tasks";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) {
    console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  } else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

// ── finding tasks ────────────────────────────────────────────────────────────

const NOTE = [
  "# Today",
  "",
  "- [ ] ask ops about the cert",
  "- [x] restart the worker",
  "  - [ ] nested still counts",
  "* [X] a star bullet, capital X",
  "- not a task",
  "1. also not a task",
].join("\n");

check("every checkbox is found, including nested and starred", noteTasks(NOTE).length === 4);
check("a plain bullet is not a task", !noteTasks(NOTE).some((t) => t.label === "not a task"));
check("done and total are counted", JSON.stringify(taskProgress(NOTE)) === JSON.stringify({ done: 2, total: 4 }));
// Obsidian and friends write `x`, `X` and `/`. Treating an unfamiliar mark as
// "not a task" would silently drop it from the bar rather than mis-state it.
check("a capital X counts as done", noteTasks(NOTE)[3]?.done === true);
check("the label has the marker stripped", noteTasks(NOTE)[0]?.label === "ask ops about the cert");
check("a note with no checkboxes has no tasks", taskProgress("just prose\nand more").total === 0);

// ── toggling ─────────────────────────────────────────────────────────────────

check(
  "toggling an unchecked task checks it",
  toggleTask("- [ ] one", 0) === "- [x] one",
);
check(
  "toggling a checked task unchecks it",
  toggleTask("- [x] one", 0) === "- [ ] one",
);
check(
  "indentation and bullet style survive a toggle",
  toggleTask("   * [ ] deep", 0) === "   * [x] deep",
);
check(
  "only the named line changes",
  toggleTask("- [ ] a\n- [ ] b", 1) === "- [ ] a\n- [x] b",
);
// The click handler can race an edit made in another window; a stale index must
// not corrupt the note.
check("a line that is not a task is returned unchanged", toggleTask("prose", 0) === "prose");
check("an out-of-range line is returned unchanged", toggleTask("- [ ] a", 99) === "- [ ] a");
check("an explicit state is honoured rather than flipped", toggleTask("- [x] a", 0, true) === "- [x] a");

// ── continuing a list ────────────────────────────────────────────────────────

check("Enter after a task starts another task", continueList("- [ ] one") === "- [ ] ");
check("Enter after a done task starts an *empty* one", continueList("- [x] one") === "- [ ] ");
check("indentation is carried down", continueList("  - [ ] one") === "  - [ ] ");
check("a plain bullet continues as a bullet", continueList("- one") === "- ");
check("a numbered list increments", continueList("3. one") === "4. ");
check("the numbered delimiter is preserved", continueList("3) one") === "4) ");
// Otherwise the only way out of a list is to backspace over punctuation you
// never typed.
check("an empty item ends the list", continueList("- [ ] ") === null);
check("an empty bullet ends the list", continueList("- ") === null);
check("prose does not continue anything", continueList("just writing") === null);

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
  failures ? "background:#e63b2e;color:#000" : "background:#2b7;color:#000",
);
