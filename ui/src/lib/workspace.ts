import { create } from "zustand";

import { invoke } from "./bridge";

import { api, CONFLICT_PREFIX, hasBackend, type TargetRef } from "./api";
import { isUnseen } from "./agent-state";
import { bufferKey, type Buffer, type BufferKey } from "./buffers";
import { companionKey, companionName, forget as forgetCompanion } from "./companion";
import { isPrimaryWindow, scopedKey } from "./window-scope";
import { requestTerminalFocus } from "./shortcuts";
import {
  activeTab as layoutActiveTab,
  addTab,
  addTreeTab,
  cloneTree,
  closeLeaf as lCloseLeaf,
  cycleTab as lCycleTab,
  findLeaf,
  focusInDirection,
  focusLeaf as lFocusLeaf,
  focusedLeaf,
  locatePane,
  locateSession,
  closeTile as lCloseTile,
  nextUnshown,
  openBeside as lOpenBeside,
  placeTile as lPlaceTile,
  setViewRatio as lSetViewRatio,
  migrateLayoutV3,
  placeChild,
  moveToTab as lMoveToTab,
  moveToNewTab as lMoveToNewTab,
  migrateV3toV4,
  moveTab as lMoveTab,
  moveBeside as lMoveBeside,
  canMoveBeside,
  resolveMoveSource,
  leaves as treeLeaves,
  normalizeLayout,
  parseLayout,
  parseSavedViews,
  removeSessionLeaves,
  removeTab,
  renameTab as lRenameTab,
  selectProject as lSelectProject,
  selectTab as lSelectTab,
  selectTabIndex as lSelectTabIndex,
  sessionsInTab,
  setTabRatio,
  splitFocused,
  swapInDirection,
  tabsOf,
  toggleZoom as lToggleZoom,
  type Area,
  type Direction,
  type Layout,
  type MoveSource,
  type SplitNode,
  type MoveTarget,
  type PersistedV4,
  type SavedView,
} from "./layout-tree.ts";
import {
  autosaveDecision,
  autosaveEnabled,
  autosavePending,
  cancelAutosave,
  scheduleAutosave,
  RETRY_DELAY,
} from "./autosave";
import {
  mintStem,
  reattachName,
  resolveWorkspace,
  serverId as deriveServerId,
  type PaneRef,
  type Project,
  type ProjectId,
  type Server,
  type ServerId,
  type SessionKind,
  type SessionStatus,
  type SessionV3,
} from "./workspace-model.ts";
import type { ChildDecision } from "./children.ts";
import {
  addProject as rAddProject,
  addServer as rAddServer,
  setServerIdentity as rSetServerIdentity,
  addSession as rAddSession,
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

// The **pool** — servers, projects, sessions and their open files — is shared by
// every window: a second workbench should carry all your servers and sessions,
// not open blank. So these keys are *unscoped*, and a cross-window `storage`
// listener (below) keeps each open window's pool in step.
//
// v4 replaced the grid with tabs and split trees (`layout-tree.ts`). The v3 and
// v2 blobs are read for migration and then left in place, unwritten, as a
// downgrade fallback — exactly what v3 did with v2.
const V4_KEY = "rmux.workspace.v4";
const V3_KEY = "rmux.workspace.v3";
const V2_KEY = "rmux.workspace.v2";
// The **layout** — the active project, each project's tabs and their split
// trees, the active session — is per-window, so each desktop can show a
// different view of the same pool without the windows fighting over one
// another's panes. Scoped by window label (see `window-scope.ts`); the primary
// window keeps the bare key.
const LAYOUT_KEY = scopedKey("rmux.workspace.layout.v4");
/** The v3 per-window layout (grid panes + active session) — read once, so an
 *  extra window migrating to v4 keeps the session it had in front of it. */
const LAYOUT_V3_KEY = scopedKey("rmux.workspace.layout");
/** The instruments-rail collapse flag, persisted on its own since before the rail
 *  moved into this store. Kept out of the v3 snapshot so existing installs keep it. */
const WIDGETS_COLLAPSED_KEY = "rmux.widgetRail.collapsed";
/** The sidebar's compact mode (herdr's collapsed rail), persisted on its own key
 *  for the same reason as the widgets flag. */
const RAIL_COLLAPSED_KEY = "rmux.rail.collapsed";

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

/**
 * The folder of the synthetic project holding sessions adopted from a host.
 *
 * Not a path, and deliberately not one that could be: every real project is
 * created from an absolute folder, so this can never collide with one. The
 * project id is derived from `(server, folder)`, which makes the bucket dedupe
 * itself without a scan — and without the failure mode of guessing the bucket
 * from a folder someone might genuinely have opened.
 */
const RUNNING_FOLDER = "(running)";

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

/** Saved views live in their own key — small, and unrelated to the session tree,
 *  so folding them into the workspace blob would only widen its churn. */
const SAVED_VIEWS_KEY = "rmux.savedViews";

function loadSavedViews(): SavedView[] {
  try {
    const raw = storage?.getItem(SAVED_VIEWS_KEY);
    // Shape-checked, because a hand-edited or older value must degrade to "no
    // saved views" rather than crash the workbench on boot. A view saved from
    // the old grid (`{ deck, panes }`) is carried forward as an equal-share
    // tree, not dropped.
    return parseSavedViews(raw ? JSON.parse(raw) : null, mint);
  } catch {
    return [];
  }
}

function persistSavedViews(views: SavedView[]) {
  try {
    storage?.setItem(SAVED_VIEWS_KEY, JSON.stringify(views));
  } catch {
    // A full or disabled localStorage still lets the views work this session.
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
): boolean => isUnseen(runtime[id], seen[id]);

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

  /**
   * This window's arrangement: the active project, each project's tabs and
   * their split trees (`layout-tree.ts`). Kept in step with the pool by
   * `normalizeLayout` — every session in exactly one leaf of one tab of its own
   * project — on load and on every pool change.
   */
  layout: Layout;
  /** Tab ids most-recently shown first — feeds the warm set (`warmTabs`).
   *  Runtime only; a relaunch starts warm with just the active tab. */
  recentTabs: string[];
  /** Named split trees the operator saved, persisted so they survive relaunch.
   *  See `saveCurrentView`/`applyView`. */
  savedViews: SavedView[];
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
  /** Change which key a Server offers — in place; its id never changes. */
  setServerIdentity: (id: ServerId, identity: string | undefined) => void;
  createProject: (server: ServerId, folder: string, label?: string) => ProjectId;
  addSession: (
    projectId: ProjectId,
    kind: SessionKind,
    opts?: Partial<
      Pick<
        SessionV3,
        | "name"
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
        | "hostName"
        | "bypassApprovals"
        | "codexSandbox"
        | "run"
      >
    >,
  ) => string;
  removeSession: (id: string) => void;
  /** Drop this machine's view; the session keeps running on the host. */
  detachSession: (id: string) => void;
  /**
   * Take over a session already running on `serverId`, by its host name.
   *
   * Idempotent: a session already in the rail is opened rather than adopted
   * twice. Returns the row's id.
   */
  adoptServerSession: (
    serverId: ServerId,
    hostName: string,
    kind: SessionKind,
    alias?: string | null,
    /** The session's real working directory on the host, when known. */
    cwd?: string | null,
  ) => string;
  /**
   * Adopt a sub-session an agent spawned on the host (`children.ts` decided
   * it): create its row and place it **beside its parent without moving the
   * focus or the keyboard** — a split of the parent's pane when the parent is in
   * a tab of the child's project, else a new tab there, not activated. Unlike
   * `addSession` nothing is activated: the operator may be mid-sentence in
   * another pane. Idempotent by daemon name. Returns the row's id, or null when
   * the decision no longer applies.
   */
  adoptChild: (serverId: ServerId, decision: ChildDecision, aspect?: number, area?: Area | null) => string | null;
  /**
   * Move every adopted child that sits in a different project from its parent
   * back beside the parent, and drop a project that move left empty. Repairs
   * children placed by the old folder rule (a `~/x` → `/data/x` symlink put the
   * child in a second project of the same name). Returns the ids it moved.
   */
  rehomeChildren: (aspect?: number, area?: Area | null) => string[];
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
  /** Session Settings › TRANSPORT. Absent follows the app default — see `SessionV3.lowLatency`. */
  setLowLatency: (id: string, lowLatency: boolean) => void;
  /** Forget this session's own choice, so it follows the app-wide default again. */
  clearLowLatency: (id: string) => void;
  setModelProfile: (id: string, profile: string | undefined) => void;
  setActiveProfile: (id: string, activeProfile: string | undefined) => void;
  /**
   * Session Settings › CODEX: the sandbox mode and the explicit bypass. Kept
   * with the session (a restart comes back the same), never an app default.
   */
  setCodexLaunch: (
    id: string,
    launch: { bypassApprovals?: boolean; codexSandbox?: "read-only" | "workspace-write" | null },
  ) => void;
  setResume: (id: string, resume: string) => void;

  // ── attention ────────────────────────────────────────────────────────────
  /** `at` is the host-clock ms of the change (from the status stream). Omitted by
   *  the fallback poll, which has no timestamp — then a working→idle edge is
   *  stamped with the local clock, preserving the old edge-based behaviour. */
  setStatus: (id: string, status: SessionStatus, at?: number) => void;
  setSessionError: (id: string, error: string | null) => void;
  /**
   * Bring a session in front of the operator: select its project and tab,
   * focus its pane, mark it seen — and, unless `keyboard: false`, give its
   * terminal the keyboard. Hands-free, the waiting chip, notifications, alerts
   * and the rail all mean this. A click *inside* a pane passes
   * `keyboard: false`, because the click already placed the cursor and taking
   * it would pull focus off whatever control was pressed.
   */
  activate: (id: string, opts?: { keyboard?: boolean }) => void;

  // ── projects / tabs / panes (herdr's model, `layout-tree.ts`) ─────────────
  selectProject: (projectId: ProjectId) => void;
  /** `keyboard: false` switches without taking the keyboard — a spring-loaded
   *  tab under a drag. */
  selectTab: (projectId: ProjectId, tabId: string, opts?: { keyboard?: boolean }) => void;
  /** The active project's tab at a 0-based index. */
  selectTabIndex: (index: number) => void;
  /** Next (+1) / previous (-1) tab in the active project, wrapping. */
  cycleTab: (delta: number) => void;
  /** A new tab holding a new terminal in `projectId` (default: the active
   *  project). Returns the new session's id, or null with no project. */
  newTab: (projectId?: ProjectId) => string | null;
  /** Rename a tab; an empty name gives the label back to its focused session. */
  renameTab: (projectId: ProjectId, tabId: string, name: string) => void;
  /** Close a tab. **Ends every session in it** (as the rail's ✕ does) — the
   *  caller confirms first and names what ends. */
  closeTab: (projectId: ProjectId, tabId: string) => void;
  moveTab: (projectId: ProjectId, from: number, to: number) => void;
  /**
   * Can `source` be dropped beside `target`? `null` when it can; `"noop"` for a
   * drop on itself or inside its own subtree (no zone is shown); `"server"` when
   * it would cross to another server (refused — a project belongs to one).
   */
  moveRefusal: (source: MoveSource, target: MoveTarget) => "noop" | "server" | null;
  /**
   * Drag-to-split: put `source` on `side` of the `target` pane
   * (`layout-tree.ts` `moveBeside`). **Moving is never closing** — structural
   * only, no session ends. A session dropped into another project of the same
   * server is re-homed there in the same write as the layout. Returns whether it
   * moved.
   */
  moveBeside: (source: MoveSource, target: MoveTarget, side: Direction) => boolean;
  /**
   * Can `source` go into the tab `tabId` of `projectId` — or, with `tabId`
   * null, into a new tab there? Same answers as `moveRefusal`: `"noop"` when it
   * is already there, `"server"` across servers.
   */
  tabMoveRefusal: (source: MoveSource, projectId: ProjectId, tabId: string | null) => "noop" | "server" | null;
  /** Move `source` into an existing tab, beside its largest pane
   *  (`layout-tree.ts` `moveToTab`). Structural only — nothing ends. */
  moveToTab: (source: MoveSource, projectId: ProjectId, tabId: string) => boolean;
  /** Break `source` out into a new tab of `projectId` (tmux `break-pane`).
   *  Structural only — nothing ends. */
  moveToNewTab: (source: MoveSource, projectId: ProjectId) => boolean;
  focusLeaf: (projectId: ProjectId, tabId: string, leafId: string, opts?: { keyboard?: boolean }) => void;
  /** Move the focus to the geometric neighbour, keyboard included. */
  focusDirection: (dir: Direction) => void;
  /** Split the focused pane; the new half is a new terminal in the same
   *  project. Returns its session id, or null when nothing is focused. */
  splitPane: (dir: "right" | "down") => string | null;
  /** Close a pane. A session pane **ends that session** (as the rail's ✕) — the
   *  caller confirms first; a host or files pane just closes. */
  closeLeaf: (projectId: ProjectId, tabId: string, leafId: string) => void;
  toggleZoom: () => void;
  swapPane: (dir: Direction) => void;
  /** Commit a divider position (clamped 0.1–0.9). Called on release, not on
   *  every pointer move — see `SplitTab`. */
  setSplitRatio: (projectId: ProjectId, tabId: string, splitId: string, ratio: number) => void;

  // ── the project grid (`layout-tree.ts`, "the project grid") ──────────────
  /** Open a project in a new tile beside the focused one (right when that tile
   *  is wider than tall, else below). Already on screen: focus its tile. */
  openBeside: (projectId: ProjectId) => void;
  /** Open the next sidebar project not on screen beside the focused tile.
   *  Returns false when every project is already shown. */
  openNextBeside: () => boolean;
  /** Place a project on one side of a tile (a sidebar drag onto a tile edge);
   *  a project already shown moves there. */
  placeProject: (projectId: ProjectId, tileId: string, side: Direction) => void;
  /** Remove a tile. **Ends nothing** — its sessions keep running and its tabs
   *  stay; the project is one click away in the sidebar. */
  closeTile: (tileId: string) => void;
  /** Commit a tile divider position, on release. */
  setViewRatio: (splitId: string, ratio: number) => void;

  // ── saved views ──────────────────────────────────────────────────────────
  /** Save the active tab's split tree under a name (replaces one of the same
   *  name, so re-saving updates it). */
  saveCurrentView: (name: string) => void;
  /** Open a saved view as a new tab. Panes whose session no longer exists are
   *  skipped; sessions it names are *moved* into it, never mounted twice. */
  applyView: (id: string) => void;
  deleteView: (id: string) => void;
  /** The rail's open verbs. A session is activated where it lives; a host or
   *  files pane is focused where one already is, else opened as a new tab. */
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

/** The per-window layout blob. */
type WindowLayout = { version: 4; layout: Layout; activeSession: string | null };

function readJson(key: string): unknown {
  const raw = storage?.getItem(key);
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/**
 * Resolve the shared pool: a v4 blob wins; otherwise v3 (itself migrated from
 * v2 when that is all there is) is **migrated**, never dropped — every id kept,
 * every session given a tab. Exported shape-checking lives in the pure module.
 */
function resolvePool(): PersistedV4 {
  const v4 = readJson(V4_KEY) as Partial<PersistedV4> | null;
  if (v4 && typeof v4 === "object") {
    const base = resolveWorkspace(null, null);
    return {
      version: 4,
      servers: v4.servers ?? base.servers,
      projects: v4.projects ?? base.projects,
      sessions: v4.sessions ?? base.sessions,
      activeSession: v4.activeSession ?? null,
      openPaths: v4.openPaths ?? {},
      activePath: v4.activePath ?? {},
      layout: parseLayout(v4.layout),
    };
  }
  const v3 = resolveWorkspace(storage?.getItem(V3_KEY) ?? null, storage?.getItem(V2_KEY) ?? null);
  return migrateV3toV4(v3, mint);
}

function load(): PersistedV4 {
  const pool = resolvePool();
  const own = readJson(LAYOUT_KEY) as Partial<WindowLayout> | null;
  if (own && typeof own === "object" && own.layout) {
    // This window has its own saved layout — the pool comes from the shared key,
    // the arrangement from the scoped one.
    pool.layout = parseLayout(own.layout);
    pool.activeSession = own.activeSession ?? null;
  } else if (!isPrimaryWindow()) {
    // An extra window upgrading from v3 keeps the session it had in front of
    // it. Otherwise a fresh extra window opens as a **duplicate** of the
    // primary's layout (the shared blob's) and diverges as it is rearranged —
    // rmux's daemon multiplexes several clients per session, so two *windows*
    // showing one session is safe; two panes in one window is not.
    const old = readJson(LAYOUT_V3_KEY) as { activeSession?: string | null } | null;
    if (old && typeof old === "object" && old.activeSession) {
      pool.activeSession = old.activeSession;
      pool.layout = migrateLayoutV3(pool, mint);
    }
  }
  pool.layout = normalizeLayout(pool.layout, pool, mint);
  return pool;
}

/** Snapshot the persisted subset. The old v3/v2 blobs are left in place as a
 *  downgrade fallback — v4 wins on load, so they are inert but not lost. */
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

  // The layout is this window's own — always written to the scoped key.
  try {
    const own: WindowLayout = { version: 4, layout: state.layout, activeSession: state.activeSession };
    storage.setItem(LAYOUT_KEY, JSON.stringify(own));
  } catch {
    // A full or disabled localStorage must never stop the app working.
  }

  // The pool is shared. The primary window owns the layout/activeSession still
  // carried in the shared blob (to seed a first extra window); an extra window
  // must **not** stamp its own view there, or it would overwrite the layout the
  // primary saved. So an extra window preserves what the shared blob holds.
  let poolLayout = state.layout;
  let poolActive = state.activeSession;
  if (!isPrimaryWindow()) {
    const prev = readJson(V4_KEY) as Partial<PersistedV4> | null;
    poolLayout = parseLayout(prev?.layout);
    poolActive = prev?.activeSession ?? null;
  }

  const snapshot: PersistedV4 = {
    version: 4,
    servers: state.servers,
    projects: state.projects,
    sessions: state.sessions,
    layout: poolLayout,
    activeSession: poolActive,
    openPaths,
    activePath,
  };
  try {
    storage.setItem(V4_KEY, JSON.stringify(snapshot));
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
 * terminal id must carry its `term-` prefix, because it is used verbatim. A pi
 * id is bare like Claude's — `reattachName` adds the `pi-` prefix — so the stem
 * is `pisession` (recognizable in logs, but crucially **not** starting with
 * `pi-`, or reattach would produce `pi-pi-…` and the daemon's prefix classifier
 * would see a malformed name).
 */
const mintSessionId = (kind: SessionKind): string =>
  `${mintStem(kind)}-${Date.now().toString(36)}-${++seq}`;


/** Layout node ids. Unlike session ids nothing on a host is named after these,
 *  so a clock plus a counter is all the uniqueness they need. */
let layoutSeq = 0;
function mint(prefix: "tab" | "leaf" | "split" | "tile"): string {
  return `${prefix}-${Date.now().toString(36)}-${++layoutSeq}`;
}

const restored = load();

/** "Is this session another child of `parentId`?" — what marks an overflow tab. */
const siblingOf =
  (sessions: readonly Pick<SessionV3, "id" | "parentId">[], parentId: string) =>
  (id: string): boolean =>
    sessions.some((x) => x.id === id && x.parentId === parentId);

/**
 * Would moving `node` into `projectId` cross servers? Every pane being moved
 * must already be on the target's server: a session runs on one host, and a
 * project is a folder on one host.
 */
function crossesServer(
  s: Pick<State, "projects" | "sessions">,
  node: SplitNode,
  projectId: ProjectId,
): boolean {
  const serverOfProject = (pid: ProjectId) => s.projects.find((p) => p.id === pid)?.serverId;
  const to = serverOfProject(projectId);
  for (const l of treeLeaves(node)) {
    const pane = l.pane;
    const from =
      pane.kind === "session"
        ? serverOfProject(s.sessions.find((x) => x.id === pane.id)?.projectId ?? "")
        : pane.kind === "host"
          ? pane.serverId
          : serverOfProject(pane.projectId);
    if (!to || from !== to) return true;
  }
  return false;
}

/** The pane area's width / height — a new tile goes the way the focused tile
 *  is long on screen, which a unit-square tree cannot know by itself. */
function paneAreaAspect(): number {
  if (typeof document === "undefined") return 16 / 9;
  const r = document.querySelector(".pane-area")?.getBoundingClientRect();
  return r && r.width > 0 && r.height > 0 ? r.width / r.height : 16 / 9;
}

/**
 * The size, in CSS pixels, a tab of `projectId` is drawn at: its tab body when
 * one is on screen, else its tile less the tab bar, else the pane area less
 * one. `null` when nothing measures (a harness, a hidden window) — callers then
 * fall back to counting panes. Exported for the child watcher.
 */
export function tabAreaOf(projectId: ProjectId | null | undefined): Area | null {
  if (typeof document === "undefined") return null;
  const ok = (r: DOMRect | undefined) => (r && r.width > 0 && r.height > 0 ? r : undefined);
  const shown = projectId
    ? Array.from(document.querySelectorAll<HTMLElement>(".split-tab")).find(
        (el) => el.dataset.tabProject === projectId && el.style.display !== "none",
      )
    : undefined;
  const body = ok(shown?.getBoundingClientRect());
  if (body) return { w: body.width, h: body.height };
  const bar = 22; // --tile-bar-h
  const tile = projectId
    ? Array.from(document.querySelectorAll<HTMLElement>(".project-tile")).find((el) => el.dataset.tileProject === projectId)
    : undefined;
  const r = ok(tile?.getBoundingClientRect()) ?? ok(document.querySelector(".pane-area")?.getBoundingClientRect());
  return r && r.height > bar ? { w: r.width, h: r.height - bar } : null;
}

/** Just the reducer core, for handing to a reducer and merging its result back. */
const coreOf = (s: State): Core => ({
  servers: s.servers,
  projects: s.projects,
  sessions: s.sessions,
  activeSession: s.activeSession,
});

/**
 * Give a session's terminal the keyboard — now, and again once the pane has had
 * a chance to mount. Switching to a tab that was not warm mounts its panes
 * *after* this store update renders, so a single event would reach no listener
 * and the cursor would stay in the pane just left: the "screen says one thing,
 * keys do another" failure. A repeat to an already-focused terminal is a no-op.
 */
function focusKeyboard(sessionId: string) {
  if (typeof window === "undefined") return;
  requestTerminalFocus(sessionId);
  requestAnimationFrame(() => requestAnimationFrame(() => requestTerminalFocus(sessionId)));
}

/** The profile an adopted child records — see `adoptChild`. */
function childProfile(
  parent: { kind: string; modelProfile?: string; activeProfile?: string },
  kind: string,
  child: { profile?: string; chain?: string },
): { modelProfile?: string; activeProfile?: string } {
  if (parent.kind === kind && parent.modelProfile) {
    return { modelProfile: parent.modelProfile, activeProfile: parent.activeProfile };
  }
  if (child.chain) return { modelProfile: child.chain, activeProfile: child.profile };
  if (child.profile && child.profile !== "default" && child.profile !== "none") {
    return { modelProfile: child.profile };
  }
  return {};
}

export const useWorkspace = create<State>((set, get) => {
  /**
   * Commit a layout change and everything that follows from what is now in
   * front of the operator: the focused session becomes the active one (the
   * instruments, notifications and control socket all read `activeSession`),
   * looking at it acknowledges it up to its latest status change, and its tab
   * goes to the front of the warm set. `keyboard` also hands it the cursor.
   */
  const land = (next: Layout, opts: { keyboard?: boolean } = {}, extra: Partial<State> = {}) => {
    const s = get();
    const patch: Partial<State> = { ...extra, layout: next };
    const at = layoutActiveTab(next);
    if (at && s.recentTabs[0] !== at.tab.id) {
      patch.recentTabs = [at.tab.id, ...s.recentTabs.filter((id) => id !== at.tab.id)].slice(0, 12);
    }
    const leaf = focusedLeaf(next);
    if (leaf?.pane.kind === "session") {
      const id = leaf.pane.id;
      patch.activeSession = id;
      const mark = s.runtime[id]?.lastStatusAt ?? Date.now();
      if ((s.seen[id] ?? 0) < mark) {
        const seen = { ...s.seen, [id]: mark };
        persistSeen(seen);
        patch.seen = seen;
      }
    }
    set(patch);
    schedulePersist(get);
    if (opts.keyboard && leaf?.pane.kind === "session") focusKeyboard(leaf.pane.id);
  };

  /** Re-run the invariant after a pool change (normalize is a no-op — same
   *  reference — when nothing needed repair). */
  const renormalize = () => {
    const s = get();
    const next = normalizeLayout(s.layout, s, mint);
    if (next !== s.layout) set({ layout: next });
  };

  /**
   * Commit a structural move (`moveBeside`, `moveToTab`, `moveToNewTab`).
   * Same server, other project: the sessions now belong to the target's
   * project — changed in the same write as the layout, so `normalizeLayout`
   * never sees a session placed outside its own project (it would pull it
   * straight back out). The source project stays even if this emptied it: the
   * operator opened it. Modelled on `rehomeChildren`, minus the removal.
   *
   * The moved pane takes the focus and the keyboard — the operator just put it
   * there on purpose. **Nothing ends**: no `removeSession`, no close.
   */
  const landMove = (r: { layout: Layout; sessions: string[]; fromProject: ProjectId }, to: ProjectId) => {
    const s = get();
    const moving = new Set(r.sessions);
    const extra: Partial<State> = {};
    if (r.fromProject !== to && moving.size) {
      extra.sessions = s.sessions.map((x) => (moving.has(x.id) ? { ...x, projectId: to } : x));
    }
    land(r.layout, { keyboard: true }, extra);
    renormalize();
  };

  /** Create a session in `projectId` without placing it — `addSession` and
   *  `splitPane` each place it their own way. */
  const createSession = (
    projectId: ProjectId,
    kind: SessionKind,
    opts?: Parameters<State["addSession"]>[2],
  ): string => {
    const id = mintSessionId(kind);
    const project = get().projects.find((p) => p.id === projectId);
    // A pi session is named after its project like Claude (not "shell"); only a
    // plain terminal defaults to "shell".
    const name = opts?.name?.trim() || (kind === "terminal" ? "shell" : (project?.label ?? kind));
    // Claude renders fullscreen by default (the native TUI); the per-session
    // INLINE toggle overrides it and persists with the session. Seeded here so a
    // brand-new session carries an explicit value while existing persisted
    // sessions keep whatever they already have. `...opts` still wins if a caller
    // passes `fullscreen` explicitly.
    const defaults = kind === "claude" ? { fullscreen: true } : {};
    // `opts` first, then the authoritative fields — so a computed `name` and the
    // real id/kind/projectId win over anything the caller passed loosely.
    const ws = rAddSession(coreOf(get()), { ...defaults, ...opts, id, projectId, kind, name });
    // A dsh session starts with *no* runtime, so the rail reads "unknown" until
    // its server answers — seeding idle would claim a measurement nobody made.
    set({ ...ws, runtime: kind === "dsh" ? get().runtime : { ...get().runtime, [id]: { ...IDLE } } });
    return id;
  };

  /** Where a host or files pane should go: focus an existing one in `pid`, else
   *  open it as a new tab there. */
  const openPaneIn = (pid: ProjectId, pane: PaneRef, name: string) => {
    const layout = get().layout;
    const same = (p: PaneRef) =>
      p.kind === pane.kind &&
      (p.kind === "host"
        ? pane.kind === "host" && p.serverId === pane.serverId
        : p.kind === "files" && pane.kind === "files" && p.projectId === pane.projectId);
    const hit = locatePane({ activeProject: pid, projects: { [pid]: tabsOf(layout, pid) } }, same);
    if (hit) return land(lFocusLeaf(layout, pid, hit.tabId, hit.leafId));
    land(addTab(layout, pid, pane, name, mint).layout);
  };

  return {
  servers: restored.servers,
  projects: restored.projects,
  sessions: restored.sessions,
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

  layout: restored.layout,
  recentTabs: [],
  savedViews: loadSavedViews(),
  railCollapsed: storage?.getItem(RAIL_COLLAPSED_KEY) === "1",
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

  setServerIdentity: (id, identity) => {
    set(rSetServerIdentity(coreOf(get()), id, identity));
    schedulePersist(get);
  },

  createProject: (server, folder, label) => {
    const { ws, id } = rAddProject(coreOf(get()), server, folder, label);
    set(ws);
    schedulePersist(get);
    return id;
  },

  /**
   * Create a session **and place it**: a new tab in its project, focused.
   *
   * Placing it here rather than at each caller is the point — the rail's
   * [+]/[✦], quick-add, the new-session dialogs, adoption and the resume
   * pickers all come through this, and a session created without a tab is one
   * the pane area cannot reach.
   */
  addSession: (projectId, kind, opts) => {
    const id = createSession(projectId, kind, opts);
    const name = get().sessions.find((x) => x.id === id)?.name ?? kind;
    land(addTab(get().layout, projectId, { kind: "session", id }, name, mint).layout);
    return id;
  },

  removeSession: (id) => {
    const state = get();
    const session = state.sessions.find((s) => s.id === id);
    if (session && hasBackend()) {
      // The kill is the point — the shell/Claude runs under the agent and would
      // otherwise leak on the host. Reattach name and target are resolved the
      // same way an attach does, so the right session on the right host dies.
      const target = state.targetOf(id);
      const name = reattachName(session);
      // Codex is hosted exactly like Claude — a Claude-store attach under the
      // agent, with a companion shell — so it ends exactly like Claude.
      if (session.kind === "claude" || session.kind === "codex") {
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
        if (session.kind === "dsh") {
          // The view half lives on this machine only: the child webview and the
          // loopback forward. The server itself dies with the agent kill below.
          void invoke("dsh_view_close", { sessionId: id }).catch(() => {});
          const pty = state.live[id];
          if (pty) void api.claudeStop(pty).catch(() => {});
        }
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
    // Its pane goes with it: the split collapses, an emptied tab is removed.
    land(removeSessionLeaves(get().layout, id));
  },

  /**
   * Stop watching a session without ending it.
   *
   * The twin of `removeSession`, and the whole difference is what is *not* sent:
   * no `claude_end_session`, no `terminal_close`. Both of those carry an
   * `agent kill` to the host. Detaching drops only this machine's view — the
   * shell and the conversation keep running under `rmux-agent`, which is the
   * reason the agent exists.
   *
   * The structural half is identical, so it reuses `removeSession`'s reducer
   * rather than a copy of it: the row goes, its panes clear, and the active
   * session hands off. A second reducer with the same body would be two things
   * to keep in step forever, and they would not stay in step.
   *
   * **The row disappearing is the deliberate part.** The alternative — leaving
   * it dimmed in the rail — was the original design on the branch this comes
   * from, and that branch abandoned it. A rail that accumulates rows for every
   * session anyone ever detached, on any machine, stops answering the question
   * it exists to answer. The way back is HOST ▸ SESSIONS, which lists what is
   * genuinely running rather than what this machine happens to remember.
   *
   * **The companion shell must be detached too.** It is a real terminal under
   * the agent and it is not in the rail, so nothing else would ever reach it —
   * the unreachable leak `rmux-agent list` exists to make findable. But its
   * preferences are *kept* (no `forgetCompanion`): re-attaching should restore
   * the split the operator was working in, and forgetting would silently reset
   * their layout as a side effect of stepping away.
   */
  detachSession: (id) => {
    const state = get();
    const session = state.sessions.find((s) => s.id === id);
    if (session && hasBackend()) {
      // These take the **live handle**, not the session id: they are dropping a
      // local PTY, not naming something on the host. There is deliberately no
      // target and no session name in either call — a detach has nothing to say
      // to the far side, and no path that could end a shell.
      const pty = state.live[id];
      if (pty) {
        // A dsh console is a Claude-store attach, like Claude's.
        if (session.kind === "claude" || session.kind === "dsh" || session.kind === "codex")
          void api.claudeStop(pty).catch(() => {});
        else void api.terminalDetach(pty).catch(() => {});
      }
      // Detaching stops *looking*: the dsh view and its forward go, the server
      // keeps running under the agent.
      if (session.kind === "dsh") void invoke("dsh_view_close", { sessionId: id }).catch(() => {});
      const companion = state.live[companionKey(id)];
      if (companion) void api.terminalDetach(companion).catch(() => {});
    }

    const ws = rRemoveSession(coreOf(state), id);
    const { [id]: _r, ...runtime } = state.runtime;
    const { [id]: _l, [companionKey(id)]: _c, ...live } = state.live;
    // Pruned for the same quota reason as `removeSession`: the row is gone, and
    // a watermark for a session no longer listed would accumulate for every
    // detach anyone ever makes.
    const { [id]: _s, ...seen } = state.seen;
    if (Object.keys(seen).length !== Object.keys(state.seen).length) persistSeen(seen);
    set({ ...ws, runtime, live, seen });
    // The pane closes like any other — structurally only. Nothing here kills.
    land(removeSessionLeaves(get().layout, id));
  },

  adoptServerSession: (serverId, hostName, kind, alias, cwd) => {
    // **Already here? Open it, do not adopt it again.** Adopting twice would put
    // two rows in front of one shell, and the second would attach to the same
    // PTY the first is already showing.
    const existing = get().sessions.find((s) => reattachName(s) === hostName);
    if (existing) {
      get().openSession(existing.id);
      return existing.id;
    }

    // **Where does an adopted session belong in the tree?**
    //
    // In the project for the folder it is actually working in. The host reports
    // that (`AgentSession.cwd`, read from procfs), so an adopted session joins
    // the project you already have for that directory — or creates it — and its
    // files pane, git pane and future terminals all point somewhere real.
    //
    // The first version of this dropped every adopted session into a synthetic
    // bucket whose folder was a sentinel. Claude resumed correctly and the files
    // pane then said **"cannot open directory"**, because the pane was faithfully
    // trying to list a folder named `(running)`. Reported with a screenshot.
    //
    // The bucket survives as the fallback for when the directory genuinely
    // cannot be determined — a host without procfs, or a process owned by
    // someone else. Its folder is a sentinel that no real project can have
    // (every real one is created from an absolute path), so the project id
    // derived from it dedupes the bucket for free.
    const real = cwd?.trim();
    const projectId = real
      ? get().createProject(serverId, real)
      : get().createProject(serverId, RUNNING_FOLDER, "running");
    if (!real) {
      set((s) => ({
        projects: s.projects.map((p) => (p.id === projectId ? { ...p, running: true } : p)),
      }));
    }

    // The row is named by its alias when the host has one — that is the name a
    // person chose, possibly on another machine — and by the raw session name
    // otherwise. `renamed` latches so a Claude title cannot overwrite it.
    const label = alias?.trim();
    // `addSession` places it — a new tab in its project, focused.
    const id = get().addSession(projectId, kind, {
      name: label || hostName,
      renamed: !!label,
      hostName,
    });
    get().activate(id);
    return id;
  },

  adoptChild: (serverId, decision, aspect, area) => {
    const state = get();
    const { child } = decision;
    const onServer = new Set(state.projects.filter((p) => p.serverId === serverId).map((p) => p.id));
    // Already here (a race with a manual adopt, or another window's write) —
    // two rows on one daemon session would mount it twice.
    const existing = state.sessions.find((x) => onServer.has(x.projectId) && reattachName(x) === child.session);
    if (existing) return existing.id;
    const parent = state.sessions.find((x) => x.id === decision.parentId);
    if (!parent) return null;

    let core = coreOf(state);
    let projectId = decision.projectId;
    if (decision.create) {
      const made = rAddProject(core, serverId, decision.create.folder);
      core = made.ws;
      projectId = made.id;
    }
    if (!core.projects.some((p) => p.id === projectId)) return null;

    const id = mintSessionId(decision.kind);
    const keepActive = core.activeSession;
    core = rAddSession(core, {
      id,
      projectId,
      kind: decision.kind,
      name: decision.name,
      // Named by whoever spawned it — a Claude title must not overwrite that.
      renamed: true,
      // Attach by the host's name, verbatim: deriving one would name a session
      // that does not exist, and attaching to it would *create* a second one.
      hostName: child.session,
      parentId: parent.id,
      ...(decision.kind === "claude" ? { fullscreen: true } : {}),
      // The host bound the child to its parent's credential set (CLAUDE.md,
      // children inherit), so the adopted row names the same profile — a
      // restart from the UI must not move it to another provider. A child of a
      // different kind took that kind's default on the host; the record names
      // the profile id when it is a real one.
      ...childProfile(parent, decision.kind, child),
    });
    // `rAddSession` makes the new session active; adoption must not.
    core = { ...core, activeSession: keepActive };

    // Beside the parent while that still leaves a usable pane; past that, the
    // parent's overflow tab (`layout-tree.ts` `childPlacement`). Never focused.
    const placed = placeChild(state.layout, projectId, parent.id, { kind: "session", id }, mint, {
      area,
      aspect,
      isSibling: siblingOf(core.sessions, parent.id),
      parentName: parent.name,
      childName: decision.name,
    });
    // Pool and layout in one write, so nothing ever renders the session unplaced.
    set({ ...core, layout: placed.layout, runtime: { ...state.runtime, [id]: { ...IDLE } } });
    schedulePersist(get);
    return id;
  },

  rehomeChildren: (aspect, area) => {
    const state = get();
    let layout = state.layout;
    let sessions = state.sessions;
    const moved: string[] = [];
    const left = new Set<ProjectId>();
    for (const child of state.sessions) {
      if (!child.parentId) continue;
      const parent = sessions.find((x) => x.id === child.parentId);
      if (!parent || parent.projectId === child.projectId) continue;
      const home = state.projects.find((p) => p.id === parent.projectId);
      const away = state.projects.find((p) => p.id === child.projectId);
      // A parent in the adopted-sessions bucket places children by folder on
      // purpose (see `chooseProject`); and never move work across servers.
      if (!home || home.running || !away || away.serverId !== home.serverId) continue;
      layout = removeSessionLeaves(layout, child.id);
      layout = placeChild(layout, home.id, parent.id, { kind: "session", id: child.id }, mint, {
        area,
        aspect,
        isSibling: siblingOf(sessions, parent.id),
        parentName: parent.name,
        childName: child.name,
      }).layout;
      sessions = sessions.map((x) => (x.id === child.id ? { ...x, projectId: home.id } : x));
      left.add(away.id);
      moved.push(child.id);
    }
    if (!moved.length) return moved;
    // Only a project this move emptied, and only when nothing else of it is
    // open (no session, no file buffer) — it existed solely to hold the child.
    const emptied = new Set(
      [...left].filter(
        (pid) => !sessions.some((x) => x.projectId === pid) && !Object.values(state.buffers).some((b) => b.projectId === pid),
      ),
    );
    let activeProject = layout.activeProject;
    if (activeProject && emptied.has(activeProject)) {
      const firstMoved = state.sessions.find((x) => x.id === moved[0]);
      activeProject = sessions.find((x) => x.id === firstMoved?.parentId)?.projectId ?? activeProject;
    }
    set({
      sessions,
      projects: state.projects.filter((p) => !emptied.has(p.id)),
      layout: { ...layout, activeProject },
    });
    renormalize();
    schedulePersist(get);
    return moved;
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
    // Its tabs (and any files pane of it elsewhere) go with it.
    renormalize();
    schedulePersist(get);
  },

  removeServer: (id) => {
    for (const p of get().projects.filter((x) => x.serverId === id)) get().removeProject(p.id);
    set((s) => ({ servers: s.servers.filter((sv) => sv.id !== id) }));
    renormalize();
    schedulePersist(get);
  },

  /**
   * Rename a session here, and — where it means anything — on the host too.
   *
   * A rail row's name is local to this machine, which is fine until the same
   * host is opened from a second computer and everything reads `term-01K9…`
   * again. So a **terminal** also gets a host-side alias, written to a file the
   * agent reads: `rmux-agent list` then reports the chosen name, and so does
   * every other client.
   *
   * Claude sessions are deliberately local-only. Their rail name is frequently
   * the conversation's own title, adopted automatically, and pushing every one
   * of those to the host would fill the alias file with names nobody chose.
   *
   * The local rename is applied **first and unconditionally**. The host half is
   * a nicety, and a host that is unreachable must not stop someone renaming a
   * row in their own sidebar.
   */
  renameSession: (id, name) => {
    const clean = name.trim();
    if (!clean) return;
    const session = get().sessions.find((x) => x.id === id);
    if (!session || session.name === clean) return;

    set((s) => ({
      sessions: s.sessions.map((x) =>
        // `renamed` latches: Claude's own title is ignored from here on.
        x.id === id ? { ...x, name: clean, renamed: true } : x,
      ),
    }));
    schedulePersist(get);

    if (session.kind !== "terminal" || !hasBackend()) return;
    const target = get().targetOf(id);
    void api
      .serverAlias(target, reattachName(session), clean)
      .then(() => get().setSessionError(id, null))
      // Surfaced rather than swallowed, because the row now shows a name the
      // host does not know — and the next machine to look will disagree with
      // this one. The local rename stands; this says the other half did not.
      .catch((e) => get().setSessionError(id, e instanceof Error ? e.message : String(e)));
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

  setLowLatency: (id, lowLatency) => {
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, lowLatency } : x)) }));
    schedulePersist(get);
  },
  clearLowLatency: (id) => {
    set((s) => ({
      sessions: s.sessions.map((x) => {
        if (x.id !== id) return x;
        const { lowLatency: _drop, ...rest } = x;
        return rest;
      }),
    }));
    schedulePersist(get);
  },

  setFullscreen: (id, fullscreen) => {
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, fullscreen } : x)) }));
    schedulePersist(get);
  },
  setModelProfile: (id, modelProfile) => {
    // A new choice starts a chain at its first member again.
    set((s) => ({
      sessions: s.sessions.map((x) => (x.id === id ? { ...x, modelProfile, activeProfile: undefined } : x)),
    }));
    schedulePersist(get);
  },
  setActiveProfile: (id, activeProfile) => {
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, activeProfile } : x)) }));
    schedulePersist(get);
  },
  setCodexLaunch: (id, launch) => {
    set((s) => ({
      sessions: s.sessions.map((x) =>
        x.id === id
          ? {
              ...x,
              ...(launch.bypassApprovals === undefined
                ? {}
                : { bypassApprovals: launch.bypassApprovals || undefined }),
              // `null` clears back to Codex's own default; undefined leaves alone.
              ...(launch.codexSandbox === undefined
                ? {}
                : { codexSandbox: launch.codexSandbox ?? undefined }),
            }
          : x,
      ),
    }));
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

      // A session with no runtime yet (a starting dsh) gets one on its first
      // report even when that report is the default `idle`.
      if (s.runtime[id] && cur.status === status && lastStatusAt === cur.lastStatusAt) return s;

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

  activate: (id, opts) => {
    let layout = get().layout;
    let loc = locateSession(layout, id);
    if (!loc) {
      // Normalize guarantees a place; this is the backstop if a caller races it.
      layout = normalizeLayout(layout, get(), mint);
      loc = locateSession(layout, id);
    }
    if (!loc) {
      // Not a session we hold (an unknown id) — nothing to bring forward.
      return;
    }
    land(lFocusLeaf(layout, loc.projectId, loc.tabId, loc.leafId), {
      keyboard: opts?.keyboard ?? true,
    });
  },

  // ── projects / tabs / panes ──────────────────────────────────────────────
  selectProject: (projectId) => {
    if (!get().projects.some((p) => p.id === projectId)) return;
    land(lSelectProject(get().layout, projectId), { keyboard: true });
  },
  selectTab: (projectId, tabId, opts) =>
    land(lSelectTab(get().layout, projectId, tabId), { keyboard: opts?.keyboard ?? true }),
  selectTabIndex: (index) => land(lSelectTabIndex(get().layout, index), { keyboard: true }),
  cycleTab: (delta) => land(lCycleTab(get().layout, delta), { keyboard: true }),

  newTab: (projectId) => {
    const pid = projectId ?? get().layout.activeProject;
    if (!pid || !get().projects.some((p) => p.id === pid)) return null;
    const id = get().addSession(pid, "terminal");
    focusKeyboard(id);
    return id;
  },

  renameTab: (projectId, tabId, name) => {
    set({ layout: lRenameTab(get().layout, projectId, tabId, name) });
    schedulePersist(get);
  },

  closeTab: (projectId, tabId) => {
    const tab = tabsOf(get().layout, projectId).tabs.find((t) => t.id === tabId);
    if (!tab) return;
    // Each session ends exactly as the rail's ✕ ends it — `removeSession` sends
    // the kill and drops its leaf; the tab goes when its last pane does.
    for (const sid of sessionsInTab(tab)) get().removeSession(sid);
    land(removeTab(get().layout, projectId, tabId), { keyboard: true });
  },

  moveTab: (projectId, from, to) => {
    set({ layout: lMoveTab(get().layout, projectId, from, to) });
    schedulePersist(get);
  },

  moveRefusal: (source, target) => {
    const s = get();
    if (!canMoveBeside(s.layout, source, target)) return "noop";
    return crossesServer(s, resolveMoveSource(s.layout, source)!.node, target.projectId) ? "server" : null;
  },

  tabMoveRefusal: (source, projectId, tabId) => {
    const s = get();
    const src = resolveMoveSource(s.layout, source);
    if (!src) return "noop";
    if (tabId === null ? src.wholeTab && src.projectId === projectId : src.projectId === projectId && src.tabId === tabId) {
      return "noop";
    }
    if (tabId !== null && !tabsOf(s.layout, projectId).tabs.some((t) => t.id === tabId)) return "noop";
    return crossesServer(s, src.node, projectId) ? "server" : null;
  },

  moveToTab: (source, projectId, tabId) => {
    if (get().tabMoveRefusal(source, projectId, tabId)) return false;
    const r = lMoveToTab(get().layout, source, { projectId, tabId }, mint, tabAreaOf(projectId));
    if (!r) return false;
    landMove(r, projectId);
    return true;
  },

  moveToNewTab: (source, projectId) => {
    if (get().tabMoveRefusal(source, projectId, null)) return false;
    const r = lMoveToNewTab(get().layout, source, projectId, mint);
    if (!r) return false;
    landMove(r, projectId);
    return true;
  },

  moveBeside: (source, target, side) => {
    if (get().moveRefusal(source, target)) return false;
    const r = lMoveBeside(get().layout, source, target, side, mint);
    if (!r) return false;
    landMove(r, target.projectId);
    return true;
  },

  focusLeaf: (projectId, tabId, leafId, opts) =>
    land(lFocusLeaf(get().layout, projectId, tabId, leafId), { keyboard: opts?.keyboard ?? true }),

  focusDirection: (dir) => {
    // Crossing into an empty tile moves the focus with no pane to land on, so
    // the test is "did anything change", not "is there a leaf".
    const { layout } = focusInDirection(get().layout, dir);
    if (layout === get().layout) return;
    land(layout, { keyboard: true });
  },

  splitPane: (dir) => {
    const at = layoutActiveTab(get().layout);
    if (!at) return null;
    // A new terminal in the same project — what [+] and a new tab create.
    const id = createSession(at.projectId, "terminal");
    const split = splitFocused(get().layout, dir, { kind: "session", id }, mint);
    if (!split) {
      // Cannot happen with an active tab, but a session must never be left
      // unplaced: give it a tab rather than nothing.
      land(addTab(get().layout, at.projectId, { kind: "session", id }, "shell", mint).layout, { keyboard: true });
      return id;
    }
    land(split.layout, { keyboard: true });
    return id;
  },

  closeLeaf: (projectId, tabId, leafId) => {
    const tab = tabsOf(get().layout, projectId).tabs.find((t) => t.id === tabId);
    const node = tab ? findLeaf(tab.root, leafId) : null;
    if (!node) return;
    if (node.pane.kind === "session") {
      // Ends the session's work — the same call as the rail's ✕, which also
      // removes this leaf. Closing a view of work *is* ending it here; the way
      // to step away without killing is Detach, in the rail's menu.
      get().removeSession(node.pane.id);
      const heir = focusedLeaf(get().layout);
      if (heir?.pane.kind === "session") focusKeyboard(heir.pane.id);
      return;
    }
    land(lCloseLeaf(get().layout, projectId, tabId, leafId), { keyboard: true });
  },

  toggleZoom: () => land(lToggleZoom(get().layout)),
  swapPane: (dir) => land(swapInDirection(get().layout, dir), { keyboard: true }),

  setSplitRatio: (projectId, tabId, splitId, ratio) => {
    set({ layout: setTabRatio(get().layout, projectId, tabId, splitId, ratio) });
    schedulePersist(get);
  },

  openBeside: (projectId) => {
    if (!get().projects.some((p) => p.id === projectId)) return;
    land(lOpenBeside(get().layout, projectId, mint, paneAreaAspect()), { keyboard: true });
  },
  openNextBeside: () => {
    const s = get();
    const order = s.servers.flatMap((sv) => s.projects.filter((p) => p.serverId === sv.id).map((p) => p.id));
    const pid = nextUnshown(s.layout, order);
    if (!pid) return false;
    get().openBeside(pid);
    return true;
  },
  placeProject: (projectId, tileId, side) => {
    if (!get().projects.some((p) => p.id === projectId)) return;
    land(lPlaceTile(get().layout, projectId, tileId, side, mint), { keyboard: true });
  },
  closeTile: (tileId) => land(lCloseTile(get().layout, tileId), { keyboard: true }),
  setViewRatio: (splitId, ratio) => {
    set({ layout: lSetViewRatio(get().layout, splitId, ratio) });
    schedulePersist(get);
  },

  saveCurrentView: (name) => {
    const trimmed = name.trim();
    const at = layoutActiveTab(get().layout);
    if (!trimmed || !at) return;
    const { savedViews } = get();
    const view: SavedView = {
      // Re-saving under an existing name updates that view in place; otherwise a
      // fresh id keyed on the clock, which is unique enough for a personal list.
      id: savedViews.find((v) => v.name === trimmed)?.id ?? `view-${Date.now().toString(36)}`,
      name: trimmed,
      // A copy, not a live reference — a later edit must not rewrite a view the
      // operator already saved.
      root: JSON.parse(JSON.stringify(at.tab.root)) as SavedView["root"],
    };
    const next = [...savedViews.filter((v) => v.id !== view.id), view];
    persistSavedViews(next);
    set({ savedViews: next });
  },
  applyView: (id) => {
    const state = get();
    const view = state.savedViews.find((v) => v.id === id);
    if (!view) return;
    const sessionById = new Map(state.sessions.map((x) => [x.id, x]));
    // The view opens in the project its sessions belong to — a session may only
    // live in its own project's tabs — else in the active project.
    const firstSession = (function first(n: SavedView["root"]): string | null {
      if (n.kind === "leaf") return n.pane.kind === "session" && sessionById.has(n.pane.id) ? n.pane.id : null;
      return first(n.a) ?? first(n.b);
    })(view.root);
    const pid = (firstSession && sessionById.get(firstSession)?.projectId) || state.layout.activeProject;
    if (!pid) return;
    // A saved pane whose target no longer exists is skipped rather than pointing
    // at a session/server/project that was since removed.
    const alive = (p: PaneRef): boolean => {
      if (p.kind === "session") return sessionById.get(p.id)?.projectId === pid;
      if (p.kind === "host") return state.servers.some((s) => s.id === p.serverId);
      return state.projects.some((pr) => pr.id === p.projectId);
    };
    const root = cloneTree(view.root, alive, mint);
    if (!root) return;
    // Its sessions are *moved* here, never shown twice: two panes on one Claude
    // conversation both write to it.
    let layout = state.layout;
    const walk = (n: SavedView["root"]) => {
      if (n.kind === "leaf") {
        if (n.pane.kind === "session") layout = removeSessionLeaves(layout, n.pane.id);
        return;
      }
      walk(n.a);
      walk(n.b);
    };
    walk(root);
    land(addTreeTab(layout, pid, root, view.name, mint).layout, { keyboard: true });
  },
  deleteView: (id) => {
    const next = get().savedViews.filter((v) => v.id !== id);
    persistSavedViews(next);
    set({ savedViews: next });
  },

  openSession: (id) => get().activate(id),
  openHost: (serverId) => {
    const s = get();
    // A host pane goes in the active project when that project is on this
    // server; otherwise in the server's first project. A server with no
    // project yet borrows the active one — the pane is about the machine.
    const active = s.projects.find((p) => p.id === s.layout.activeProject);
    const pid =
      active?.serverId === serverId
        ? active.id
        : (s.projects.find((p) => p.serverId === serverId)?.id ?? active?.id);
    if (!pid) return;
    openPaneIn(pid, { kind: "host", serverId }, "host");
  },
  openFiles: (projectId) => {
    if (!get().projects.some((p) => p.id === projectId)) return;
    openPaneIn(projectId, { kind: "files", projectId }, "files");
  },

  // ── runtime handles ──────────────────────────────────────────────────────
  setLive: (id, handle) => set((s) => ({ live: { ...s.live, [id]: handle } })),
  clearLive: (id) =>
    set((s) => {
      const { [id]: _gone, ...live } = s.live;
      return { live };
    }),
  toggleRail: () =>
    set((s) => {
      const railCollapsed = !s.railCollapsed;
      try {
        storage?.setItem(RAIL_COLLAPSED_KEY, railCollapsed ? "1" : "0");
      } catch {
        // A full localStorage costs the preference, not the toggle.
      }
      return { railCollapsed };
    }),
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
      // Versioned: the token comes back with the bytes, so a later save can be
      // refused if the file changed on disk in between (see `buffers.ts`).
      const { content, version } = await api.fsReadVersioned(target, path);
      set((s) => {
        const b = s.buffers[key];
        if (!b) return s;
        const text = content.kind === "text" ? content.text : "";
        return { buffers: { ...s.buffers, [key]: { ...b, content, text, saved: text, loading: false, version } } };
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
    // The disk moved and the operator has not chosen yet — writing now would
    // decide for them. `keepMine` clears this and saves.
    if (buffer.disk) return;
    const target = get().targetOfProject(buffer.projectId);

    set((s) => ({ buffers: { ...s.buffers, [key]: { ...s.buffers[key]!, saving: true, saveError: null } } }));
    // Captured before the await: the user may keep typing during the write.
    const written = buffer.text;
    try {
      // Quotes the version it was read at: a file an agent rewrote since is
      // refused with a conflict, never silently overwritten.
      const version = await api.fsWriteVersioned(target, buffer.path, written, buffer.version ?? null);
      set((s) => {
        const cur = s.buffers[key];
        if (!cur) return s;
        return { buffers: { ...s.buffers, [key]: { ...cur, saved: written, saving: false, version } } };
      });
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      const conflict = message.startsWith(CONFLICT_PREFIX);
      set((s) => {
        const cur = s.buffers[key];
        if (!cur) return s;
        return {
          buffers: {
            ...s.buffers,
            [key]: conflict
              ? { ...cur, saving: false, disk: { kind: "conflict" } }
              : { ...cur, saving: false, saveError: message },
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
  };
});

/**
 * Keep every open window's **pool** in step.
 *
 * The pool (servers, projects, sessions, open files) is one shared key, so when
 * one window connects a server or starts a session, the others should see it —
 * and, just as importantly, a window that has been sitting open must not later
 * persist its *stale* pool over the newer one. A `storage` event fires only in
 * the windows that did **not** write it, so this merges the fresh pool in while
 * leaving this window's own layout (panes, active session, deck) alone.
 */
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key !== V4_KEY || e.newValue === null) return;
    let pool: Partial<PersistedV4>;
    try {
      pool = JSON.parse(e.newValue) as Partial<PersistedV4>;
    } catch {
      return;
    }
    const cur = useWorkspace.getState();
    const servers = pool.servers ?? cur.servers;
    const projects = pool.projects ?? cur.projects;
    const sessions = pool.sessions ?? cur.sessions;
    useWorkspace.setState({
      servers,
      projects,
      sessions,
      openPaths: pool.openPaths ?? cur.openPaths,
      activePath: pool.activePath ?? cur.activePath,
      // The layout and activeSession are this window's own — a rearrangement in
      // another window does not reach across and move these panes. But the
      // invariant is re-run against the new pool: a session another window
      // started gets a tab here (unfocused), and one it ended loses its pane.
      layout: normalizeLayout(cur.layout, { servers, projects, sessions }, mint),
    });
  });
}

// Keep the same derived-server-id helper reachable for callers that only have a
// target (the new-session wizard resolves a target → server before creating a
// project under it).
export { deriveServerId as serverId };

// Dev-only handle, mirroring `sessions.ts`, so the workbench can be driven from
// the console without the Tauri bridge.
if (import.meta.env.DEV) {
  (window as unknown as Record<string, unknown>).__rmuxWorkspace = useWorkspace;
}
