/**
 * The note widget, in a real browser, with a real click.
 *
 * `note-tasks-check.ts` proves the *string* functions. It passed 22/22 while
 * ticking a box in the running app did nothing — because the fault was never in
 * the parsing, it was in whether a click reaches the input at all. That is a
 * DOM question, and only a real click can answer it.
 *
 * Open http://localhost:5273/note-widget-check.html and read the console.
 */
import { createRoot } from "react-dom/client";
import { createElement } from "react";

import { Note } from "./src/components/widgets/Note";
import "./src/styles/signal-room.css";

const SESSION = "note-widget-check";
localStorage.setItem(
  `rmux.note.${SESSION}`,
  ["# Today", "", "- [ ] one", "- [x] two", "- [ ] three"].join("\n"),
);

const host = document.createElement("div");
host.style.cssText = "width:260px;padding:8px";
document.body.append(host);
createRoot(host).render(createElement(Note, { sessionId: SESSION }));

let failures = 0;
const check = (what: string, ok: boolean) => {
  if (ok) console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
};

const settle = () => new Promise((r) => setTimeout(r, 120));

await settle();

const boxes = () => Array.from(host.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'));
const stored = () => localStorage.getItem(`rmux.note.${SESSION}`) ?? "";
/** Saves are debounced; reading sooner measures the debounce, not the feature. */
const saved = () => new Promise((r) => setTimeout(r, 600));

check("three checkboxes are rendered", boxes().length === 3);
// A disabled input receives no pointer events at all, so it would look exactly
// like a handler that is not wired up. `remark-gfm` marks task boxes disabled
// by default, which is why this is asserted rather than assumed.
check("no checkbox is disabled", boxes().every((b) => !b.disabled));
check("the second box reflects `- [x]`", boxes()[1]?.checked === true);
check("the progress counter reads the note", (host.textContent ?? "").includes("1/3"));

// **Click the third, not the first.** Two separate bugs made every box toggle
// line 0 — a drifting render counter, then a null parser position — and both
// survived a test that only ever clicked the first box, because ticking task 0
// by clicking task 0 is indistinguishable from ticking whatever you clicked.
boxes()[2]?.click();
await saved();
check("clicking the *third* box ticks the third task", /^- \[x\] three$/m.test(stored()));
check("...and leaves the first alone", /^- \[ \] one$/m.test(stored()));
check("the note did not fall into edit mode", host.querySelector("textarea") === null);

boxes()[2]?.click();
await saved();
check("clicking it again unticks it", /^- \[ \] three$/m.test(stored()));

boxes()[1]?.click();
await saved();
check("an already-done task can be undone", /^- \[ \] two$/m.test(stored()));

// ── editing ─────────────────────────────────────────────────────────────────
//
// Reported as "where is the cursor when i click on note i cant edit". Ticking a
// box must *not* open an editor, and clicking the text must — two behaviours on
// the same element, so both are pinned.

// **A full press, not `.click()`.** `element.click()` fires only a `click`
// event — no `mousedown` at all — so it cannot exercise an interaction that
// begins on mousedown, and reported a broken editor that was merely untested.
// The earlier `dispatchEvent(mousedown)` had the opposite flaw: it fired the
// handler but not the browser's default focus handling, which is where the real
// bug lived. Only the full sequence exercises both.
const press = (el: Element) => {
  for (const type of ["pointerdown", "mousedown", "pointerup", "mouseup", "click"]) {
    el.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true }));
  }
};
const target = host.querySelector<HTMLElement>(".note-rendered");
if (target) press(target);
await settle();

const area = host.querySelector("textarea");
check("clicking the rendered note opens a textarea", area !== null);
check("the textarea holds the whole note as raw markdown", (area?.value ?? "").includes("# Today") && (area?.value ?? "").includes("three"));
check("the textarea has focus, so there is a caret", document.activeElement === area);

// Typing must land in the note rather than being swallowed.
if (area) {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  setter?.call(area, "# Tomorrow");
  area.dispatchEvent(new Event("input", { bubbles: true }));
}
await new Promise((r) => setTimeout(r, 600));
check("editing writes through to storage", stored().startsWith("# Tomorrow"));

// Click-away is the documented way back to rendered, so it is pinned.
host.querySelector("textarea")?.blur();
await settle();
check("clicking away renders it again", host.querySelector("textarea") === null);
check("the rendered note is back", host.querySelector(".note-rendered") !== null);

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
  failures ? "background:#e63b2e;color:#000" : "background:#2b7;color:#000",
);
