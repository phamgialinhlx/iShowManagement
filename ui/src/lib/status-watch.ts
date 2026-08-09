import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { isTauri } from "./api";
import { bench } from "./debug-log";
import { useWorkspace } from "./workspace";
import type { SessionStatus } from "./workspace-model";

/**
 * Keep every session's status current, not only the ones on screen.
 *
 * ## What this exists for
 *
 * The rail answers "which of my machines needs me right now?" for the sessions
 * you are **not** looking at — for the ones on screen you can simply look. So
 * something has to report status for every session, whether or not it has a
 * rendered panel.
 *
 * ## Push, not poll
 *
 * This used to poll `claude_state` for every session every 1.5s, and `ClaudePanel`
 * polled every *rendered* pane every 400ms on top of that — one IPC round trip
 * plus a re-render each, none of it stopping when the window was hidden. Measured
 * with `powermetrics`, that kept the machine waking ~75–82 times a second even
 * backgrounded and scaled with pane count; a high wakeup rate is what keeps a
 * laptop's CPU out of the idle states that let it run cool.
 *
 * Now the host pushes. `rmux-agent watch-status` (via `claude_status.rs`) watches
 * Claude's own `~/.claude/sessions/<pid>.json` files and emits a `claude-status`
 * event on every change — so this listens instead of asking, and does nothing
 * between changes.
 *
 * ## The poll that remains
 *
 * A slow, visibility-gated poll stays as a *fallback*, running only for hosts the
 * push stream does not cover: an agent too old to know the subcommand
 * (`unsupported`), or one whose stream has not come up yet (startup, a dropped
 * connection reconnecting). Once a host's stream is live, its sessions rely on
 * events and the poll skips them.
 *
 * ## Mapping an event to a session
 *
 * The event carries Claude's own conversation id, which is globally unique, so
 * matching a session by its stored `resume` is exact. Before that is known, a
 * session is matched by folder + host — but only when exactly one candidate fits,
 * because guessing between two sessions in one folder would light the wrong dot.
 * The first unambiguous match is *learned* (stored as the session's `resume`), so
 * it becomes exact thereafter even once a second session opens in that folder.
 */

/** Fallback cadence — only hosts the push stream does not cover are polled, and
 *  only while the window is visible. Slower than the old 1.5s: a dot does not
 *  need frequent updates, and this is now the exception, not the rule. */
const POLL_MS = 5000;

type StatusUpdate = {
  targetId: string;
  sessionId: string;
  cwd?: string;
  pid?: number;
  status: string;
  /** Host-clock ms of the change (Claude's `statusUpdatedAt`). Drives the unseen
   *  watermark, so a run that finished while the app was closed still shows. */
  updatedAt?: number;
};
type StatusEvent =
  | { targetId: string; ready: true }
  | { targetId: string; unsupported: true }
  | StatusUpdate;

/** The label the agent reports for a host — matches Rust's `TargetId::label()`. */
function targetLabel(target: { host?: string; user?: string } | undefined): string {
  if (!target?.host) return "local";
  return target.user ? `${target.user}@${target.host}` : target.host;
}

/** Claude's status word → the rail's three states, or `null` to leave alone. */
function mapStatus(status: string): SessionStatus | null {
  switch (status) {
    case "busy":
    case "shell":
      return "working";
    case "waiting":
      return "waiting";
    case "idle":
    // The conversation ended or its process is gone — nothing needs the
    // operator. The pane's own reconnect handles a dropped attachment.
    case "gone":
      return "idle";
    default:
      // An unknown word is not a measurement — inventing "idle" for it would be
      // the same lie as reporting idle for a session never observed.
      return null;
  }
}

let active = false;

export function startStatusWatch(): () => void {
  // Guarded so importing/rendering in a non-Tauri context (a test) is inert, and
  // against a double-start (React StrictMode remounts the effect).
  if (!isTauri() || active) return () => {};
  active = true;

  // Hosts whose push stream is live, and hosts whose agent cannot push. A
  // session is left to the fallback poll unless its host is `ready` and not
  // `unsupported`.
  const ready = new Set<string>();
  const unsupported = new Set<string>();

  const applyUpdate = (ev: StatusUpdate) => {
    const state = useWorkspace.getState();
    const claude = state.sessions.filter((s) => s.kind === "claude");

    // Exact: the conversation id has already been learned for a session.
    let match = claude.find((s) => s.resume && s.resume === ev.sessionId);
    // Otherwise, one unambiguous folder+host candidate that has no id yet.
    if (!match && ev.cwd) {
      const candidates = claude.filter(
        (s) =>
          !s.resume &&
          state.projectOf(s.id)?.folder === ev.cwd &&
          targetLabel(state.serverOf(s.id)?.target) === ev.targetId,
      );
      if (candidates.length === 1) match = candidates[0];
    }
    if (!match) return;

    // Learn the conversation id so future events for it are exact — but not from
    // a "gone" event, which no longer names a live conversation to resume.
    if (ev.status !== "gone" && match.resume !== ev.sessionId) {
      state.setResume(match.id, ev.sessionId);
    }
    const status = mapStatus(ev.status);
    // Pass the host-clock stamp through so the unseen watermark is skew-free and
    // survives a restart — the whole reason for the watermark over an edge flag.
    if (status) {
      state.setStatus(match.id, status, ev.updatedAt);
      bench(`status session=${match.id} status=${status} source=push`);
    }
  };

  const onEvent = (ev: StatusEvent) => {
    if ("ready" in ev) {
      ready.add(ev.targetId);
      unsupported.delete(ev.targetId);
      return;
    }
    if ("unsupported" in ev) {
      unsupported.add(ev.targetId);
      ready.delete(ev.targetId);
      return;
    }
    applyUpdate(ev);
  };

  let unlisten: UnlistenFn | undefined;
  let stopped = false;
  void listen<StatusEvent>("claude-status", (e) => onEvent(e.payload)).then((fn) => {
    // If teardown beat the listener registering, unlisten immediately.
    if (stopped) fn();
    else unlisten = fn;
  });

  const tick = async () => {
    // Nobody is reading the rail when the window is hidden, so the fallback does
    // nothing — this is where the backgrounded wakeups went.
    if (typeof document !== "undefined" && document.visibilityState !== "visible") return;

    const { sessions, live, serverOf, setStatus } = useWorkspace.getState();
    await Promise.all(
      sessions.map(async (session) => {
        if (session.kind !== "claude") return;
        const claudeId = live[session.id];
        if (!claudeId) return;

        // Covered by the push stream — leave it to the events.
        const label = targetLabel(serverOf(session.id)?.target);
        if (ready.has(label) && !unsupported.has(label)) return;

        try {
          const next = await invoke<{ prompt: unknown; working: boolean; exited?: number }>(
            "claude_state",
            { id: claudeId },
          );
          if (next.exited != null) return;
          const s = next.prompt ? "waiting" : next.working ? "working" : "idle";
          setStatus(session.id, s);
          bench(`status session=${session.id} status=${s} source=fallback`);
        } catch {
          // The handle is gone; the panel owns recovery.
        }
      }),
    );
  };

  void tick();
  const timer = window.setInterval(() => void tick(), POLL_MS);

  return () => {
    stopped = true;
    active = false;
    window.clearInterval(timer);
    unlisten?.();
  };
}
