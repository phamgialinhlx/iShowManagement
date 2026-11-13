/**
 * The sidebar's agent states (`src/lib/agent-state.ts`): the mapping from a
 * session's runtime to herdr's five states, the priority roll-up a tab /
 * project / server shows, and the agents list's `priority` sort.
 *
 *     node --experimental-strip-types ui/agent-state-check.ts
 *
 * Every failure here is silent in the app: a roll-up that loses to the wrong
 * state is a project that says "working" while a session in it is waiting on a
 * question, and a terminal mapped to `unknown` is a grey dot beside every shell.
 */
import {
  GLYPHS,
  STATE_PRIORITY,
  agentState,
  needsYou,
  parseIndicatorStyle,
  rollup,
  sortAgents,
  type AgentState,
} from "./src/lib/agent-state.ts";
import { mintStem, reattachName } from "./src/lib/workspace-model.ts";

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    if (!ok) failures += 1;
    log(`${ok ? "PASS" : "FAIL"}  ${name}${ok || !detail ? "" : `  — ${detail}`}`);
  };

  // ── mapping ──
  check("a terminal has no state (not unknown)", agentState("terminal", { status: "working" }, 0) === null);
  check("an agent with no runtime is unknown", agentState("claude", undefined, undefined) === "unknown");
  check("pi is an agent too", agentState("pi", undefined, undefined) === "unknown");
  check("waiting → blocked", agentState("claude", { status: "waiting", lastStatusAt: 5 }, 0) === "blocked");
  check("working → working", agentState("claude", { status: "working", lastStatusAt: 5 }, 0) === "working");
  check("idle, newer than seen → done", agentState("claude", { status: "idle", lastStatusAt: 10 }, 5) === "done");
  check("idle, already seen → idle", agentState("claude", { status: "idle", lastStatusAt: 10 }, 10) === "idle");
  check("idle with no stamp → idle", agentState("claude", { status: "idle" }, undefined) === "idle");
  check("waiting is never 'done', however new", agentState("claude", { status: "waiting", lastStatusAt: 99 }, 0) === "blocked");

  // ── priority ──
  check(
    "priority order is blocked > done > working > idle > unknown",
    STATE_PRIORITY.blocked > STATE_PRIORITY.done &&
      STATE_PRIORITY.done > STATE_PRIORITY.working &&
      STATE_PRIORITY.working > STATE_PRIORITY.idle &&
      STATE_PRIORITY.idle > STATE_PRIORITY.unknown,
  );
  check("rollup of nothing is null", rollup([]) === null);
  check("rollup of terminals only is null", rollup([null, null]) === null);
  check("rollup takes the max", rollup(["idle", "working", null, "unknown"]) === "working");
  check("done beats working", rollup(["working", "done"]) === "done");
  check("blocked beats everything", rollup(["done", "blocked", "working", "idle"]) === "blocked");
  check("unknown is kept when it is all there is", rollup(["unknown", null]) === "unknown");
  check("idle beats unknown", rollup(["unknown", "idle"]) === "idle");
  check("needs-you is blocked and done only", needsYou("blocked") && needsYou("done") && !needsYou("working") && !needsYou("idle") && !needsYou("unknown") && !needsYou(null));

  // ── sort ──
  type Row = { id: string; state: AgentState; lastStatusAt?: number };
  const rows: Row[] = [
    { id: "a", state: "idle", lastStatusAt: 50 },
    { id: "b", state: "working", lastStatusAt: 10 },
    { id: "c", state: "blocked", lastStatusAt: 1 },
    { id: "d", state: "done", lastStatusAt: 30 },
    { id: "e", state: "blocked", lastStatusAt: 9 },
    { id: "f", state: "unknown" },
    { id: "g", state: "working", lastStatusAt: 10 },
  ];
  const grouped = sortAgents(rows, "grouped").map((r) => r.id).join("");
  check("grouped keeps the given order", grouped === "abcdefg", grouped);
  const prio = sortAgents(rows, "priority").map((r) => r.id).join("");
  check("priority: state first, then most recent, ties stable", prio === "ecdbgaf", prio);
  check("sorting does not mutate the input", rows.map((r) => r.id).join("") === "abcdefg");

  // ── indicators ──
  check("default style is dots", parseIndicatorStyle(undefined) === "dots" && parseIndicatorStyle("nonsense") === "dots");
  check("symbols parses", parseIndicatorStyle("symbols") === "symbols");
  check(
    "dots: filled for working/blocked/done, hollow idle, dim unknown",
    GLYPHS.dots.working === "●" && GLYPHS.dots.blocked === "●" && GLYPHS.dots.done === "●" && GLYPHS.dots.idle === "○" && GLYPHS.dots.unknown === "·",
  );
  check(
    "symbols: ◐ × ✓ ○ ·",
    GLYPHS.symbols.working === "◐" && GLYPHS.symbols.blocked === "×" && GLYPHS.symbols.done === "✓" && GLYPHS.symbols.idle === "○" && GLYPHS.symbols.unknown === "·",
  );

  // DeepSeek Harness: an agent kind (the rail shows its state), `unknown` until
  // its server reports, and a host name of `dsh-<id>` from a bare stem.
  check("dsh is an agent kind", agentState("dsh", undefined, undefined) === "unknown");
  check("dsh idle maps to idle", agentState("dsh", { status: "idle" }, undefined) === "idle");
  check("dsh working maps to working", agentState("dsh", { status: "working" }, undefined) === "working");
  check("dsh waiting maps to blocked", agentState("dsh", { status: "waiting" }, undefined) === "blocked");
  check("dsh stem is bare", mintStem("dsh") === "dshsession" && !mintStem("dsh").startsWith("dsh-"));
  const dshId = `${mintStem("dsh")}-abc-1`;
  check("dsh host name", reattachName({ id: dshId, kind: "dsh" }) === "dsh-dshsession-abc-1");
  check("adopted dsh keeps its host name", reattachName({ id: dshId, kind: "dsh", hostName: "dsh-other" }) === "dsh-other");
  check("other stems unchanged", mintStem("claude") === "session" && mintStem("pi") === "pisession" && mintStem("terminal") === "term");

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}

if (typeof window === "undefined") {
  const ok = run((line) => console.log(line));
  if (!ok) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
}
