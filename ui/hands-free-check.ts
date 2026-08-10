/**
 * When does hands-free move the cursor, and when must it not?
 *
 * Open http://localhost:5273/hands-free-check.html and read the console.
 *
 * The whole feature is one decision, and both ways of getting it wrong are
 * worse than not having it:
 *
 *  - move too eagerly and the cursor is dragged around while someone is typing,
 *    which sends the rest of a sentence to another machine;
 *  - move on the *state* rather than the edge and it jumps to the same idle pane
 *    forever, since idle is the resting state of most panes most of the time.
 *
 * `nextHandsFreeTarget` is pure precisely so this can be checked without a grid,
 * a store or a clock.
 */
import { nextHandsFreeTarget } from "./src/lib/hands-free";
import type { PaneRef, SessionStatus } from "./src/lib/workspace-model";

let failures = 0;
const check = (name: string, ok: boolean, detail = "") => {
  if (ok) console.log(`%c PASS %c ${name}${detail ? ` — ${detail}` : ""}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name}${detail ? ` — ${detail}` : ""}`);
  }
};

const panes: (PaneRef | null)[] = [
  { kind: "session", id: "a" },
  { kind: "session", id: "b" },
  { kind: "files", projectId: "p" },
  null,
];

type S = Record<string, SessionStatus | undefined>;
const target = (before: S, after: S, activeSession: string | null = null) =>
  nextHandsFreeTarget({ panes, before, after, activeSession });

// 1. The edge: working → idle is the moment a session becomes answerable.
{
  const t = target({ a: "working", b: "working" }, { a: "idle", b: "working" });
  check("a session that just finished takes the cursor", t?.id === "a" && t.cell === 0, JSON.stringify(t));
}

// 2. The state is not the edge. This is the difference between a useful mode and
//    one that drags the cursor to the same pane for as long as it is on.
{
  const t = target({ a: "idle", b: "working" }, { a: "idle", b: "working" });
  check("a session that was already idle is left alone", t === null, JSON.stringify(t));
}

// 3. A question outranks a session that merely stopped: someone is waiting on
//    the other end of it.
{
  const t = target({ a: "working", b: "working" }, { a: "idle", b: "waiting" });
  check("waiting wins over idle", t?.id === "b", JSON.stringify(t));
}

// 4. Never jump to the pane the operator is already in — there is nothing to
//    move, and doing it would steal focus from whatever they clicked into.
{
  const t = target({ a: "working" }, { a: "idle" }, "a");
  check("the active session is not a destination", t === null, JSON.stringify(t));
}

// 5. First sight is not an edge. Without this, switching the mode on grabs the
//    keyboard immediately, from wherever the operator happened to be.
{
  const t = target({}, { a: "idle", b: "idle" });
  check("a session seen for the first time does not trigger", t === null, JSON.stringify(t));
}

// 6. Only sessions actually on screen. A pane holding files, a host or nothing
//    has no terminal, and a session in no pane at all cannot be typed into.
{
  const t = target({ ghost: "working" }, { ghost: "idle" });
  check("a session that is in no pane is ignored", t === null, JSON.stringify(t));
}

// 7. Still working is not answerable.
{
  const t = target({ a: "idle" }, { a: "working" });
  check("a session that just started working is not a destination", t === null, JSON.stringify(t));
}

// 8. **Switching it on acts on the state, not on an edge.**
//
//    The reported bug: turn it on with a grid of idle sessions, type, and the
//    letters go nowhere. There is no transition to react to, so the edge rule —
//    correct while the mode runs — meant nothing was ever focused.
{
  const idle: S = { a: "idle", b: "idle" };
  check(
    "with no edge, the running mode stays put",
    target(idle, idle) === null,
    "unchanged statuses",
  );
  const armed = nextHandsFreeTarget({
    panes,
    before: idle,
    after: idle,
    activeSession: null,
    arm: true,
  });
  check(
    "but arming takes the first answerable session",
    armed?.id === "a" && armed.cell === 0,
    JSON.stringify(armed),
  );
}

// 9. Arming still prefers a question, and still refuses the pane you are in.
{
  const now: S = { a: "idle", b: "waiting" };
  const armed = nextHandsFreeTarget({
    panes,
    before: now,
    after: now,
    activeSession: null,
    arm: true,
  });
  check("arming prefers a waiting session", armed?.id === "b", JSON.stringify(armed));

  const inA = nextHandsFreeTarget({
    panes,
    before: { a: "idle" },
    after: { a: "idle" },
    activeSession: "a",
    arm: true,
  });
  check("arming skips the active session", inA === null, JSON.stringify(inA));
}

// 10. **The pane list must be what is on screen.**
//
//     `layoutPanes` auto-fills empty cells, so a grid showing four sessions sits
//     on a stored `panes` of `[null]`. Reading the store directly found nothing
//     and the mode silently did nothing at all — the actual reported failure.
{
  const stored: (PaneRef | null)[] = [null, null];
  const armed = nextHandsFreeTarget({
    panes: stored,
    before: { a: "idle" },
    after: { a: "idle" },
    activeSession: null,
    arm: true,
  });
  check(
    "an unassigned pane array yields nothing — so the caller must pass the laid-out one",
    armed === null,
    "this is why Workbench derives with layoutPanes",
  );
}

console.log(
  failures === 0
    ? "%c ALL PASS %c hands-free moves on the edge, prefers a question, and stays put otherwise"
    : `%c ${failures} FAILED %c`,
  failures === 0 ? "background:#2b7;color:#000" : "background:#e63b2e;color:#fff",
  "",
);
