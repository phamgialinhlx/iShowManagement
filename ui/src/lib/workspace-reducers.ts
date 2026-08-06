import type { TargetRef } from "./api";
import {
  makeServer,
  makeProject,
  paneRefKey,
  serverId,
  type PaneRef,
  type PersistedV3,
  type ProjectId,
  type ServerId,
  type SessionKind,
  type SessionV3,
  type TabRef,
} from "./workspace-model.ts";

/**
 * Pure state transitions for the v3 workspace store.
 *
 * The Zustand store in `workspace.ts` is a thin IO wrapper (localStorage, PTY
 * handles, file reads) over these; the branching logic — dedup on create, pane
 * placement, cleanup on remove — lives here so it can be unit-tested in Node
 * without booting the store. Every function is immutable: it returns new state
 * and never mutates its input.
 *
 * `Core` is the persisted skeleton (`PersistedV3` minus the file maps, which the
 * store carries separately as runtime buffers). Reducers touch only structure.
 */
export type Core = Pick<
  PersistedV3,
  "servers" | "projects" | "sessions" | "panes" | "tabs" | "activeSession"
>;

export const EMPTY_CORE: Core = {
  servers: [],
  projects: [],
  sessions: [],
  panes: [],
  tabs: [],
  activeSession: null,
};

/** Append a tab unless its identity is already open. Creation (`addSession`)
 *  and placement (`assignPane`) both funnel through this, so "anything on
 *  screen has a tab" holds by construction, not by call-site discipline. */
export function ensureTab(ws: Core, ref: TabRef): Core {
  const key = paneRefKey(ref);
  if (ws.tabs.some((t) => paneRefKey(t) === key)) return ws;
  return { ...ws, tabs: [...ws.tabs, ref] };
}

/** Add a Server for a target, or return the existing one — ids are derived, so
 *  connecting the same host twice never double-creates. */
export function addServer(ws: Core, target: TargetRef): { ws: Core; id: ServerId } {
  const id = serverId(target);
  if (ws.servers.some((s) => s.id === id)) return { ws, id };
  return { ws: { ...ws, servers: [...ws.servers, makeServer(target)] }, id };
}

/** Add a Project (folder) on a Server, or return the existing one. */
export function addProject(
  ws: Core,
  server: ServerId,
  folder: string,
  label?: string,
): { ws: Core; id: ProjectId } {
  const project = makeProject(server, folder, label);
  if (ws.projects.some((p) => p.id === project.id)) return { ws, id: project.id };
  return { ws: { ...ws, projects: [...ws.projects, project] }, id: project.id };
}

export type NewSession = {
  id: string;
  projectId: ProjectId;
  kind: SessionKind;
  name: string;
} & Partial<Pick<SessionV3, "resume" | "fullscreen" | "skipPermissions" | "modelProfile" | "claudeAccount" | "jiraProject" | "contextWindow">>;

/** Append a Session and make it active. The id is minted by the caller (it needs
 *  a clock/counter, which is not pure) and must already carry the right prefix. */
export function addSession(ws: Core, session: NewSession): Core {
  const full: SessionV3 = { ...session };
  // Creating is opening: the new session gets a tab immediately.
  return ensureTab(
    { ...ws, sessions: [...ws.sessions, full], activeSession: session.id },
    { kind: "session", id: session.id },
  );
}

/**
 * Remove a Session from the structure.
 *
 * The Project and Server it lived under **stay** — an empty Project is a valid,
 * persisted node (created deliberately, or just emptied), and auto-pruning it
 * would delete a folder the operator set up. Panes pointing at the session are
 * cleared to `null` (the tile becomes empty, not stuck on a dead session), and a
 * removed active session hands off to the last remaining one.
 *
 * This does **not** send the agent kill or drop the live handle — those are IO,
 * done by the store around this reducer. Compare v2's `removeSession`.
 */
export function removeSession(ws: Core, id: string): Core {
  if (!ws.sessions.some((s) => s.id === id)) return ws;
  const sessions = ws.sessions.filter((s) => s.id !== id);
  const panes = ws.panes.map((p) => (p && p.kind === "session" && p.id === id ? null : p));
  const tabs = ws.tabs.filter((t) => !(t.kind === "session" && t.id === id));
  const activeSession =
    ws.activeSession === id ? (sessions[sessions.length - 1]?.id ?? null) : ws.activeSession;
  return { ...ws, sessions, panes, tabs, activeSession };
}

/**
 * Put a pane in a grid tile.
 *
 * A **session** may be in only one tile: mounting its view twice means two xterm
 * hosts attaching to one PTY (the v2 `assignSlot` invariant). So assigning a
 * session pane first removes that session from any other tile. Host/files panes
 * are read views and may repeat freely. `null` hands the tile back to empty.
 */
export function assignPane(ws: Core, index: number, ref: PaneRef | null): Core {
  const panes = [...ws.panes];
  while (panes.length <= index) panes.push(null);
  if (ref && ref.kind === "session") {
    for (let i = 0; i < panes.length; i += 1) {
      const p = panes[i];
      if (p && p.kind === "session" && p.id === ref.id) panes[i] = null;
    }
  }
  panes[index] = ref;
  const next = { ...ws, panes };
  // Putting something on screen opens it: the tab appears with the pane.
  return ref && ref.kind !== "empty" ? ensureTab(next, ref) : next;
}

/** Clear a tile without touching the session behind it (close pane ≠ kill). */
export function closePane(ws: Core, index: number): Core {
  if (index < 0 || index >= ws.panes.length) return ws;
  const panes = [...ws.panes];
  panes[index] = null;
  return { ...ws, panes };
}

/** Remove a ref from the tab bar and from every cell holding it. The entity
 *  behind it is untouched — this is `closeTab`'s body, reused by the remove
 *  cascades so a deleted project/server cannot leave a dangling files/host tab
 *  or pane behind. */
export function stripRef(ws: Core, ref: TabRef): Core {
  const key = paneRefKey(ref);
  const tabs = ws.tabs.filter((t) => paneRefKey(t) !== key);
  const panes = ws.panes.map((p) => (p && paneRefKey(p) === key ? null : p));
  return { ...ws, tabs, panes };
}

/**
 * Close a tab: gone from the bar and from every cell. A freed cell returns to
 * `null`, not `"empty"` — the closed tab is no longer a fill candidate, so
 * auto-fill hands the cell to the next open tab rather than putting this one
 * straight back. The session behind a session tab lives on in the rail;
 * killing is `removeSession` and nothing else.
 *
 * Closing the active session's tab hands `activeSession` to the last remaining
 * session tab — the rail highlight, title bar and instruments must not keep
 * pointing at something that is no longer open.
 */
export function closeTab(ws: Core, ref: TabRef): Core {
  const next = stripRef(ws, ref);
  if (ref.kind === "session" && ws.activeSession === ref.id) {
    const last = [...next.tabs]
      .reverse()
      .find((t): t is Extract<TabRef, { kind: "session" }> => t.kind === "session");
    return { ...next, activeSession: last?.id ?? null };
  }
  return next;
}
