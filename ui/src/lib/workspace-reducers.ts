import type { TargetRef } from "./api";
import {
  makeServer,
  makeProject,
  serverId,
  type PaneRef,
  type PersistedV3,
  type ProjectId,
  type ServerId,
  type SessionKind,
  type SessionV3,
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
  "servers" | "projects" | "sessions" | "panes" | "activeSession"
>;

export const EMPTY_CORE: Core = {
  servers: [],
  projects: [],
  sessions: [],
  panes: [],
  activeSession: null,
};

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
} & Partial<
  Pick<
    SessionV3,
    | "resume"
    | "cwd"
    | "fullscreen"
    | "skipPermissions"
    | "modelProfile"
    | "claudeAccount"
    | "jiraProject"
    | "contextWindow"
    | "renamed"
    // Carried explicitly rather than surviving by accident through the spread
    // below: an adopted session that loses `hostName` starts a second one.
    | "hostName"
  >
>;

/** Append a Session and make it active. The id is minted by the caller (it needs
 *  a clock/counter, which is not pure) and must already carry the right prefix. */
export function addSession(ws: Core, session: NewSession): Core {
  const full: SessionV3 = { ...session };
  return { ...ws, sessions: [...ws.sessions, full], activeSession: session.id };
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
  const activeSession =
    ws.activeSession === id ? (sessions[sessions.length - 1]?.id ?? null) : ws.activeSession;
  return { ...ws, sessions, panes, activeSession };
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
  return { ...ws, panes };
}

/** Clear a tile without touching the session behind it (close pane ≠ kill). */
export function closePane(ws: Core, index: number): Core {
  if (index < 0 || index >= ws.panes.length) return ws;
  const panes = [...ws.panes];
  panes[index] = null;
  return { ...ws, panes };
}
