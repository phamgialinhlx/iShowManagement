import { useEffect, useSyncExternalStore } from "react";

import { api, hasBackend, type GitBranch } from "./api";
import { useWorkspace } from "./workspace";
import type { ServerId } from "./workspace-model";

/**
 * The branch line under each space (project) in the sidebar.
 *
 * ## Light, and never the thing that opens a connection
 *
 * One `git_branch` per project, on two triggers only: when a project comes into
 * focus, and every 30s for projects whose server is **already connected**. A
 * server the operator disconnected stays disconnected — `git_branch` resolves a
 * target and would happily dial it, and a sidebar that re-opens a connection
 * the operator just closed is the "server that refuses to disconnect" bug in a
 * new place (see "Disconnecting a server" in CLAUDE.md).
 *
 * "Connected" is read from what the app is already doing, not asked for: the
 * local machine always is, and a remote one is when at least one of its
 * sessions holds a live handle. After an explicit Disconnect the handles that
 * existed at that moment stop counting; a *new* one (a pane reattaching, which
 * reconnects on its own) is the evidence it is back.
 *
 * ## Absent means absent
 *
 * `null` is "not a git repository" — an answer, shown as no second line. A
 * failure is remembered as nothing at all, so a host that cannot run git does
 * not get an invented branch, and the next poll tries again.
 */

const POLL_MS = 30_000;

type Entry = { value: GitBranch | null; at: number };

let branches: Record<string, Entry> = {};
const listeners = new Set<() => void>();
const inFlight = new Set<string>();

function publish(projectId: string, value: GitBranch | null) {
  branches = { ...branches, [projectId]: { value, at: Date.now() } };
  for (const l of listeners) l();
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

/** Every known branch line, by project id. Stable between changes. */
export function useBranches(): Record<string, Entry> {
  return useSyncExternalStore(subscribe, () => branches, () => branches);
}

/** For tests and harnesses: set a project's branch line directly. */
export function setBranchForProject(projectId: string, value: GitBranch | null) {
  publish(projectId, value);
}

// ── connection gate ──────────────────────────────────────────────────────────

/** Live handles present when the operator pressed Disconnect, per server. */
const disconnectedWith = new Map<ServerId, Set<string>>();

/** Called by the sidebar's Disconnect, so polling stops for that server. */
export function markDisconnected(serverId: ServerId) {
  const s = useWorkspace.getState();
  const handles = new Set(
    s.sessions
      .filter((x) => s.projectOf(x.id)?.serverId === serverId)
      .flatMap((x) => (s.live[x.id] ? [s.live[x.id]!] : [])),
  );
  disconnectedWith.set(serverId, handles);
}

/** Whether polling this server could not be what opens a connection to it. */
export function isServerConnected(serverId: ServerId): boolean {
  const s = useWorkspace.getState();
  const server = s.servers.find((x) => x.id === serverId);
  if (!server) return false;
  if (!server.target.host) return true; // the local machine
  const handles = s.sessions
    .filter((x) => s.projectOf(x.id)?.serverId === serverId)
    .flatMap((x) => (s.live[x.id] ? [s.live[x.id]!] : []));
  if (handles.length === 0) return false;
  const before = disconnectedWith.get(serverId);
  if (!before) return true;
  if (handles.some((h) => !before.has(h))) {
    // Something reattached since — the connection is back by the app's own doing.
    disconnectedWith.delete(serverId);
    return true;
  }
  return false;
}

async function refresh(projectId: string) {
  if (inFlight.has(projectId)) return;
  const s = useWorkspace.getState();
  const project = s.projects.find((p) => p.id === projectId);
  // The adopted-sessions bucket is not a folder.
  if (!project || project.running) return;
  if (!isServerConnected(project.serverId)) return;
  inFlight.add(projectId);
  try {
    publish(projectId, await api.gitBranch(s.targetOfProject(projectId), project.folder));
  } catch {
    // Not a measurement; leave whatever was last known and try on the next tick.
  } finally {
    inFlight.delete(projectId);
  }
}

/**
 * Run the poll while the sidebar is showing branch lines. `enabled` is in the
 * dependency list so switching the sidebar back to its full width restarts it.
 */
export function useBranchPoll(enabled: boolean) {
  const activeProject = useWorkspace((s) => s.layout.activeProject);

  useEffect(() => {
    if (!enabled || !hasBackend() || !activeProject) return;
    void refresh(activeProject);
  }, [enabled, activeProject]);

  useEffect(() => {
    if (!enabled || !hasBackend()) return;
    const tick = () => {
      if (typeof document !== "undefined" && document.visibilityState !== "visible") return;
      for (const p of useWorkspace.getState().projects) void refresh(p.id);
    };
    tick();
    const timer = window.setInterval(tick, POLL_MS);
    return () => window.clearInterval(timer);
  }, [enabled]);
}
