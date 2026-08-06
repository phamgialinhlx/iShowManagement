import type { TargetRef } from "./api";

/**
 * The v3 workspace domain: **Server → Project → Session (kind)**.
 *
 * This replaces the flat `Session[]` model (`sessions.ts`, `rmux.workspace.v2`),
 * where one fused `Session` carried a target, a folder, one Claude conversation
 * and a bag of terminal tabs. Here those are separate entities:
 *
 * - a **Server** is a connection identity (`TargetRef`), the local machine
 *   included — it is `TargetId` promoted into the UI;
 * - a **Project** is an absolute folder on a Server;
 * - a **Session** has a `kind` — `terminal` or `claude` — and the two are full
 *   peers under a Project.
 *
 * See `references/server-redesign/` (glossary + ADR-001/002) for the reasoning.
 *
 * **This module is pure and has no runtime imports** (the `TargetRef` import is
 * type-only and erased). Keep it that way: the store and the migration test both
 * depend on it, and a runtime import would boot the Zustand store as a side
 * effect of the test.
 */

/** NUL — the one byte a host, user or folder name cannot contain, so joining on
 *  it makes ids that no printable path can forge (same reason `groupKey` uses
 *  it in `session-groups.ts`). */
const SEP = "\u0000";

export type ServerId = string;
export type ProjectId = string;

export type Server = {
  /** Derived from the target — see `serverId`. `"local"` for the local machine. */
  id: ServerId;
  target: TargetRef;
};

export type Project = {
  /** Derived from `(serverId, folder)` — see `projectId`. */
  id: ProjectId;
  serverId: ServerId;
  /** The absolute folder. This *is* the Project's identity. */
  folder: string;
  /** Display name; the folder basename by default. */
  label: string;
  /** Latches once the operator renames it, so `label` stops tracking basename. */
  renamed?: boolean;
};

export type SessionKind = "terminal" | "claude";

/** Runtime attention state. Claude-only in v1 (a terminal is always `idle`). */
export type SessionStatus = "waiting" | "working" | "idle";

/**
 * A Session under a Project. `kind` discriminates; the Claude-only fields are
 * absent on a terminal. Shared fields (`status`, `error`) are runtime and live
 * in the store, not in the persisted shape — same split as the v2 `Session`.
 */
export type SessionV3 = {
  id: string;
  projectId: ProjectId;
  kind: SessionKind;
  name: string;
  renamed?: boolean;

  // ── claude-only ──────────────────────────────────────────────────────────
  resume?: string;
  fullscreen?: boolean;
  skipPermissions?: boolean;
  modelProfile?: string;
  claudeAccount?: string;
  jiraProject?: string;
  contextWindow?: number;
};

/** What a grid tile points at. A session is one kind of pane, not the only kind.
 *  `empty` is a tile cleared **on purpose**: auto-fill must not reuse it —
 *  clearing to `null` would let the layout put the same session straight back,
 *  and a clear control that appears to do nothing is worse than none. */
export type PaneRef =
  | { kind: "session"; id: string }
  | { kind: "host"; serverId: ServerId }
  | { kind: "files"; projectId: ProjectId }
  | { kind: "empty" };

/** What the tab bar holds — everything a pane can show *except* the cleared
 *  tile. Typed as an exclusion so "a tab is never empty" is a compile-time
 *  fact, not a runtime rule someone has to remember. */
export type TabRef = Exclude<PaneRef, { kind: "empty" }>;

/** A pane's deterministic identity — what tabs dedupe by. Joined on NUL like
 *  `serverId`/`projectId`, so no printable id can forge a collision. */
export function paneRefKey(ref: PaneRef): string {
  switch (ref.kind) {
    case "session":
      return ["session", ref.id].join(SEP);
    case "host":
      return ["host", ref.serverId].join(SEP);
    case "files":
      return ["files", ref.projectId].join(SEP);
    case "empty":
      return "empty";
  }
}

export type PersistedV3 = {
  version: 3;
  servers: Server[];
  projects: Project[];
  sessions: SessionV3[];
  /** The grid layout — one entry per cell, `null` for an empty tile. */
  panes: (PaneRef | null)[];
  /** The open set — what the tab bar shows, in creation order, deduped by
   *  `paneRefKey`. The single truth of "open": auto-fill draws from these, and
   *  a session without a tab exists only in the rail. Closing a tab never
   *  touches the session behind it. */
  tabs: TabRef[];
  activeSession: string | null;
  /** Open file paths, now **per project** (were per session). Contents re-read. */
  openPaths: Record<ProjectId, string[]>;
  activePath: Record<ProjectId, string | null>;
};

// ── id derivation (deterministic, so migration and dedup are pure) ───────────

/**
 * A Server's id. Identity is the **ssh alias, never the resolved hostname**
 * (CLAUDE.md invariant), and a differing user or port is a *different*
 * authenticated connection — so all three participate, matching the backend's
 * `SshHostId` and the ControlMaster test that pins "same alias, different user
 * must not share a master".
 */
export function serverId(target: TargetRef): ServerId {
  if (!target.host) return "local";
  return ["ssh", target.host, target.user ?? "", target.port ?? ""].join(SEP);
}

/** A Project's id: its Server plus its folder. */
export function projectId(server: ServerId, folder: string): ProjectId {
  return [server, folder].join(SEP);
}

/**
 * The agent session name a Session reattaches by — **load-bearing**.
 *
 * The daemon holds sessions by this name; getting it wrong orphans running work.
 * The v2 scheme is reproduced exactly: a Claude session is `claude-<id>` (the id
 * is bare, prefixed at use), a terminal's id already carries its `term-` prefix
 * and is used verbatim. Migration therefore preserves ids, and this function
 * yields the identical name it did before the upgrade.
 */
export function reattachName(s: Pick<SessionV3, "id" | "kind">): string {
  return s.kind === "claude" ? `claude-${s.id}` : s.id;
}

/** The folder's last component — what a project is labelled by default. */
export const basename = (path: string): string =>
  path.replace(/\/+$/, "").split("/").filter(Boolean).pop() ?? path;

// ── migration v2 → v3 ────────────────────────────────────────────────────────

/** The v2 persisted shape we read from `rmux.workspace.v2` (structural, so this
 *  module need not import the v2 store). */
export type PersistedV2 = {
  sessions: Array<{
    id: string;
    name: string;
    renamed?: boolean;
    target: TargetRef;
    folder: string;
    resume?: string;
    fullscreen?: boolean;
    skipPermissions?: boolean;
    modelProfile?: string;
    claudeAccount?: string;
    jiraProject?: string;
    contextWindow?: number;
  }>;
  activeSession: string | null;
  terminals: Array<{ id: string; sessionId: string; title: string }>;
  activeTerminal?: Record<string, string | null>;
  openPaths?: Record<string, string[]>;
  activePath?: Record<string, string | null>;
  gridSlots?: (string | null)[];
};

/**
 * Translate a v2 workspace into v3, **preserving every id** so the live
 * `claude-<id>` / `term-<id>` sessions on the host reattach instead of
 * re-spawning. Dropping state (as the v1→v2 bump did) would orphan running work.
 *
 * - each distinct `target` → a Server (first occurrence sets the target);
 * - each `(target, folder)` → a Project (two sessions in one folder share it);
 * - each old fused Session → a **Claude** session, id kept;
 * - each old `TerminalTab` → a **Terminal** session under its parent's Project,
 *   id kept; an orphan terminal (parent session gone) is dropped, because there
 *   is no Project to place it under and its reattach target is unknowable;
 * - `openPaths`/`activePath` re-key from session to project (files are
 *   per-project now), merging the paths of sessions that share a project;
 * - `gridSlots` (cell→sessionId) → session panes.
 *
 * Insertion order follows first appearance in `v2.sessions`, so the rail's
 * server/project order survives the upgrade.
 */
export function migrateV2toV3(v2: PersistedV2): PersistedV3 {
  const servers = new Map<ServerId, Server>();
  const projects = new Map<ProjectId, Project>();
  const sessions: SessionV3[] = [];
  /** old session id → its project, so terminals and open files can be placed. */
  const projectOfSession = new Map<string, ProjectId>();

  const ensureServer = (target: TargetRef): ServerId => {
    const id = serverId(target);
    if (!servers.has(id)) servers.set(id, { id, target });
    return id;
  };
  const ensureProject = (server: ServerId, folder: string): ProjectId => {
    const id = projectId(server, folder);
    if (!projects.has(id)) {
      projects.set(id, { id, serverId: server, folder, label: basename(folder) });
    }
    return id;
  };

  for (const s of v2.sessions) {
    const server = ensureServer(s.target);
    const project = ensureProject(server, s.folder);
    projectOfSession.set(s.id, project);
    sessions.push({
      id: s.id,
      projectId: project,
      kind: "claude",
      name: s.name,
      renamed: s.renamed,
      resume: s.resume,
      fullscreen: s.fullscreen,
      skipPermissions: s.skipPermissions,
      modelProfile: s.modelProfile,
      claudeAccount: s.claudeAccount,
      jiraProject: s.jiraProject,
      contextWindow: s.contextWindow,
    });
  }

  for (const t of v2.terminals ?? []) {
    const project = projectOfSession.get(t.sessionId);
    if (!project) continue; // orphan — no Project to place it under
    sessions.push({ id: t.id, projectId: project, kind: "terminal", name: t.title });
  }

  const openPaths: Record<ProjectId, string[]> = {};
  for (const [oldId, paths] of Object.entries(v2.openPaths ?? {})) {
    const project = projectOfSession.get(oldId);
    if (!project) continue;
    const merged = (openPaths[project] ??= []);
    for (const p of paths) if (!merged.includes(p)) merged.push(p);
  }

  const activePath: Record<ProjectId, string | null> = {};
  for (const [oldId, p] of Object.entries(v2.activePath ?? {})) {
    const project = projectOfSession.get(oldId);
    if (!project || !p) continue;
    activePath[project] ??= p; // first non-null wins when sessions merge
  }

  const panes: (PaneRef | null)[] = (v2.gridSlots ?? []).map((slot) =>
    slot ? { kind: "session", id: slot } : null,
  );

  return {
    version: 3,
    servers: [...servers.values()],
    projects: [...projects.values()],
    sessions,
    panes,
    tabs: seedTabs(panes, sessions),
    activeSession: v2.activeSession ?? null,
    openPaths,
    activePath,
  };
}

// ── tabs: seeding and validation ─────────────────────────────────────────────

/**
 * Reconstruct the open set for a workspace persisted before tabs existed:
 * everything on the deck first (dedup, cleared tiles skipped), then every
 * remaining session in array order. That matches what the old auto-fill (which
 * drew from *all* sessions) put on screen, so the upgraded deck looks exactly
 * like the deck the operator left.
 */
export function seedTabs(
  panes: readonly (PaneRef | null)[],
  sessions: readonly Pick<SessionV3, "id">[],
): TabRef[] {
  const seen = new Set<string>();
  const out: TabRef[] = [];
  for (const p of panes) {
    if (!p || p.kind === "empty") continue;
    const key = paneRefKey(p);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(p);
  }
  for (const s of sessions) {
    const ref: TabRef = { kind: "session", id: s.id };
    const key = paneRefKey(ref);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(ref);
  }
  return out;
}

/** Validate a persisted tab list: drop empties, duplicates, unknown kinds and
 *  refs whose entity no longer exists in the blob. */
export function sanitizeTabs(raw: unknown, ws: PersistedV3): TabRef[] {
  if (!Array.isArray(raw)) return [];
  const seen = new Set<string>();
  const out: TabRef[] = [];
  for (const entry of raw) {
    const t = entry as PaneRef | null;
    if (!t || typeof t !== "object") continue;
    const alive =
      t.kind === "session"
        ? ws.sessions.some((s) => s.id === t.id)
        : t.kind === "host"
          ? ws.servers.some((s) => s.id === t.serverId)
          : t.kind === "files"
            ? ws.projects.some((p) => p.id === t.projectId)
            : false; // "empty" and future kinds are not tabs
    if (!alive) continue;
    const key = paneRefKey(t);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(t as TabRef);
  }
  return out;
}

// ── loading (v3, else migrate v2, else empty) ────────────────────────────────

export const EMPTY_V3: PersistedV3 = {
  version: 3,
  servers: [],
  projects: [],
  sessions: [],
  panes: [],
  tabs: [],
  activeSession: null,
  openPaths: {},
  activePath: {},
};

/**
 * Resolve the persisted workspace from the two possible localStorage blobs,
 * given their raw strings (the caller does the `localStorage.getItem`, so this
 * stays pure and testable).
 *
 * - a v3 blob wins, spread over `EMPTY_V3` so a blob from an older v3 build that
 *   lacks a key added since is not a blank app (the v2 store used this same
 *   defensive spread);
 * - otherwise a v2 blob is **migrated** (never dropped — see `migrateV2toV3`);
 * - otherwise, or on any parse error, a fresh empty workspace.
 */
export function resolveWorkspace(v3raw: string | null, v2raw: string | null): PersistedV3 {
  if (v3raw) {
    try {
      const parsed = JSON.parse(v3raw) as Partial<PersistedV3>;
      const ws: PersistedV3 = { ...EMPTY_V3, ...parsed, version: 3 };
      // Seed on key *absence*, never on emptiness: a blob from before tabs
      // existed gets its open set reconstructed so the deck looks unchanged,
      // while an operator who closed every tab must not have them resurrected
      // on the next launch.
      ws.tabs = "tabs" in parsed ? sanitizeTabs(parsed.tabs, ws) : seedTabs(ws.panes, ws.sessions);
      return ws;
    } catch {
      // fall through — a corrupt v3 blob must not wedge the app
    }
  }
  if (v2raw) {
    try {
      return migrateV2toV3(JSON.parse(v2raw) as PersistedV2);
    } catch {
      // fall through
    }
  }
  return EMPTY_V3;
}

// ── creation (deterministic ids, so re-adding an existing node dedups) ───────

/** A Server for a target — id derived, so two calls for one target are one Server. */
export function makeServer(target: TargetRef): Server {
  return { id: serverId(target), target };
}

/** A Project on a Server — id derived from `(server, folder)`, so re-adding the
 *  same folder returns an equal id and dedups rather than double-creating. */
export function makeProject(server: ServerId, folder: string, label?: string): Project {
  return { id: projectId(server, folder), serverId: server, folder, label: label ?? basename(folder) };
}
