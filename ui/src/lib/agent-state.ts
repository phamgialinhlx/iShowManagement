/**
 * The five agent states the sidebar shows, in herdr's vocabulary.
 *
 * This is the **one** place a session's runtime is turned into what the operator
 * sees. The rail, its roll-ups (tab, project, server, a folded group), the
 * collapsed column and the agents list all ask here, so they cannot disagree
 * about whether something needs attention.
 *
 * | state     | from                                         | priority |
 * |-----------|----------------------------------------------|----------|
 * | `blocked` | `waiting` — Claude is asking something       | 4        |
 * | `done`    | idle, and its last change is newer than seen | 3        |
 * | `working` | `working`                                    | 2        |
 * | `idle`    | idle, already looked at                      | 1        |
 * | `unknown` | an agent with no runtime entry yet           | 0        |
 *
 * `done` outranks `working` on purpose: a run that finished while you were
 * elsewhere is waiting for *you*, a running one is not. `unknown` is the floor,
 * and it is a real state rather than a fallback to idle — reporting "idle" for a
 * session nothing has observed yet is inventing a measurement.
 *
 * A **terminal** has no state here at all (`null`), not `unknown`: it is not an
 * agent, and a grey dot beside every shell would read as "something is wrong
 * with all my terminals".
 *
 * Pure, no runtime imports — `ui/agent-state-check.ts` runs it under Node.
 */

export type AgentState = "blocked" | "done" | "working" | "idle" | "unknown";

/** Only the fields this module reads — kept structural so the store's own
 *  `Runtime` type satisfies it without an import. */
export type RuntimeLike = { status: "waiting" | "working" | "idle"; lastStatusAt?: number };

export const STATE_PRIORITY: Record<AgentState, number> = {
  blocked: 4,
  done: 3,
  working: 2,
  idle: 1,
  unknown: 0,
};

export function isAgentKind(kind: string): boolean {
  return kind === "claude" || kind === "pi" || kind === "dsh" || kind === "codex";
}

/** Finished and not looked at: idle, with a change newer than the watermark. */
export function isUnseen(runtime: RuntimeLike | undefined, seenAt: number | undefined): boolean {
  return !!runtime && runtime.status === "idle" && (runtime.lastStatusAt ?? 0) > (seenAt ?? 0);
}

/** The state of one session, or `null` for a session that is not an agent. */
export function agentState(
  kind: string,
  runtime: RuntimeLike | undefined,
  seenAt: number | undefined,
): AgentState | null {
  if (!isAgentKind(kind)) return null;
  if (!runtime) return "unknown";
  if (runtime.status === "waiting") return "blocked";
  if (runtime.status === "working") return "working";
  return isUnseen(runtime, seenAt) ? "done" : "idle";
}

/**
 * The most urgent of several states — what a tab, project, server or folded
 * group shows. `null` when none of them is an agent (a project of terminals
 * shows no mark rather than a misleading one).
 */
export function rollup(states: Iterable<AgentState | null | undefined>): AgentState | null {
  let best: AgentState | null = null;
  for (const s of states) {
    if (!s) continue;
    if (best === null || STATE_PRIORITY[s] > STATE_PRIORITY[best]) best = s;
  }
  return best;
}

/** "Needs you" is the two states that are waiting on the operator. */
export function needsYou(state: AgentState | null): boolean {
  return state === "blocked" || state === "done";
}

export type AgentSort = "grouped" | "priority";

/**
 * Order the agents list.
 *
 * - `grouped` keeps the given order (machine, then project, as the spaces list
 *   shows them), so the two lists read in the same sequence.
 * - `priority` puts the most urgent first, then the most recently changed —
 *   two blocked sessions are ordered by which asked most recently. Ties fall
 *   back to the grouped order, so the sort is stable and rows do not shuffle
 *   between renders.
 */
export function sortAgents<T extends { state: AgentState; lastStatusAt?: number }>(
  rows: readonly T[],
  mode: AgentSort,
): T[] {
  const indexed = rows.map((row, i) => ({ row, i }));
  if (mode === "priority") {
    indexed.sort(
      (a, b) =>
        STATE_PRIORITY[b.row.state] - STATE_PRIORITY[a.row.state] ||
        (b.row.lastStatusAt ?? 0) - (a.row.lastStatusAt ?? 0) ||
        a.i - b.i,
    );
  }
  return indexed.map((x) => x.row);
}

// ── indicators ───────────────────────────────────────────────────────────────

export type IndicatorStyle = "dots" | "symbols";

/** Static glyphs — nothing here animates. A spinner says "working" no better
 *  than a yellow dot does, and it is the one thing on screen that never stops
 *  moving. */
export const GLYPHS: Record<IndicatorStyle, Record<AgentState, string>> = {
  dots: { working: "●", blocked: "●", done: "●", idle: "○", unknown: "·" },
  symbols: { working: "◐", blocked: "×", done: "✓", idle: "○", unknown: "·" },
};

/** The theme token for each state; the same in both styles. */
export const STATE_COLOR: Record<AgentState, string> = {
  working: "var(--st-working)",
  blocked: "var(--st-blocked)",
  done: "var(--st-done)",
  idle: "var(--st-idle)",
  unknown: "var(--st-unknown)",
};

export const STATE_LABEL: Record<AgentState, string> = {
  working: "working",
  blocked: "blocked — needs an answer",
  done: "done — not looked at yet",
  idle: "idle",
  unknown: "no status yet",
};

/** Read from a stored appearance object; anything unrecognised is the default. */
export function parseIndicatorStyle(raw: unknown): IndicatorStyle {
  return raw === "symbols" ? "symbols" : "dots";
}
