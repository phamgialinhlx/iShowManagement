import { invoke } from "@tauri-apps/api/core";

import { isTauri } from "./api";
import { useWorkspace } from "./workspace";

/**
 * Keep every session's status current, not only the ones on screen.
 *
 * ## The bug this exists for
 *
 * Status was published by `ClaudePanel`, and a `ClaudePanel` only exists for a
 * session that is *rendered*. In a 2x2 grid four of eight sessions have no
 * panel, so nothing ever told the rail what they were doing and they sat on
 * whatever they last reported — usually "idle", forever.
 *
 * That is the rail's entire purpose failing: it exists to answer "which of my
 * machines needs me right now?" for the sessions you are **not** looking at.
 * For the ones on screen you can simply look.
 *
 * ## Why this can work at all
 *
 * A `ClaudeSession` lives in Rust and outlives its view — unmounting a pane
 * detaches, it does not stop the conversation. So `claude_state` answers for
 * any session started in this run of the app, whether or not it is rendered.
 *
 * ## Slower than the panel's own poll, deliberately
 *
 * The visible pane polls at 400ms because its own header shows the answer. This
 * is one IPC round trip per off-screen session, and a dot in a rail does not
 * need four updates a second — 1.5s is well inside the time it takes to look up
 * from what you were doing.
 */
const INTERVAL_MS = 1500;

let timer: number | null = null;

export function startStatusWatch(): () => void {
  if (!isTauri() || timer !== null) return () => {};

  const tick = async () => {
    const state = useWorkspace.getState();
    const { live, sessions, setStatus } = state;

    await Promise.all(
      sessions.map(async (session) => {
        // Terminals are status-neutral in v1 (ADR-001); only Claude reports.
        if (session.kind !== "claude") return;
        const claudeId = live[session.id];
        // No handle means this session has not been opened in this run, so
        // there is genuinely nothing to report. Leaving it alone is honest;
        // guessing "idle" would be inventing a measurement.
        if (!claudeId) return;

        try {
          const next = await invoke<{ prompt: unknown; working: boolean; exited?: number }>(
            "claude_state",
            { id: claudeId },
          );
          // A session whose attachment died is not idle — it is unknown, and
          // the pane's own reconnect handles it. Touching status here would
          // race that.
          if (next.exited != null) return;

          setStatus(session.id, next.prompt ? "waiting" : next.working ? "working" : "idle");
        } catch {
          // The handle is gone. Same reasoning: the panel owns recovery.
        }
      }),
    );
  };

  void tick();
  timer = window.setInterval(() => void tick(), INTERVAL_MS);

  return () => {
    if (timer !== null) window.clearInterval(timer);
    timer = null;
  };
}
