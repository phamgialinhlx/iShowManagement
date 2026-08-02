import { invoke } from "@tauri-apps/api/core";

import { isTauri } from "./api";
import { useSessions, type Session, type SessionStatus } from "./sessions";

/**
 * Telling the operator when a session wants them.
 *
 * Running several Claudes at once only pays off if you can stop watching them,
 * and until now nothing ever acted on the status the rail already tracked — so
 * a session that finished in ninety seconds sat idle until someone happened to
 * look.
 *
 * ## What counts as worth interrupting for
 *
 * The two states are **not** treated the same, and the asymmetry is the point.
 *
 *  - **anything → waiting**: Claude is asking a question. This fires from *any*
 *    previous state, not just from `working`. Status is sampled every 400ms by
 *    reading the screen, so a question that appears and settles between two
 *    polls is seen as `idle → waiting` and would have been missed entirely by a
 *    working-only rule — as would a session reattached while already asking.
 *    A question blocks everything until it is answered; missing one is the most
 *    expensive failure this feature has.
 *  - **working → idle**: the turn finished. This one *does* require having been
 *    working, because "idle" is also the resting state of every session that
 *    has never run — without the guard, opening the app would announce them all.
 *
 * Everything else is silent. `idle → working` is the operator's own keystroke
 * echoing back at them.
 *
 * **The session you are looking at stays quiet.** A notification for the pane
 * already on screen is pure noise, and it is the common case — most turns are
 * watched. The same applies when the window is focused *and* that session is
 * active; a background session still pings, because that is the entire point.
 */

/** What each session was doing last time we looked. */
const previous = new Map<string, SessionStatus>();

function describe(session: Session, status: SessionStatus): { title: string; body: string } | null {
  const where = session.target.host ?? "this machine";

  if (status === "waiting") {
    return {
      title: session.name,
      // Named as a decision rather than "finished": what the operator has to do
      // is different, and the wording is the only thing carrying that.
      body: `Claude is waiting for you · ${where}`,
    };
  }
  if (status === "idle") {
    return { title: session.name, body: `Claude finished · ${where}` };
  }
  return null;
}

let started = false;

/**
 * Watch the store and notify on the transitions above.
 *
 * Idempotent: React's development double-mount would otherwise install two
 * subscriptions and fire every notification twice.
 */
export function startNotifications(): void {
  if (started || !isTauri()) return;
  started = true;

  // Seeded from the current state rather than empty, so restoring a workspace
  // full of sessions does not announce all of them at once.
  for (const session of useSessions.getState().sessions) {
    previous.set(session.id, session.status);
  }

  useSessions.subscribe((state) => {
    const active = state.activeSession;
    // `document.hasFocus()` rather than `visibilityState`: a window that is open
    // but behind something else is still one the operator is not reading.
    const watching = document.hasFocus();

    for (const session of state.sessions) {
      const before = previous.get(session.id);
      previous.set(session.id, session.status);

      if (session.status === before) continue;

      const asking = session.status === "waiting";
      // A question fires from any state; a finish only from `working`. See the
      // note above — `idle` is also where a session that never ran sits.
      if (!asking && !(before === "working" && session.status === "idle")) continue;
      if (watching && session.id === active) continue;

      const message = describe(session, session.status);
      if (!message) continue;

      void invoke("notify", {
        session: session.id,
        title: message.title,
        body: message.body,
      }).catch(() => {
        // A notification that could not be shown is a missed ping, not a
        // reason to break the app.
      });
    }

    // Sessions that have gone: drop them, or the map grows for the life of the
    // app and a reused id would compare against a stale status.
    if (previous.size > state.sessions.length) {
      const live = new Set(state.sessions.map((s) => s.id));
      for (const id of previous.keys()) {
        if (!live.has(id)) previous.delete(id);
      }
    }
  });
}
