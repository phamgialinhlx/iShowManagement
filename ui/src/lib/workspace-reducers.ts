import type { TargetRef } from "./api";
import {
  makeServer,
  makeProject,
  serverId,
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
export type Core = Pick<PersistedV3, "servers" | "projects" | "sessions" | "activeSession">;

export const EMPTY_CORE: Core = {
  servers: [],
  projects: [],
  sessions: [],
  activeSession: null,
};

/** Add a Server for a target, or return the existing one — ids are derived, so
 *  connecting the same host twice never double-creates. */
export function addServer(ws: Core, target: TargetRef): { ws: Core; id: ServerId } {
  const id = serverId(target);
  if (ws.servers.some((s) => s.id === id)) return { ws, id };
  return { ws: { ...ws, servers: [...ws.servers, makeServer(target)] }, id };
}

/**
 * Choose the key a Server authenticates with — in place.
 *
 * Only `target.identity` changes. The id is derived from host/user/port and
 * deliberately not from the key, so every Project and Session under the Server
 * keeps pointing at it; a key change that minted a new id would orphan them
 * all. `undefined` (or blank) returns the Server to DEFAULT.
 */
export function setServerIdentity(ws: Core, id: ServerId, identity: string | undefined): Core {
  const path = identity?.trim() || undefined;
  return {
    ...ws,
    servers: ws.servers.map((s) => {
      if (s.id !== id || !s.target.host) return s;
      const { identity: _old, ...rest } = s.target;
      return { ...s, target: path ? { ...rest, identity: path } : rest };
    }),
  };
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
    | "lowLatency"
    | "fullscreen"
    | "skipPermissions"
    | "modelProfile"
    | "activeProfile"
    | "claudeAccount"
    | "jiraProject"
    | "contextWindow"
    | "renamed"
    // Carried explicitly rather than surviving by accident through the spread
    // below: an adopted session that loses `hostName` starts a second one.
    | "hostName"
    | "parentId"
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
 * would delete a folder the operator set up. The pane showing it is the layout's
 * business (`layout-tree.ts` `removeSessionLeaves`, and `normalizeLayout` as the
 * backstop), and a removed active session hands off to the last remaining one.
 *
 * This does **not** send the agent kill or drop the live handle — those are IO,
 * done by the store around this reducer. Compare v2's `removeSession`.
 */
export function removeSession(ws: Core, id: string): Core {
  if (!ws.sessions.some((s) => s.id === id)) return ws;
  const sessions = ws.sessions.filter((s) => s.id !== id);
  const activeSession =
    ws.activeSession === id ? (sessions[sessions.length - 1]?.id ?? null) : ws.activeSession;
  return { ...ws, sessions, activeSession };
}
