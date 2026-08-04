/**
 * Autosave, and the four ways it could destroy a file instead of saving one.
 *
 * Open http://localhost:5273/autosave-check.html and read the console.
 *
 * The timing half runs against real `setTimeout`, because the bug this guards
 * against is a *scheduling* one: a pending write cancelled by something the
 * operator did not connect to saving (switching tabs, closing a file), or an
 * in-flight write swallowing the edits made during it.
 */
import {
  autosaveDecision,
  autosaveEnabled,
  autosavePending,
  cancelAutosave,
  scheduleAutosave,
  setAutosaveEnabled,
  AUTOSAVE_DELAY,
} from "./src/lib/autosave";

let failures = 0;
const check = (name: string, ok: boolean, detail: string) => {
  if (ok) console.log(`%c PASS %c ${name} — ${detail}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name} — ${detail}`);
  }
};

const buffer = (over: Partial<Parameters<typeof autosaveDecision>[0]> = {}) => ({
  loading: false,
  error: null as string | null,
  content: { kind: "text" },
  saving: false,
  text: "changed",
  saved: "original",
  ...over,
});

// --- the refusals. each of these is a file lost if it returns "write" --------

check(
  "a file still loading is never written back",
  autosaveDecision(buffer({ loading: true, text: "", saved: "" })) === "skip",
  "the buffer is empty until the read lands — writing it truncates the real file",
);

check(
  "a file whose read FAILED is never written back",
  autosaveDecision(buffer({ error: "connection reset", text: "", saved: "" })) === "skip",
  "the worst case: an error is on screen saying it could not be read, while it is overwritten",
);

check(
  "a binary or image buffer is never written back",
  autosaveDecision(buffer({ content: { kind: "binary" } })) === "skip",
  "there is no editable text to write",
);

check(
  "an unchanged file is not rewritten",
  autosaveDecision(buffer({ text: "same", saved: "same" })) === "skip",
  "an identical rewrite still bumps mtime, which is enough to trip a watcher or a build",
);

check(
  "a missing buffer is not an error",
  autosaveDecision(undefined) === "skip",
  "the tab can close between the timer firing and the callback running",
);

// --- the one that must NOT be a refusal --------------------------------------

check(
  "a write already in flight schedules a RETRY, not a skip",
  autosaveDecision(buffer({ saving: true })) === "retry",
  "skipping would drop every edit made during the write — silent loss, which is the whole point",
);

check(
  "an ordinary dirty text buffer writes",
  autosaveDecision(buffer()) === "write",
  "the common case",
);

// --- the preference ----------------------------------------------------------

{
  const before = localStorage.getItem("rmux.editor.autosave");
  localStorage.removeItem("rmux.editor.autosave");
  check(
    "absent preference means ON",
    autosaveEnabled(),
    "a first run must autosave; reading 'no preference' as off opts out everyone silently",
  );

  setAutosaveEnabled(false);
  check("turning it off persists", !autosaveEnabled(), "stored as '0'");
  setAutosaveEnabled(true);
  check("turning it back on persists", autosaveEnabled(), "stored as '1'");

  if (before === null) localStorage.removeItem("rmux.editor.autosave");
  else localStorage.setItem("rmux.editor.autosave", before);
}

// --- scheduling, against the real clock --------------------------------------

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

await (async () => {
  let runs = 0;
  for (let i = 0; i < 5; i++) scheduleAutosave("burst", () => runs++, 60);
  check("a burst of edits collapses to one write", autosavePending("burst"), "still pending");
  await sleep(140);
  check(
    "five edits produced one save, not five",
    runs === 1,
    `ran ${runs} time(s) — each save is a whole-file write across the network`,
  );

  // Two files must not cancel each other. Same path in two sessions is routine.
  let a = 0;
  let b = 0;
  scheduleAutosave("session1/main.rs", () => a++, 40);
  scheduleAutosave("session2/main.rs", () => b++, 40);
  await sleep(120);
  check(
    "two buffers save independently",
    a === 1 && b === 1,
    `a=${a} b=${b} — keying by path alone would let one file's edit cancel another's save`,
  );

  let cancelled = 0;
  scheduleAutosave("gone", () => cancelled++, 40);
  cancelAutosave("gone");
  await sleep(100);
  check("a cancelled save does not fire", cancelled === 0, "used when a buffer is closed");
  check("and stops reporting as pending", !autosavePending("gone"), "no leaked timer");
})();

check(
  "the delay is long enough to be a pause, not a keystroke",
  AUTOSAVE_DELAY >= 800,
  `${AUTOSAVE_DELAY}ms — a remote save is a whole-file write; per-keystroke would queue faster than it drains`,
);

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASSED ",
  `background:${failures ? "#e63b2e" : "#2b7"};color:#000;font-weight:bold`,
);
