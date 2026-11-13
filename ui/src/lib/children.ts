import type { Project, ProjectId, ServerId, SessionKind, SessionV3 } from "./workspace-model.ts";
import { makeProject, reattachName } from "./workspace-model.ts";

/**
 * Which spawned children this workspace should adopt — herdr's orchestration,
 * the desktop half.
 *
 * An agent in an rmux session can start sub-sessions on its host with
 * `rmux spawn`; the host records each in `~/.rmux/children.json`, and
 * `host_children` reads it. This module decides, from that list and the pool,
 * **what to adopt and where it belongs**. Placement in the tree is
 * `layout-tree.ts`'s `placeBeside`; applying it is the store's `adoptChild`;
 * polling is `children-watch.ts`.
 *
 * **Pure.** `workspace-model.ts` is the only runtime import, and it is itself
 * pure — so `ui/children-check.ts` runs this in Node without booting the store.
 *
 * ## Decided once, remembered
 *
 * A child is adopted **once**. The per-server `seen` list records every child
 * this workspace has made a decision about, so a child the operator detached or
 * closed is not dragged back on the next poll — "nothing moves under the
 * operator's hands" would mean nothing if a pane they closed reappeared three
 * seconds later. `seen` is pruned against the host's list on every successful
 * poll: a name the host no longer records can never be reported again, and an
 * unpruned list shares the `localStorage` quota with the session list.
 *
 * ## Only children of something we show
 *
 * A child is adopted only when its parent is a session in *this* workspace on
 * *that* server. A shared host carries work started from other machines, and a
 * window that pulled in every child on the box would be adopting other people's
 * orchestration. An unmatched child is **not** marked seen, so if its parent is
 * attached here later the child follows it in. It stays reachable either way
 * through HOST ▸ SESSIONS.
 */

/** One row of `host_children` (structurally `api.HostChild`; repeated here so
 *  this module needs no runtime import from `api.ts`). */
export type HostChild = {
  session: string;
  parent: string | null;
  kind: string;
  cwd: string;
  name: string;
  createdAt: number;
  /** The credential set the host bound it to (inherited from its parent). */
  profile?: string;
  /** The fallback chain it inherited. */
  chain?: string;
};

export type ChildPool = {
  projects: Pick<Project, "id" | "serverId" | "folder" | "running">[];
  sessions: Pick<SessionV3, "id" | "projectId" | "kind" | "hostName">[];
};

export type ChildDecision = {
  child: HostChild;
  kind: SessionKind;
  /** App session id of the parent. */
  parentId: string;
  /** The project it joins. `create` set means it does not exist yet. */
  projectId: ProjectId;
  create?: { folder: string };
  name: string;
};

export type ChildPlan = {
  adopt: ChildDecision[];
  /** The new `seen` list for this server: pruned, plus every decision made. */
  seen: string[];
};

const KINDS: Record<string, SessionKind> = {
  claude: "claude",
  pi: "pi",
  codex: "codex",
  terminal: "terminal",
};

/** Folder equality that a trailing slash cannot defeat (`/w/` is `/w`). */
export function sameFolder(a: string, b: string): boolean {
  const norm = (p: string) => (p.length > 1 ? p.replace(/\/+$/, "") : p);
  return norm(a) === norm(b);
}

/** The app session on `serverId` the host knows by `daemonName`, if any. */
export function sessionByDaemonName(
  pool: ChildPool,
  serverId: ServerId,
  daemonName: string,
): ChildPool["sessions"][number] | undefined {
  const onServer = new Set(pool.projects.filter((p) => p.serverId === serverId).map((p) => p.id));
  return pool.sessions.find((s) => onServer.has(s.projectId) && reattachName(s) === daemonName);
}

/**
 * The project a child joins: **its parent's, always** — so it opens as a split
 * beside the session that spawned it, which is the whole point of adopting it.
 *
 * It used to follow the child's folder, and that put children somewhere the
 * operator was not looking: a project opened at `~/arrow` (a symlink to
 * `/data/arrow`) got its child — spawned by an agent with the *resolved* path —
 * in a brand-new `/data/arrow` project, so nothing appeared beside the parent.
 * A folder string cannot tell a symlink, a subdirectory and a different checkout
 * apart, and an agent picks whichever spelling it likes; the parent is the one
 * fact that is not ambiguous.
 *
 * The exception is a parent in the adopted-sessions bucket (`running`), which
 * holds work of unknown directory and must not collect children of a known one:
 * then the existing project on that server for the folder, else a new one.
 */
export function chooseProject(
  pool: ChildPool,
  serverId: ServerId,
  parentProjectId: ProjectId,
  cwd: string,
): { projectId: ProjectId; create?: { folder: string } } {
  const parent = pool.projects.find((p) => p.id === parentProjectId);
  const folder = cwd.trim();
  if (!folder || !parent || !parent.running) return { projectId: parentProjectId };
  const existing = pool.projects.find(
    (p) => p.serverId === serverId && !p.running && sameFolder(p.folder, folder),
  );
  if (existing) return { projectId: existing.id };
  const made = makeProject(serverId, folder);
  return { projectId: made.id, create: { folder } };
}

/**
 * Decide this poll's adoptions for one server.
 *
 * `children` is the host's list (already parsed, already tolerant); `seen` is
 * what this workspace decided on before. Malformed rows — no session name, an
 * unknown kind, a duplicate — are skipped here too, because the list may come
 * from an older Rust build or a hand-edited file.
 */
export function planChildren(
  serverId: ServerId,
  children: readonly HostChild[],
  pool: ChildPool,
  seen: readonly string[],
): ChildPlan {
  const valid: HostChild[] = [];
  const names = new Set<string>();
  for (const c of Array.isArray(children) ? children : []) {
    if (!c || typeof c.session !== "string" || !c.session.trim()) continue;
    if (typeof c.kind !== "string" || !KINDS[c.kind]) continue;
    if (names.has(c.session)) continue;
    names.add(c.session);
    valid.push(c);
  }

  // Pruned: a name the host no longer lists cannot come back.
  const nextSeen = new Set(seen.filter((n) => names.has(n)));
  const adopt: ChildDecision[] = [];
  // Projects this plan will create, so two children in one new folder share it.
  const creating = new Set<ProjectId>();

  for (const c of valid) {
    if (nextSeen.has(c.session)) continue;
    // Already in the workspace (adopted by hand from HOST ▸ SESSIONS, or by an
    // earlier poll whose seen-write was lost) — record the decision, adopt
    // nothing. Two rows on one daemon session would mount it twice.
    if (sessionByDaemonName(pool, serverId, c.session)) {
      nextSeen.add(c.session);
      continue;
    }
    const parent = c.parent ? sessionByDaemonName(pool, serverId, c.parent) : undefined;
    if (!parent) continue; // not ours — see the module docs; *not* marked seen
    const choice = chooseProject(pool, serverId, parent.projectId, typeof c.cwd === "string" ? c.cwd : "");
    let create = choice.create;
    if (create && creating.has(choice.projectId)) create = undefined;
    if (create) creating.add(choice.projectId);
    adopt.push({
      child: c,
      kind: KINDS[c.kind]!,
      parentId: parent.id,
      projectId: choice.projectId,
      create,
      name: (typeof c.name === "string" && c.name.trim()) || c.session,
    });
    nextSeen.add(c.session);
  }

  return { adopt, seen: [...nextSeen] };
}

/**
 * Adopted children that the host no longer lists — they ended there. Their
 * panes are **not** removed (nothing moves under the operator's hands); the
 * pane says the work ended and offers to close it.
 */
export function endedChildren(
  serverId: ServerId,
  children: readonly HostChild[],
  pool: {
    projects: ChildPool["projects"];
    sessions: Pick<SessionV3, "id" | "projectId" | "kind" | "hostName" | "parentId">[];
  },
): string[] {
  const live = new Set(children.map((c) => c.session));
  const onServer = new Set(pool.projects.filter((p) => p.serverId === serverId).map((p) => p.id));
  return pool.sessions
    .filter((s) => s.parentId && s.hostName && onServer.has(s.projectId) && !live.has(s.hostName))
    .map((s) => s.id);
}

/** The persisted per-server seen lists, shape-checked: a hand-edited or older
 *  value degrades to "nothing decided" rather than throwing on boot. */
export function parseSeen(raw: unknown): Record<ServerId, string[]> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const out: Record<ServerId, string[]> = {};
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    if (Array.isArray(v)) out[k] = v.filter((x): x is string => typeof x === "string");
  }
  return out;
}

/** Poll cadence: quicker while anything on the server is working or waiting,
 *  because that is when an agent is likely to spawn. */
export const FAST_MS = 3_000;
export const SLOW_MS = 10_000;
export function pollInterval(busy: boolean): number {
  return busy ? FAST_MS : SLOW_MS;
}
