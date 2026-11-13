import { STATE_COLOR, STATE_LABEL, type AgentState } from "../lib/agent-state";

/** herdr's static dot for an agent state, in the desktop rail's colours: filled
 *  for working / blocked / done, a ring for idle, a speck for unknown. Drawn in
 *  CSS rather than with the rail's `●` glyph, which the body face renders as
 *  something else entirely. The label beside it carries the meaning for anyone
 *  who cannot tell the colours apart. */
export function StateDot({ state }: { state: AgentState }) {
  return <span className={`m-dot m-dot-${state}`} style={{ color: STATE_COLOR[state] }} aria-hidden="true" />;
}

/** Short words for a phone row; `STATE_LABEL` is the long form, used as a title. */
export const SHORT_STATE: Record<AgentState, string> = {
  blocked: "needs an answer",
  done: "finished",
  working: "working",
  idle: "idle",
  unknown: "no status yet",
};

export { STATE_LABEL };
