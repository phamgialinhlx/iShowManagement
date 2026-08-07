/**
 * The companion shell's naming, clamping and split arithmetic.
 *
 * Open http://localhost:5273/companion-check.html and read the console.
 *
 * Two things here are worth pinning. The **name is derived**, because a minted
 * one would spawn a second shell on every restart instead of reattaching — the
 * duplicate-session failure that cost real work before `hand_off` existed. And
 * the **split clamps**, because a pane dragged to zero looks closed and leaves
 * a 1px handle as the only way back.
 */
import {
  clampSplit,
  companionKey,
  companionName,
  forget,
  readOpen,
  readSplit,
  splitFromPointer,
  writeOpen,
  writeSplit,
} from "./src/lib/companion";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

// ── naming ───────────────────────────────────────────────────────────────────

check("the name is derived from the session", companionName("abc") === "companion-abc");
// Stable across calls is the whole property: a fresh name each time would
// attach to nothing and spawn a second shell that nothing can reach.
check("and is stable", companionName("abc") === companionName("abc"));
// `claude-<id>` and a terminal's own ULID both live in the same namespace on
// the host; a collision would attach the conversation to its own shell.
check("it cannot collide with a claude session", companionName("abc") !== "claude-abc");
check("the live-map key is distinct from the session id", companionKey("abc") !== "abc");

// ── the split clamps at both ends ────────────────────────────────────────────

check("a normal split passes through", clampSplit(0.5) === 0.5);
check("dragging to the top clamps", clampSplit(0) === 0.2);
check("dragging past the bottom clamps", clampSplit(1) === 0.85);
check("nonsense clamps rather than propagating", clampSplit(-5) === 0.2);

// ── pointer → fraction ───────────────────────────────────────────────────────

check("mid-tile is a half", splitFromPointer(150, 100, 100) === 0.5);
check("the top of the tile clamps", splitFromPointer(100, 100, 100) === 0.2);
check("the bottom of the tile clamps", splitFromPointer(200, 100, 100) === 0.85);
// A tile measured at zero height happens for one frame while a pane mounts.
// Dividing by it would give Infinity and a NaN width.
check("a zero-height tile does not divide by zero", Number.isFinite(splitFromPointer(50, 0, 0)));

// ── remembered per session ───────────────────────────────────────────────────

const saved = localStorage.getItem("rmux.companion.open.s1");
forget("s1");
check("closed by default", readOpen("s1") === false);
writeOpen("s1", true);
check("opening is remembered", readOpen("s1") === true);
writeSplit("s1", 0.4);
check("the split is remembered", Math.abs(readSplit("s1") - 0.4) < 1e-9);
// Two sessions must not share a shell or a layout — the point of the feature is
// that switching brings *that* session's terminal.
check("another session is unaffected", readOpen("s2") === false);
forget("s1");
check("forgetting clears both", readOpen("s1") === false && readSplit("s1") > 0.5);

if (saved === null) localStorage.removeItem("rmux.companion.open.s1");
else localStorage.setItem("rmux.companion.open.s1", saved);

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
  failures ? "background:#e63b2e;color:#fff" : "background:#2b7;color:#000",
);
