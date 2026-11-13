/**
 * Putting a fullscreen Claude's mouse back after a replay that lost it.
 *
 * The daemon replays a bounded ring of raw output. A fullscreen TUI turns its
 * modes on once, at startup, and then redraws for hours — so on a session that
 * ran detached for a few minutes (an agent-spawned child, adopted late) the
 * bytes that enabled mouse tracking have been drained, while later output still
 * puts xterm on the **alternate screen**. xterm with the alt buffer active and
 * no mouse mode translates the wheel into ↑/↓, so every wheel step recalls an
 * earlier prompt in Claude's composer instead of scrolling.
 *
 * Agents built since now mirror the modes in front of the replay
 * (`rmux-term`'s `modes.rs`). This is the net for sessions still served by an
 * older daemon, which keep their old code until they end.
 *
 * It is deliberately narrow. Only a **Claude** session launched **fullscreen**
 * (`CLAUDE_CODE_NO_FLICKER=1`, which keeps the mouse on) is known to want mouse
 * reports; a shell, an inline session, pi, Codex or dsh get nothing, because
 * enabling reporting under a program that did not ask feeds it junk input.
 * Never while SELECT holds the mouse off on purpose. Written into xterm, never
 * to the pty: this is terminal state, and Claude is not told.
 *
 * The decision is read from xterm's own state (`modes.mouseTrackingMode`),
 * not from a tracker of what was seen: an encoding (`?1006h`) or focus
 * reporting (`?1004h`) surviving in the ring without a tracking mode leaves a
 * tracker non-empty while the wheel is still turned into arrow keys.
 */

import { AGENT_MOUSE_MODES } from "./mouse-modes.ts";

export type ReplayModeInput = {
  provider: string;
  /** The session's setting; unset means fullscreen, as `claude_start` reads it. */
  fullscreen: boolean | undefined;
  buffer: "normal" | "alternate";
  /** `xterm.modes.mouseTrackingMode`: `none`, `x10`, `vt200`, `drag` or `any`. */
  mouseTracking: string;
  selectMode: boolean;
};

/** The sequences to write into xterm, or `""` when nothing should change. */
export function replayMouseRepair(input: ReplayModeInput): string {
  if (input.provider !== "claude") return "";
  if (input.fullscreen === false) return "";
  if (input.selectMode) return "";
  if (input.buffer !== "alternate") return "";
  if (input.mouseTracking !== "none") return "";
  return AGENT_MOUSE_MODES;
}
