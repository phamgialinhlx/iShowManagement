import { useSyncExternalStore } from "react";

import { api, hasBackend } from "./api";
import { endedChildren, parseSeen, planChildren, pollInterval, type HostChild } from "./children.ts";
import { isServerConnected } from "./git-branch";
import { isPrimaryWindow } from "./window-scope";
import { tabAreaOf, useWorkspace } from "./workspace";
import type { ServerId } from "./workspace-model.ts";

/**
 * The runtime half of child adoption (`children.ts` is the pure half).
 *
 * ## When a server is asked
 *
 * - **Only a server with a live session handle, and only one that is
 *   connected** (`isServerConnected`, the gate the branch line uses). Reading
 *   the file resolves a target and would happily dial a host; a sidebar feature
 *   that reopens a connection the operator just closed is the "server that
 *   refuses to disconnect" bug. A server with no live handle has no session a
 *   child could be beside anyway.
 * - **Every 3s while anything on that server is working or waiting, else every
 *   10s** — an agent spawns while it works, and an idle host has nothing to say.
 * - **Never while the window is hidden.** Nobody is there to see a pane appear,
 *   and the next visible tick catches up — the file is a list, not a stream.
 * - **One request per server at a time**, so a slow host cannot stack polls.
 *
 * It runs in the **primary window only**. The pool is shared between windows, so
 * two watchers would each adopt the same child; one adopts and the others
 * receive the session through the cross-window `storage` sync and give it a tab
 * of their own (`normalizeLayout`).
 *
 * A failed read is not a measurement: the seen list is neither pruned nor
 * extended, and the next tick tries again.
 */

const SEEN_KEY = "rmux.children.seen";
const TICK_MS = 1_000;

const storage: Pick<Storage, "getItem" | "setItem"> | null =
  typeof localStorage === "undefined" ? null : localStorage;

function loadSeen(): Record<ServerId, string[]> {
  try {
    const raw = storage?.getItem(SEEN_KEY);
    return parseSeen(raw ? JSON.parse(raw) : null);
  } catch {
    return {};
  }
}

function saveSeen(seen: Record<ServerId, string[]>) {
  try {
    storage?.setItem(SEEN_KEY, JSON.stringify(seen));
  } catch {
    // A full localStorage costs re-deciding, never the app.
  }
}

// ── ended children (runtime; read by the pane) ───────────────────────────────

let ended: ReadonlySet<string> = new Set();
const listeners = new Set<() => void>();
const endedByServer = new Map<ServerId, string[]>();

function publishEnded() {
  const next = new Set([...endedByServer.values()].flat());
  if (next.size === ended.size && [...next].every((id) => ended.has(id))) return;
  ended = next;
  for (const l of listeners) l();
}

/** Session ids of adopted children the host says have ended. */
export function useEndedChildren(): ReadonlySet<string> {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => ended,
    () => ended,
  );
}

/**
 * The width / height of the area the parent's tab is drawn in, so a split goes
 * the way the pane is long. With a project grid that is the parent project's
 * **tile**, not the whole pane area — a tall tile in a wide window would
 * otherwise split a tall pane side by side. A project with no tile on screen
 * falls back to the pane area (the shape it will have when focused).
 */
function paneAspect(projectId?: string): number {
  if (typeof document === "undefined") return 16 / 9;
  const tile = projectId
    ? Array.from(document.querySelectorAll<HTMLElement>(".project-tile")).find(
        (el) => el.dataset.tileProject === projectId,
      )
    : undefined;
  const el = tile ?? document.querySelector(".pane-area");
  const r = el?.getBoundingClientRect();
  if (r && r.width > 0 && r.height > 0) return r.width / r.height;
  return typeof window !== "undefined" && window.innerHeight > 0 ? window.innerWidth / window.innerHeight : 16 / 9;
}

/**
 * Apply one server's list: adopt what is new, prune and extend the seen list,
 * refresh the ended set. Exported so a harness (and the browser check) can feed
 * a list without a host.
 */
export function ingestChildren(serverId: ServerId, children: readonly HostChild[]): string[] {
  const state = useWorkspace.getState();
  const all = loadSeen();
  const plan = planChildren(serverId, children, state, all[serverId] ?? []);
  const adopted: string[] = [];
  for (const d of plan.adopt) {
    const parent = useWorkspace.getState().sessions.find((s) => s.id === d.parentId);
    const aspect = paneAspect(parent?.projectId);
    // Measured, so "too crowded for another split" is a pane size and not a
    // count (`layout-tree.ts` `childPlacement`).
    const id = useWorkspace.getState().adoptChild(serverId, d, aspect, tabAreaOf(parent?.projectId));
    if (id) adopted.push(id);
  }
  const next = { ...all };
  if (plan.seen.length) next[serverId] = plan.seen;
  else delete next[serverId];
  saveSeen(next);
  endedByServer.set(serverId, endedChildren(serverId, children, useWorkspace.getState()));
  publishEnded();
  return adopted;
}

function serverHasLiveHandle(serverId: ServerId): boolean {
  const s = useWorkspace.getState();
  return s.sessions.some((x) => s.live[x.id] && s.projectOf(x.id)?.serverId === serverId);
}

function serverIsBusy(serverId: ServerId): boolean {
  const s = useWorkspace.getState();
  return s.sessions.some((x) => {
    if (s.projectOf(x.id)?.serverId !== serverId) return false;
    const st = s.runtime[x.id]?.status;
    return st === "working" || st === "waiting";
  });
}

let running = false;

/** Start the watcher; returns its stop. Inert without a backend, outside the
 *  primary window, and on a second start (StrictMode remounts the effect). */
export function startChildrenWatch(): () => void {
  if (!hasBackend() || !isPrimaryWindow() || running) return () => {};
  running = true;
  // Children placed by the old folder rule sit in a second project; put them
  // back beside their parents once, before the first poll.
  const focused = useWorkspace.getState().layout.activeProject ?? undefined;
  useWorkspace.getState().rehomeChildren(paneAspect(focused), tabAreaOf(focused));
  const last = new Map<ServerId, number>();
  const inFlight = new Set<ServerId>();

  const tick = () => {
    if (typeof document !== "undefined" && document.visibilityState !== "visible") return;
    const now = Date.now();
    for (const server of useWorkspace.getState().servers) {
      const id = server.id;
      if (inFlight.has(id)) continue;
      if (!serverHasLiveHandle(id) || !isServerConnected(id)) continue;
      if (now - (last.get(id) ?? 0) < pollInterval(serverIsBusy(id))) continue;
      last.set(id, now);
      inFlight.add(id);
      api
        .hostChildren(server.target)
        .then((children) => {
          // The server may have been removed or disconnected while we waited.
          if (!useWorkspace.getState().servers.some((s) => s.id === id)) return;
          ingestChildren(id, children);
        })
        .catch(() => {
          // Not an answer; leave every decision as it was.
        })
        .finally(() => inFlight.delete(id));
    }
  };

  const timer = window.setInterval(tick, TICK_MS);
  return () => {
    window.clearInterval(timer);
    running = false;
  };
}
