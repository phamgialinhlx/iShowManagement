import { invoke } from "@tauri-apps/api/core";

import { isTauri } from "./api";
import { raiseAlert } from "./alerts";
import { useWorkspace } from "./workspace";
import type { SessionStatus } from "./workspace-model";

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
 * ## Whether the watched session stays quiet is the operator's call
 *
 * It used to be suppressed unconditionally: a notification for the pane already
 * on screen looked like pure noise. In practice that made the feature appear
 * broken — you watch a session precisely *because* you are waiting on it, and
 * silence is indistinguishable from a bug. So it now notifies for everything by
 * default, and Settings › Notifications has a switch for anyone who does find it
 * redundant.
 */

const QUIET_KEY = "rmux.notify.quietWatched";

/** Whether the session on screen is deliberately silent. Off by default. */
export function quietWhenWatching(): boolean {
  return localStorage.getItem(QUIET_KEY) === "1";
}

export function setQuietWhenWatching(quiet: boolean): void {
  localStorage.setItem(QUIET_KEY, quiet ? "1" : "0");
}


const ALERT_KEY = "rmux.notify.alert";
const SOUND_KEY = "rmux.notify.sound";

/**
 * Whether an unwatched session gets an **alert** rather than only a
 * notification.
 *
 * A macOS notification is a banner that slides away on its own. That is right
 * for "this finished"; it is not enough for the case rmux exists to serve —
 * several sessions running at once, and one of them stopping to ask a question
 * that blocks everything behind it. A banner missed while looking at another
 * pane leaves that work stalled with nothing on screen saying so.
 *
 * **On by default**, and only ever for a session you are *not* looking at. For
 * the one on screen there is nothing to alert about: you can see it.
 */
export function alertWhenUnwatched(): boolean {
  return localStorage.getItem(ALERT_KEY) !== "0";
}

export function setAlertWhenUnwatched(on: boolean): void {
  try {
    localStorage.setItem(ALERT_KEY, on ? "1" : "0");
  } catch {
    // Losing the preference falls back to on, which is the safe direction:
    // an alert too many is noticed, an alert too few is missed work.
  }
}

/** Whether the alert is accompanied by a sound. On by default. */
export function alertSound(): boolean {
  return localStorage.getItem(SOUND_KEY) !== "0";
}

export function setAlertSound(on: boolean): void {
  try {
    localStorage.setItem(SOUND_KEY, on ? "1" : "0");
  } catch {
    /* see above */
  }
}

/**
 * Play the alert tone.
 *
 * **One `Audio` per call, not one reused.** Two sessions can stop within the
 * same tick, and restarting a shared element cuts the first tone off — so the
 * second alert makes the first one *quieter*, which is backwards.
 *
 * Every failure is swallowed. A browser may refuse playback until the page has
 * been interacted with, the file may be missing from a bundle, the machine may
 * have no output device. None of those are worth an error in front of someone
 * who is being told something else needs them.
 */
export function playAlertSound(): void {
  if (!alertSound()) return;
  try {
    const audio = new Audio("/sounds/edex-granted.wav");
    audio.volume = 0.5;
    void audio.play().catch(() => {});
  } catch {
    /* not worth surfacing */
  }
}

/** What each session was doing last time we looked. */
const previous = new Map<string, SessionStatus>();

function describe(name: string, where: string, status: SessionStatus): { title: string; body: string } | null {
  if (status === "waiting") {
    return {
      title: name,
      // Named as a decision rather than "finished": what the operator has to do
      // is different, and the wording is the only thing carrying that.
      body: `Claude is waiting for you · ${where}`,
    };
  }
  if (status === "idle") {
    return { title: name, body: `Claude finished · ${where}` };
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
  {
    const s = useWorkspace.getState();
    for (const session of s.sessions) {
      previous.set(session.id, s.runtime[session.id]?.status ?? "idle");
    }
  }

  useWorkspace.subscribe((state) => {
    const active = state.activeSession;
    // `document.hasFocus()` rather than `visibilityState`: a window that is open
    // but behind something else is still one the operator is not reading.
    const watching = document.hasFocus();

    for (const session of state.sessions) {
      const status = state.runtime[session.id]?.status ?? "idle";
      const before = previous.get(session.id);
      previous.set(session.id, status);

      if (status === before) continue;

      const asking = status === "waiting";
      // A question fires from any state; a finish only from `working`. See the
      // note above — `idle` is also where a session that never ran sits.
      if (!asking && !(before === "working" && status === "idle")) continue;
      // Read per event, not captured once: the operator can change it in
      // Settings while sessions are running, and a subscription installed at
      // startup would hold the old value for the life of the app.
      if (quietWhenWatching() && watching && session.id === active) continue;

      const where = state.serverOf(session.id)?.target.host ?? "this machine";
      const message = describe(session.name, where, status);
      if (!message) continue;

      // "Unwatched" is both halves: the window is not frontmost, or it is but
      // this session is not the one on screen. Either way the operator is not
      // looking at the thing that needs them.
      const unwatched = !watching || session.id !== active;
      if (unwatched && alertWhenUnwatched()) {
        raiseAlert({
          sessionId: session.id,
          title: message.title,
          body: message.body,
          asking,
        });
        playAlertSound();
      }

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
