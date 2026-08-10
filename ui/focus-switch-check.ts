/**
 * Does switching panes move the *cursor*, or only the highlight?
 *
 * Open http://localhost:5273/focus-switch-check.html and read the console.
 *
 * Moving between tiles updated state — the focus ring moved, the rail followed
 * — while the keyboard stayed wherever it already was. So you could switch to a
 * pane, type, and watch the characters arrive in the pane you had just left.
 * Reported as: "you just switch the focus, you did not focus typing cursor on
 * it".
 *
 * `document.activeElement` is the whole test. Anything short of that — a class,
 * a store field, a focus ring — is the thing that was already right while the
 * bug was live.
 */
import { FOCUS_EVENT, requestTerminalFocus } from "./src/lib/shortcuts";

let failures = 0;
const check = (name: string, ok: boolean, detail: string) => {
  if (ok) console.log(`%c PASS %c ${name} — ${detail}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name} — ${detail}`);
  }
};

/** A stand-in for a pane's terminal: something focusable that names itself. */
function pane(sessionId: string, answers: boolean): HTMLTextAreaElement {
  const el = document.createElement("textarea");
  el.dataset.session = sessionId;
  document.body.appendChild(el);
  if (answers) {
    window.addEventListener(FOCUS_EVENT, (e) => {
      const detail = (e as CustomEvent<{ sessionId: string }>).detail;
      if (detail?.sessionId === sessionId) el.focus();
    });
  }
  return el;
}

const a = pane("session-a", true);
const b = pane("session-b", true);
// The companion shares its conversation's id and must stay out of it.
const companion = pane("session-b", false);

a.focus();
check("starts in A", document.activeElement === a, "activeElement is A");

requestTerminalFocus("session-b");
check(
  "switching to B moves the cursor",
  document.activeElement === b,
  document.activeElement === b ? "activeElement is B" : "activeElement did NOT move",
);
check(
  "the companion did not steal it",
  document.activeElement !== companion,
  "a shell sharing the conversation's id must not answer",
);

requestTerminalFocus("session-a");
check("and back to A", document.activeElement === a, "activeElement is A");

// A tile holding files, a host, or nothing has no terminal. Nothing answers,
// and the cursor must simply stay put rather than being thrown somewhere.
requestTerminalFocus("session-with-no-terminal");
check(
  "an unaddressed pane leaves the cursor alone",
  document.activeElement === a,
  "still A",
);

[a, b, companion].forEach((el) => el.remove());
console.log(
  failures === 0
    ? "%c ALL PASS %c a pane switch moves the keyboard, not just the highlight"
    : `%c ${failures} FAILED %c`,
  failures === 0 ? "background:#2b7;color:#000" : "background:#e63b2e;color:#fff",
  "",
);
