import { create } from "zustand";

import { invoke } from "@tauri-apps/api/core";

import { api, isTauri, type TargetRef } from "./api";
import { bufferKey, type Buffer, type BufferKey } from "./buffers";
import { companionKey, companionName, forget as forgetCompanion } from "./companion";
import { deckId, isFocusDeck, parseDeck, type Deck } from "./grid";
import {
  autosaveDecision,
  autosaveEnabled,
  autosavePending,
  cancelAutosave,
  scheduleAutosave,
  RETRY_DELAY,
} from "./autosave";
import {
  reattachName,
  resolveWorkspace,
  serverId as deriveServerId,
  type PaneRef,
  type PersistedV3,
  type Project,
  type ProjectId,
  type Server,
  type ServerId,
  type SessionKind,
  type SessionStatus,
  type SessionV3,
} from "./workspace-model.ts";
import {
  addProject as rAddProject,
  addServer as rAddServer,
  addSession as rAddSession,
  assignPane as rAssignPane,
  closePane as rClosePane,
  removeSession as rRemoveSession,
  type Core,
} from "./workspace-reducers.ts";

/**
 * The v3 workspace store — **Server → Project → Session (kind)**.
 *
 * This is the thin IO layer over the pure model (`workspace-model.ts`) and pure
 * reducers (`workspace-reducers.ts`): it owns localStorage, the agent kills, and
 * the runtime handles a browser session accumulates. All branching logic lives in
 * the reducers, which are unit-tested; this file is glue and side effects.
 *
 * It replaces `sessions.ts` (`rmux.workspace.v2`). Consumers are moved over in
 * Phase 1; the file/editor buffer subsystem lands in Phase 2 with the files pane.
 */

const V3_KEY = "rmux.workspace.v3";
const V2_KEY = "rmux.workspace.v2";
/** The instruments-rail collapse flag, persisted on its own since before the rail
 *  moved into this store. Kept out of the v3 snapshot so existing installs keep it. */
const WIDGETS_COLLAPSED_KEY = "rmux.widgetRail.collapsed";

/** Guarded so importing this module in a non-browser context (a test) is inert. */
const storage: Pick<Storage, "getItem" | "setItem"> | null =
  typeof localStorage === "undefined" ? null : localStorage;

/** Runtime, per-session — never persisted. `lastStatusAt` is the host-clock ms of
 *  the session's most recent status change; the status stream re-delivers it on
 *  connect, so it does not need to survive a restart. The "unseen" (finished, not
 *  yet looked at) mark is *derived* from it against the persisted `seen`
 *  watermark — see `isSessionUnseen` — rather than being a stored flag. */
export type Runtime = { status: SessionStatus; error: string | null; lastStatusAt?: number };
const IDLE: Runtime = { status: "idle", error: null };

/** The read/unread watermark: the newest status-change time acknowledged, per
 *  session id. Persisted on its own key (like the widgets-collapse flag, kept out
 *  of the v3 snapshot) **on purpose** — a run that finished while the app was
 *  closed then still reads as *unseen* on next launch, the case an in-memory-only
 *  mark misses. This is the model the 0.2.8 tmux tree used (`seen[session]` vs
 *  `statusUpdatedAt`); it replaces the earlier edge-recorded `finishedAt`. */
const SEEN_KEY = "rmux.seen";

function loadSeen(): Record<string, number> {
  try {
    const raw = storage?.getItem(SEEN_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    return parsed && typeof parsed === "object" ? (parsed as Record<string, number>) : {};
  } catch {
    return {};
  }
}

function persistSeen(seen: Record<string, number>) {
  try {
    storage?.setItem(SEEN_KEY, JSON.stringify(seen));
  } catch {
    // A full or disabled localStorage must never stop the rail marking.
  }
}

/** A Claude session is *unseen* when it is idle and its last status change is
 *  newer than what has been acknowledged. Working and waiting are their own
 *  states (handled before this is consulted), so only `idle` can be unseen. Pure
 *  and exported so the rail and its roll-ups all agree on one definition. */
export const isSessionUnseen = (
  runtime: Record<string, Runtime>,
  seen: Record<string, number>,
  id: string,
): boolean => {
  const r = runtime[id];
  return !!r && r.status === "idle" && (r.lastStatusAt ?? 0) > (seen[id] ?? 0);
};

/** A single stable empty target. `targetOf`/`targetOfProject` return this rather
 *  than a fresh `{}`, so a selector reading them keeps a stable reference — a new
 *  `{}` each render breaks useSyncExternalStore and loops to a white screen. */
const EMPTY_TARGET: TargetRef = {};

type State = Core & {
  /** Open file paths, per project. Carried through migration; wired to the files
   *  pane in Phase 2. Persisted so a reopened project restores its tabs. */
  openPaths: Record<ProjectId, string[]>;
  activePath: Record<ProjectId, string | null>;

  /** Attention state, keyed by session id. Separate from the persisted session. */
  runtime: Record<string, Runtime>;
  /** Read/unread watermark, keyed by session id — the newest status-change time
   *  acknowledged. Persisted on its own key (`rmux.seen`); drives the "unseen"
   *  finished mark via `isSessionUnseen`. */
  seen: Record<string, number>;
  /** Live PTY/handle id for an attached session view, so a remount reattaches
   *  instead of re-spawning. Runtime only — dies with this run of the app. */
  live: Record<string, string>;

  /** Open file buffers, **per project** (was per session in v2). Rebuilt lazily
   *  by `restoreFiles`; contents come from the target, so a stale copy is worse
   *  than a brief load. */
  buffers: Record<BufferKey, Buffer>;
  openOrder: Record<ProjectId, BufferKey[]>;
  activeBuffer: Record<ProjectId, BufferKey | null>;

  deck: Deck;
  focusedCell: number | null;
  railCollapsed: boolean;
  /** The instruments (right) rail collapsed to a strip. Unlike the servers rail
   *  this is a lasting preference, so it is persisted (`rmux.widgetRail.collapsed`)
   *  and survives relaunch. */
  widgetsCollapsed: boolean;

  // ── selectors (cheap derivations consumers reach for) ──────────────────────
  serverOf: (sessionOrProjectId: string) => Server | undefined;
  projectOf: (sessionId: string) => Project | undefined;
  targetOf: (sessionId: string) => TargetRef;
  sessionsIn: (projectId: ProjectId) => SessionV3[];
  statusOf: (sessionId: string) => Runtime;

  // ── structure ──────────────────────────────────────────────────────────────
  connectServer: (target: TargetRef) => ServerId;
  createProject: (server: ServerId, folder: string, label?: string) => ProjectId;
  addSession: (
    projectId: ProjectId,
    kind: SessionKind,
    opts?: Partial<
      Pick<
        SessionV3,
        | "name"
        | "resume"
        | "fullscreen"
        | "skipPermissions"
        | "modelProfile"
        | "claudeAccount"
        | "jiraProject"
        | "contextWindow"
      >
    >,
  ) => string;
  removeSession: (id: string) => void;
  removeProject: (id: ProjectId) => void;
  removeServer: (id: ServerId) => void;

  renameSession: (id: string, name: string) => void;
  adoptClaudeTitle: (id: string, title: string) => void;
  renameProject: (id: ProjectId, label: string) => void;
  configureSession: (
    id: string,
    settings: { claudeAccount?: string; jiraProject?: string; contextWindow?: number },
  ) => void;
  setFullscreen: (id: string, fullscreen: boolean) => void;
  setModelProfile: (id: string, profile: string | undefined) => void;
  setResume: (id: string, resume: string) => void;

  // ── attention ────────────────────────────────────────────────────────────
  /** `at` is the host-clock ms of the change (from the status stream). Omitted by
   *  the fallback poll, which has no timestamp — then a working→idle edge is
   *  stamped with the local clock, preserving the old edge-based behaviour. */
  setStatus: (id: string, status: SessionStatus, at?: number) => void;
  setSessionError: (id: string, error: string | null) => void;
  activate: (id: string) => void;

  // ── panes / grid ───────────────────────────────────────────────────────────
  setDeck: (deck: Deck) => void;
  focusCell: (index: number | null) => void;
  assignPane: (index: number, ref: PaneRef | null) => void;
  closePane: (index: number) => void;
  /** The rail's "open into the focused tile" verb, for each openable node. */
  openSession: (id: string) => void;
  openHost: (serverId: ServerId) => void;
  openFiles: (projectId: ProjectId) => void;

  // ── runtime handles ──────────────────────────────────────────────────────
  setLive: (id: string, handle: string) => void;
  clearLive: (id: string) => void;
  toggleRail: () => void;
  toggleWidgets: () => void;

  // ── files (per project) ────────────────────────────────────────────────────
  targetOfProject: (projectId: ProjectId) => TargetRef;
  openFile: (projectId: ProjectId, path: string) => Promise<void>;
  closeBuffer: (key: BufferKey) => void;
  activateBuffer: (projectId: ProjectId, key: BufferKey) => void;
  edit: (key: BufferKey, text: string) => void;
  save: (key: BufferKey) => Promise<void>;
  /** Reopen the files this project had open when the app last closed. */
  restoreFiles: (projectId: ProjectId) => void;
};

function load(): PersistedV3 {
  return resolveWorkspace(storage?.getItem(V3_KEY) ?? null, storage?.getItem(V2_KEY) ?? null);
}

/** Snapshot the persisted subset. The old `rmux.workspace.v2` blob is left in
 *  place as a downgrade fallback — v3 wins on load, so it is inert but not lost. */
function persist(state: State) {
  if (!storage) return;
  // Open files are derived from the live buffers for projects that have been
  // touched, and preserved from the stored map for projects not yet restored —
  // so persisting before a project's files are opened does not wipe its list.
  const openPaths: Record<ProjectId, string[]> = { ...state.openPaths };
  for (const [projectId, keys] of Object.entries(state.openOrder)) {
    openPaths[projectId] = (keys ?? [])
      .map((k) => state.buffers[k]?.path)
      .filter((p): p is string => !!p);
  }
  const activePath: Record<ProjectId, string | null> = { ...state.activePath };
  for (const [projectId, key] of Object.entries(state.activeBuffer)) {
    activePath[projectId] = key ? (state.buffers[key]?.path ?? null) : null;
  }

  const snapshot: PersistedV3 = {
    version: 3,
    servers: state.servers,
    projects: state.projects,
    sessions: state.sessions,
    panes: state.panes,
    activeSession: state.activeSession,
    openPaths,
    activePath,
  };
  try {
    storage.setItem(V3_KEY, JSON.stringify(snapshot));
  } catch {
    // A full or disabled localStorage must never stop the app working.
  }
}

let pending = false;
function schedulePersist(read: () => State) {
  if (pending) return;
  pending = true;
  queueMicrotask(() => {
    pending = false;
    persist(read());
  });
}

let seq = 0;
/**
 * Mint a session id. **The prefix is load-bearing** (see `reattachName`): a
 * Claude id must be *bare* (the `claude-` prefix is added at reattach), while a
 * terminal id must carry its `term-` prefix, because it is used verbatim.
 */
const mintSessionId = (kind: SessionKind): string =>
  `${kind === "claude" ? "session" : "term"}-${Date.now().toString(36)}-${++seq}`;

const restored = load();

/** Just the reducer core, for handing to a reducer and merging its result back. */
const coreOf = (s: State): Core => ({
  servers: s.servers,
  projects: s.projects,
  sessions: s.sessions,
  panes: s.panes,
  activeSession: s.activeSession,
});

/** The cell a rail "open" should fill: cell 0 in focus mode (the only visible
 *  one), else the focused cell, else cell 0. */
const openCell = (s: State): number => (isFocusDeck(s.deck) ? 0 : (s.focusedCell ?? 0));

export const useWorkspace = create<State>((set, get) => ({
  servers: restored.servers,
  projects: restored.projects,
  sessions: restored.sessions,
  panes: restored.panes,
  activeSession: restored.activeSession,
  openPaths: restored.openPaths,
  activePath: restored.activePath,

  // Runtime is rebuilt fresh: every restored session starts idle (a stale badge
  // from a previous run has already been seen), and reattaching happens by name.
  runtime: {},
  seen: loadSeen(),
  live: {},

  // Buffers are rebuilt lazily by `restoreFiles`; their contents are re-read
  // from the target, and a stale copy of a file someone else changed is worse
  // than a brief load.
  buffers: {},
  openOrder: {},
  activeBuffer: {},

  // `rmux.deck` is the new key; `rmux.grid` is the old bare number every
  // existing install still has. Reading both means an upgrade keeps the
  // layout instead of silently dropping everyone into focus mode.
  deck: parseDeck(storage?.getItem("rmux.deck") ?? storage?.getItem("rmux.grid")),
  focusedCell: null,
  railCollapsed: false,
  widgetsCollapsed: storage?.getItem(WIDGETS_COLLAPSED_KEY) === "1",

  // ── selectors ──────────────────────────────────────────────────────────────
  projectOf: (sessionId) => {
    const s = get().sessions.find((x) => x.id === sessionId);
    return s ? get().projects.find((p) => p.id === s.projectId) : undefined;
  },
  serverOf: (sessionOrProjectId) => {
    const { projects, servers } = get();
    const project =
      projects.find((p) => p.id === sessionOrProjectId) ?? get().projectOf(sessionOrProjectId);
    return project ? servers.find((sv) => sv.id === project.serverId) : undefined;
  },
  targetOf: (sessionId) => get().serverOf(sessionId)?.target ?? EMPTY_TARGET,
  sessionsIn: (projectId) => get().sessions.filter((s) => s.projectId === projectId),
  statusOf: (sessionId) => get().runtime[sessionId] ?? IDLE,

  // ── structure ──────────────────────────────────────────────────────────────
  connectServer: (target) => {
    const { ws, id } = rAddServer(coreOf(get()), target);
    set(ws);
    schedulePersist(get);
    return id;
  },

  createProject: (server, folder, label) => {
    const { ws, id } = rAddProject(coreOf(get()), server, folder, label);
    set(ws);
    schedulePersist(get);
    return id;
  },

  addSession: (projectId, kind, opts) => {
    const id = mintSessionId(kind);
    const project = get().projects.find((p) => p.id === projectId);
    const name = opts?.name?.trim() || (kind === "claude" ? (project?.label ?? "claude") : "shell");
    // `opts` first, then the authoritative fields — so a computed `name` and the
    // real id/kind/projectId win over anything the caller passed loosely.
    const ws = rAddSession(coreOf(get()), { ...opts, id, projectId, kind, name });
    set({ ...ws, runtime: { ...get().runtime, [id]: { ...IDLE } } });
    schedulePersist(get);
    return id;
  },

  removeSession: (id) => {
    const state = get();
    const session = state.sessions.find((s) => s.id === id);
    if (session && isTauri()) {
      // The kill is the point — the shell/Claude runs under the agent and would
      // otherwise leak on the host. Reattach name and target are resolved the
      // same way an attach does, so the right session on the right host dies.
      const target = state.targetOf(id);
      const name = reattachName(session);
      if (session.kind === "claude") {
        void api.claudeEndSession(target, name).catch(() => {});
        // **And its companion shell**, which is a real terminal under the agent
        // and survives the app. It is not in the rail, so nothing else would
        // ever reach it — exactly the unreachable leak `rmux-agent list` was
        // added to make findable. Sent unconditionally: a session that never
        // opened one has nothing to kill and the call is harmless.
        void invoke("terminal_close", {
          id: state.live[companionKey(id)] ?? "",
          target,
          session: companionName(id),
        }).catch(() => {});
        forgetCompanion(id);
      } else {
        void invoke("terminal_close", {
          id: state.live[id] ?? "",
          target,
          session: name,
        }).catch(() => {});
      }
    }
    const ws = rRemoveSession(coreOf(state), id);
    const { [id]: _r, ...runtime } = state.runtime;
    const { [id]: _l, [companionKey(id)]: _c, ...live } = state.live;
    // Prune the watermark too, or `seen` grows without bound as sessions come and
    // go — it shares the localStorage quota with everything else.
    const { [id]: _s, ...seen } = state.seen;
    if (Object.keys(seen).length !== Object.keys(state.seen).length) persistSeen(seen);
    set({ ...ws, runtime, live, seen });
    schedulePersist(get);
  },

  removeProject: (id) => {
    // Cascade: end every session in the project first (each fires its own kill),
    // then drop the now-empty project. Removing a session mutates the store, so
    // snapshot the ids before iterating.
    for (const s of get().sessions.filter((x) => x.projectId === id)) get().removeSession(s.id);
    set((s) => {
      const buffers = Object.fromEntries(
        Object.entries(s.buffers).filter(([, b]) => b.projectId !== id),
      );
      const { [id]: _o, ...openOrder } = s.openOrder;
      const { [id]: _a, ...activeBuffer } = s.activeBuffer;
      return { projects: s.projects.filter((p) => p.id !== id), buffers, openOrder, activeBuffer };
    });
    schedulePersist(get);
  },

  removeServer: (id) => {
    for (const p of get().projects.filter((x) => x.serverId === id)) get().removeProject(p.id);
    set((s) => ({ servers: s.servers.filter((sv) => sv.id !== id) }));
    schedulePersist(get);
  },

  renameSession: (id, name) => {
    set((s) => ({
      sessions: s.sessions.map((x) =>
        // `renamed` latches: Claude's own title is ignored from here on.
        x.id === id ? { ...x, name: name.trim() || x.name, renamed: true } : x,
      ),
    }));
    schedulePersist(get);
  },

  adoptClaudeTitle: (id, title) => {
    const clean = title.trim();
    if (!clean) return;
    const current = get().sessions.find((x) => x.id === id);
    if (!current || current.renamed || current.name === clean) return;
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, name: clean } : x)) }));
    schedulePersist(get);
  },

  renameProject: (id, label) => {
    const clean = label.trim();
    if (!clean) return;
    set((s) => ({
      projects: s.projects.map((p) => (p.id === id ? { ...p, label: clean, renamed: true } : p)),
    }));
    schedulePersist(get);
  },

  configureSession: (id, settings) => {
    set((s) => ({
      sessions: s.sessions.map((x) =>
        x.id === id
          ? {
              ...x,
              // "" clears; undefined leaves alone — the two must not collapse.
              claudeAccount:
                settings.claudeAccount === undefined ? x.claudeAccount : settings.claudeAccount || undefined,
              jiraProject:
                settings.jiraProject === undefined ? x.jiraProject : settings.jiraProject || undefined,
              contextWindow:
                settings.contextWindow === undefined ? x.contextWindow : settings.contextWindow || undefined,
            }
          : x,
      ),
    }));
    schedulePersist(get);
  },

  setFullscreen: (id, fullscreen) => {
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, fullscreen } : x)) }));
    schedulePersist(get);
  },
  setModelProfile: (id, modelProfile) => {
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, modelProfile } : x)) }));
    schedulePersist(get);
  },
  setResume: (id, resume) => {
    if (!resume) return;
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, resume } : x)) }));
    schedulePersist(get);
  },

  // ── attention ────────────────────────────────────────────────────────────
  setStatus: (id, status, at) =>
    set((s) => {
      const cur = s.runtime[id] ?? IDLE;
      // When this change happened, on the host clock: the stream's own stamp if we
      // have it, else — for the fallback poll — synthesized on the working→idle
      // edge, exactly as the old edge-recorded mark did. Never goes backwards.
      const lastStatusAt =
        at != null
          ? Math.max(at, cur.lastStatusAt ?? 0)
          : cur.status === "working" && status !== "working"
            ? Date.now()
            : cur.lastStatusAt;

      if (cur.status === status && lastStatusAt === cur.lastStatusAt) return s;

      const runtime = { ...s.runtime, [id]: { status, error: cur.error, lastStatusAt } };

      // Seed the watermark the first time this session is ever seen with a time,
      // so a session nobody has looked at does not start out "unseen". A session
      // already seen keeps its persisted watermark — so a change past it, even one
      // that happened while the app was closed, reads as unseen on next launch.
      if (lastStatusAt != null && s.seen[id] === undefined) {
        const seen = { ...s.seen, [id]: lastStatusAt };
        persistSeen(seen);
        return { runtime, seen };
      }
      return { runtime };
    }),

  setSessionError: (id, error) =>
    set((s) => {
      const cur = s.runtime[id] ?? IDLE;
      if (cur.error === error) return s;
      return { runtime: { ...s.runtime, [id]: { ...cur, error } } };
    }),

  activate: (id) => {
    // Looking at a session acknowledges it up to its latest status change — never
    // on a timer. Advancing the watermark to now-known time clears the unseen mark.
    set((s) => {
      const mark = s.runtime[id]?.lastStatusAt ?? Date.now();
      if ((s.seen[id] ?? 0) >= mark) return { activeSession: id };
      const seen = { ...s.seen, [id]: mark };
      persistSeen(seen);
      return { activeSession: id, seen };
    });
    schedulePersist(get);
  },

  // ── panes / grid ───────────────────────────────────────────────────────────
  setDeck: (deck) => {
    storage?.setItem("rmux.deck", deckId(deck));
    // A cell selected under the old layout may not exist under the new one.
    set({ deck, focusedCell: null });
  },
  focusCell: (index) => set({ focusedCell: index }),
  assignPane: (index, ref) => {
    set(rAssignPane(coreOf(get()), index, ref));
    schedulePersist(get);
  },
  closePane: (index) => {
    set(rClosePane(coreOf(get()), index));
    schedulePersist(get);
  },
  // Focus mode (grid 1) only ever *shows* cell 0, so opening must target it —
  // a stale `focusedCell` left from a grid interaction would otherwise send the
  // pane to a hidden cell and the view would appear not to change.
  openSession: (id) => {
    get().activate(id);
    get().assignPane(openCell(get()), { kind: "session", id });
  },
  openHost: (serverId) => get().assignPane(openCell(get()), { kind: "host", serverId }),
  openFiles: (projectId) => get().assignPane(openCell(get()), { kind: "files", projectId }),

  // ── runtime handles ──────────────────────────────────────────────────────
  setLive: (id, handle) => set((s) => ({ live: { ...s.live, [id]: handle } })),
  clearLive: (id) =>
    set((s) => {
      const { [id]: _gone, ...live } = s.live;
      return { live };
    }),
  toggleRail: () => set((s) => ({ railCollapsed: !s.railCollapsed })),
  toggleWidgets: () =>
    set((s) => {
      const widgetsCollapsed = !s.widgetsCollapsed;
      // Persisted on its own key, not through the v3 snapshot — see the note there.
      try {
        storage?.setItem(WIDGETS_COLLAPSED_KEY, widgetsCollapsed ? "1" : "0");
      } catch {
        // A full localStorage must not stop the panel toggling.
      }
      return { widgetsCollapsed };
    }),

  // ── files (per project) ────────────────────────────────────────────────────
  targetOfProject: (projectId) => get().serverOf(projectId)?.target ?? EMPTY_TARGET,

  openFile: async (projectId, path) => {
    const key = bufferKey(projectId, path);
    const project = get().projects.find((p) => p.id === projectId);
    if (!project) return;
    const target = get().targetOfProject(projectId);

    if (get().buffers[key]) {
      set((s) => ({
        activeBuffer: { ...s.activeBuffer, [projectId]: key },
        openOrder: {
          ...s.openOrder,
          [projectId]: s.openOrder[projectId]?.includes(key)
            ? s.openOrder[projectId]!
            : [...(s.openOrder[projectId] ?? []), key],
        },
      }));
      return;
    }

    set((s) => ({
      buffers: {
        ...s.buffers,
        [key]: {
          key,
          projectId,
          path,
          saved: "",
          text: "",
          content: null,
          loading: true,
          error: null,
          saving: false,
          saveError: null,
        },
      },
      openOrder: { ...s.openOrder, [projectId]: [...(s.openOrder[projectId] ?? []), key] },
      activeBuffer: { ...s.activeBuffer, [projectId]: key },
    }));
    schedulePersist(get);

    try {
      const content = await api.fsRead(target, path);
      set((s) => {
        const b = s.buffers[key];
        if (!b) return s;
        const text = content.kind === "text" ? content.text : "";
        return { buffers: { ...s.buffers, [key]: { ...b, content, text, saved: text, loading: false } } };
      });
    } catch (e) {
      set((s) => {
        const b = s.buffers[key];
        if (!b) return s;
        return {
          buffers: {
            ...s.buffers,
            [key]: { ...b, loading: false, error: e instanceof Error ? e.message : String(e) },
          },
        };
      });
    }
  },

  closeBuffer: (key) => {
    // Closing a tab must not lose work: a pending autosave is flushed, not
    // cancelled (started before the buffer is dropped, since `save` reads it).
    if (autosavePending(key) && autosaveDecision(get().buffers[key]) !== "skip") {
      void get().save(key);
    }
    cancelAutosave(key);

    set((s) => {
      const buffer = s.buffers[key];
      if (!buffer) return s;
      const projectId = buffer.projectId;
      const { [key]: _dropped, ...buffers } = s.buffers;
      const order = (s.openOrder[projectId] ?? []).filter((k) => k !== key);
      const active =
        s.activeBuffer[projectId] === key
          ? (order[order.length - 1] ?? null)
          : (s.activeBuffer[projectId] ?? null);
      queueMicrotask(() => schedulePersist(get));
      return {
        buffers,
        openOrder: { ...s.openOrder, [projectId]: order },
        activeBuffer: { ...s.activeBuffer, [projectId]: active },
      };
    });
  },

  activateBuffer: (projectId, key) => {
    set((s) => ({ activeBuffer: { ...s.activeBuffer, [projectId]: key } }));
    schedulePersist(get);
  },

  edit: (key, text) => {
    set((s) => {
      const b = s.buffers[key];
      if (!b) return s;
      return { buffers: { ...s.buffers, [key]: { ...b, text } } };
    });
    if (!autosaveEnabled()) return;
    scheduleAutosave(key, function attempt() {
      const decision = autosaveDecision(get().buffers[key]);
      if (decision === "retry") return scheduleAutosave(key, attempt, RETRY_DELAY);
      if (decision === "write") void get().save(key);
    });
  },

  save: async (key) => {
    const buffer = get().buffers[key];
    if (!buffer || buffer.saving || buffer.content?.kind !== "text") return;
    const target = get().targetOfProject(buffer.projectId);

    set((s) => ({ buffers: { ...s.buffers, [key]: { ...s.buffers[key]!, saving: true, saveError: null } } }));
    // Captured before the await: the user may keep typing during the write.
    const written = buffer.text;
    try {
      await api.fsWrite(target, buffer.path, written);
      set((s) => {
        const cur = s.buffers[key];
        if (!cur) return s;
        return { buffers: { ...s.buffers, [key]: { ...cur, saved: written, saving: false } } };
      });
    } catch (e) {
      set((s) => {
        const cur = s.buffers[key];
        if (!cur) return s;
        return {
          buffers: {
            ...s.buffers,
            [key]: { ...cur, saving: false, saveError: e instanceof Error ? e.message : String(e) },
          },
        };
      });
    }
  },

  restoreFiles: (projectId) => {
    // Already open in this run — do not resurrect files the operator closed.
    if (get().openOrder[projectId]?.length) return;
    const paths = get().openPaths[projectId];
    if (!paths?.length) return;
    const activePath = get().activePath[projectId];
    // Consume: clear the stored lists so a later persist (derived from the live
    // buffers) is the source of truth from here on.
    set((s) => {
      const { [projectId]: _p, ...openPaths } = s.openPaths;
      const { [projectId]: _a, ...activePaths } = s.activePath;
      return { openPaths, activePath: activePaths };
    });
    void (async () => {
      for (const path of paths) await get().openFile(projectId, path);
      if (activePath) get().activateBuffer(projectId, bufferKey(projectId, activePath));
    })();
  },
}));

// Keep the same derived-server-id helper reachable for callers that only have a
// target (the new-session wizard resolves a target → server before creating a
// project under it).
export { deriveServerId as serverId };

// Dev-only handle, mirroring `sessions.ts`, so the workbench can be driven from
// the console without the Tauri bridge.
if (import.meta.env.DEV) {
  (window as unknown as Record<string, unknown>).__rmuxWorkspace = useWorkspace;
}
