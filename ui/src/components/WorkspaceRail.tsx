import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { motion } from "motion/react";

import "../styles/sidebar.css";
import { useRailWidth } from "../lib/rail-width";
import { useProfileNames } from "../lib/fallback-watch";
import { useWorkspace } from "../lib/workspace";
import {
  leaves,
  locateSession,
  sessionsInTab,
  tabLabel,
  tabsOf,
  visibleLeaves,
  shownProjects,
  tileOf,
  type Leaf,
  type Tab,
} from "../lib/layout-tree";
import {
  GLYPHS,
  STATE_COLOR,
  STATE_LABEL,
  agentState,
  isAgentKind,
  needsYou,
  rollup,
  sortAgents,
  type AgentSort,
  type AgentState,
} from "../lib/agent-state";
import { useIndicatorStyle } from "../lib/agent-indicators";
import { ServerConnectionSettings } from "./ServerConnectionSettings";
import { isServerConnected, markDisconnected, useBranches, useBranchPoll } from "../lib/git-branch";
import { isMac, requestClose } from "../lib/shortcuts";
import { PROJECT_DRAG_EVENT, PROJECT_DRAG_TYPE } from "./WorkspaceDeck";
import { beginSessionDrag, endSessionDrag } from "../lib/session-drag";
import { MenuDivider, MenuItem, MenuSurface, type MenuAt } from "./Menu";
import { api, isChainId, type GitBranch } from "../lib/api";
import { RunningSessions } from "./RunningSessions";
import { PiSessionPicker } from "./PiSessionPicker";
import { CodexSessionPicker } from "./CodexSessionPicker";
import { RailGrip } from "./RailGrip";
import type { Project, Server, SessionV3 } from "../lib/workspace-model";
import { QuickAdd } from "./QuickAdd";

/**
 * The sidebar — herdr's anatomy, adapted to a multi-machine app.
 *
 * Top to bottom:
 *
 * 1. **spaces** — every project, numbered 1..N, grouped under the machine it
 *    lives on. Line one is `[state] label`, line two the git branch and its
 *    divergence. A ▸/▾ opens the project to its tabs and, inside each tab, its
 *    sessions — which is where every per-session action the old tree rail had
 *    still lives (rename, detach, the counted ✕ kill, the on-screen mark).
 * 2. a draggable divider (the split is remembered);
 * 3. **agents** — every Claude/pi session on every machine, `grouped` in the
 *    spaces order or by `priority` (most urgent, then most recent);
 * 4. a footer: ` new` and the `«` that folds this into
 * 5. the **compact column** — each space's number and rolled-up state, then
 *    each agent's state, click to go there.
 *
 * It still answers the one question the rail exists for — "which of my
 * machines needs me?" — through one mapping (`agent-state.ts`): blocked, done,
 * working, idle, unknown. The marks are **static**; nothing here spins.
 */

const RAIL_DEFAULT = 240;

// ── persisted view state ─────────────────────────────────────────────────────

function readSet(key: string): Set<string> {
  try {
    const raw = localStorage.getItem(key);
    return new Set(raw ? (JSON.parse(raw) as string[]) : []);
  } catch {
    return new Set();
  }
}
function writeSet(key: string, set: Set<string>) {
  try {
    localStorage.setItem(key, JSON.stringify([...set]));
  } catch {
    // A full localStorage must not stop the sidebar folding.
  }
}
function readString(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function writeString(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Preference only.
  }
}

/** Machines folded shut. Kept from the tree rail's key so a fold survives. */
const FOLD_KEY = "rmux.rail.folded.v3";
/** Spaces opened to show their tabs. Closed is the default — herdr's look. */
const EXPANDED_KEY = "rmux.sidebar.expanded";
const SPLIT_KEY = "rmux.sidebar.split";
const SORT_KEY = "rmux.sidebar.agentSort";
const SPLIT_MIN = 0.15;
const SPLIT_MAX = 0.85;

function useToggleSet(key: string) {
  const [set, setSet] = useState<Set<string>>(() => readSet(key));
  const toggle = useCallback(
    (id: string) =>
      setSet((prev) => {
        const next = new Set(prev);
        if (!next.delete(id)) next.add(id);
        writeSet(key, next);
        return next;
      }),
    [key],
  );
  return [set, toggle] as const;
}

// ── search ───────────────────────────────────────────────────────────────────

/**
 * Whether `name` matches a search: every word, anywhere, ignoring case.
 *
 * Words rather than one substring so "rs voice" finds `rs-voice` and the
 * order typed does not matter — a title is half-remembered, not quoted.
 */
export function matchesSearch(name: string, query: string): boolean {
  const haystack = name.toLowerCase();
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((word) => haystack.includes(word));
}

function serverLabel(server: Server): string {
  const t = server.target;
  if (!t.host) return "local";
  return `${t.user ? `${t.user}@` : ""}${t.host}${t.port ? `:${t.port}` : ""}`;
}

/** The short machine name an agent row leads with. */
function machineName(server: Server | undefined): string {
  return server?.target.host ?? "local";
}

/**
 * The surface of a session row: the active one filled with a 3px edge, the ones
 * on screen with a fainter 2px edge. Exported for `ui/rail-selection-check.ts`,
 * which measures the contrast of exactly these.
 */
export function rowSurface(active: boolean, onScreen: boolean) {
  return {
    background: active
      ? "color-mix(in srgb, var(--text) 14%, transparent)"
      : onScreen
        ? "color-mix(in srgb, var(--text) 3.5%, transparent)"
        : "transparent",
    boxShadow: active
      ? "inset 3px 0 0 var(--text)"
      : onScreen
        ? "inset 2px 0 0 var(--text-faint)"
        : "none",
  };
}

// ── marks ────────────────────────────────────────────────────────────────────

/** A state glyph in a fixed cell. `null` (a terminal, a group of terminals)
 *  keeps the cell, so labels stay in one column, and draws nothing. */
function StateMark({ state }: { state: AgentState | null }) {
  const style = useIndicatorStyle();
  if (!state) return <span className="sb-state" aria-hidden="true" />;
  return (
    <span className="sb-state" style={{ color: STATE_COLOR[state] }} title={STATE_LABEL[state]} aria-label={STATE_LABEL[state]}>
      {GLYPHS[style][state]}
    </span>
  );
}


// ── the derived model ────────────────────────────────────────────────────────

type SpaceModel = {
  project: Project;
  /** 1-based, over every space on every machine, independent of the search —
   *  a number that changes while you type is not something to remember. */
  number: number;
  state: AgentState | null;
  /** The sessions the search kept, or all of them; `null` = not searching. */
  visible: Set<string> | null;
};

type MachineModel = { server: Server; state: AgentState | null; spaces: SpaceModel[] };

type AgentRow = {
  session: SessionV3;
  state: AgentState;
  lastStatusAt?: number;
  where: string;
  /** The spawning session's name, for a child an agent started. */
  parentName?: string;
};

function useSidebarModel(query: string) {
  const servers = useWorkspace((s) => s.servers);
  const projects = useWorkspace((s) => s.projects);
  const sessions = useWorkspace((s) => s.sessions);
  const runtime = useWorkspace((s) => s.runtime);
  const seen = useWorkspace((s) => s.seen);
  const layout = useWorkspace((s) => s.layout);

  return useMemo(() => {
    const q = query.trim();
    const stateOf: Record<string, AgentState | null> = {};
    for (const s of sessions) stateOf[s.id] = agentState(s.kind, runtime[s.id], seen[s.id]);
    const nameOf = (id: string) => sessions.find((s) => s.id === id)?.name;

    const machines: MachineModel[] = [];
    const agents: AgentRow[] = [];
    let number = 0;

    for (const server of servers) {
      const machine = machineName(server);
      const spaces: SpaceModel[] = [];
      for (const project of projects) {
        if (project.serverId !== server.id) continue;
        number += 1;
        const mine = sessions.filter((s) => s.projectId === project.id);
        const state = rollup(mine.map((s) => stateOf[s.id]));

        let visible: Set<string> | null = null;
        if (q) {
          const selfMatch = matchesSearch(`${project.label} ${machine}`, q);
          visible = new Set(
            mine.filter((s) => selfMatch || matchesSearch(`${s.name} ${project.label} ${machine}`, q)).map((s) => s.id),
          );
          if (!selfMatch && visible.size === 0) continue;
        }
        spaces.push({ project, number, state, visible });

        for (const s of mine) {
          const st = stateOf[s.id];
          if (!st) continue;
          if (visible && !visible.has(s.id)) continue;
          const loc = locateSession(layout, s.id);
          const pt = loc ? tabsOf(layout, loc.projectId) : null;
          const ti = pt ? pt.tabs.findIndex((t) => t.id === loc!.tabId) : -1;
          const tab = ti >= 0 ? tabLabel(pt!.tabs[ti]!, ti, nameOf) : null;
          agents.push({
            session: s,
            state: st,
            lastStatusAt: runtime[s.id]?.lastStatusAt,
            where: [machine, project.label, tab].filter(Boolean).join(" · "),
            parentName: s.parentId ? nameOf(s.parentId) : undefined,
          });
        }
      }
      if (q && spaces.length === 0) continue;
      machines.push({ server, state: rollup(spaces.map((sp) => sp.state)), spaces });
    }

    // Counted over everything, not only what the search shows: the header is
    // "how many need you", and a filter does not make one stop needing you.
    let total = 0;
    for (const s of sessions) if (needsYou(stateOf[s.id] ?? null)) total += 1;

    const onScreen = new Set(
      visibleLeaves(layout).flatMap((l) => (l.pane.kind === "session" ? [l.pane.id] : [])),
    );
    // The project grid: which projects have a tile, in tile order (1-based
    // for the mark). Only meaningful with more than one — a single tile is the
    // active project, which the row already says.
    const shown = shownProjects(layout);
    const tiles = new Map(shown.length > 1 ? shown.map((pid, i) => [pid, i + 1] as const) : []);
    return { machines, agents, stateOf, needing: total, onScreen, layout, tiles, searching: !!q };
  }, [servers, projects, sessions, runtime, seen, layout, query]);
}

type Model = ReturnType<typeof useSidebarModel>;

// ── one session (a tree row or an agents-list row) ───────────────────────────

function SessionItem({
  session,
  state,
  active,
  onScreen,
  where,
  parentName,
}: {
  session: SessionV3;
  state: AgentState | null;
  active: boolean;
  onScreen: boolean;
  /** Present for an agents-list row: the `machine · project · tab` line. */
  where?: string;
  /** Present for a child an agent spawned: who spawned it (`↳ parent`). */
  parentName?: string;
}) {
  const [confirming, setConfirming] = useState(false);
  const [editing, setEditing] = useState(false);
  const [menu, setMenu] = useState<MenuAt | null>(null);
  const rename = useWorkspace((s) => s.renameSession);
  const detach = useWorkspace((s) => s.detachSession);
  const removeSession = useWorkspace((s) => s.removeSession);
  const activate = useWorkspace((s) => s.activate);
  const error = useWorkspace((s) => s.runtime[session.id]?.error ?? null);
  const agent = isAgentKind(session.kind);
  const mark = agent ? <StateMark state={state} /> : <TerminalMark />;

  const input = (
    <input
      autoFocus
      defaultValue={session.name}
      aria-label="Session name"
      className="data inset min-w-0 flex-1 px-1 py-[1px] text-[12px] outline-none"
      style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
      onFocus={(e) => e.currentTarget.select()}
      onBlur={(e) => {
        rename(session.id, e.currentTarget.value);
        setEditing(false);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
        if (e.key === "Escape") {
          e.currentTarget.value = session.name;
          e.currentTarget.blur();
        }
      }}
    />
  );

  return (
    <div
      className="sb-row"
      data-session={session.id}
      data-active={active}
      data-on-screen={onScreen}
      style={rowSurface(active, onScreen)}
      // Drag onto a pane to split it with this session beside it — a move,
      // never a close (`session-drag.ts`, `moveBeside`).
      draggable={!editing && !confirming}
      onDragStart={(e) => beginSessionDrag(e.dataTransfer, { kind: "session", sessionId: session.id })}
      onDragEnd={() => endSessionDrag()}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      {menu && (
        <MenuSurface at={menu} onClose={() => setMenu(null)} minWidth={210}>
          <MenuItem
            label="Rename"
            onClick={() => {
              setMenu(null);
              setEditing(true);
            }}
            hint="double-click"
          />
          <MenuDivider />
          {/* Not destructive, so not red — the work keeps running. The hint
              says where it went, because a row vanishing without one reads as
              having closed the session. */}
          <MenuItem
            label="Detach"
            hint="→ HOST ▸ SESSIONS"
            onClick={() => {
              setMenu(null);
              detach(session.id);
            }}
          />
          <MenuItem
            label="Close session"
            destructive
            onClick={() => {
              // The menu closes so the row's own confirmation — which names
              // what ends, on the server — is not hidden underneath it.
              setMenu(null);
              setConfirming(true);
            }}
          />
        </MenuSurface>
      )}

      {where !== undefined ? (
        <div className="sb-agent">
          <button
            type="button"
            className="sb-where min-w-0 text-left"
            onClick={() => activate(session.id)}
            onDoubleClick={() => setEditing(true)}
            title={`${where}\n${session.name}\nDouble-click to rename`}
          >
            {mark}
            <span className="sb-label">{where}</span>
          </button>
          {editing ? (
            <div className="flex pl-[calc(1.1em+6px)]">{input}</div>
          ) : (
            <button type="button" className="sb-name text-left" onClick={() => activate(session.id)} onDoubleClick={() => setEditing(true)}>
              {session.name}
            </button>
          )}
          {parentName && (
            <span className="sb-parent" title={`Spawned by ${parentName}`}>
              ↳ {parentName}
            </span>
          )}
          <ProfileLine session={session} />
        </div>
      ) : editing ? (
        <div className="sb-row-main">
          {mark}
          {input}
        </div>
      ) : (
        <button
          type="button"
          className="sb-row-main"
          onClick={() => activate(session.id)}
          onDoubleClick={() => setEditing(true)}
          title={`${session.name}${onScreen ? " — on screen" : ""}${parentName ? `\nSpawned by ${parentName}` : ""}\nDouble-click to rename`}
        >
          {parentName && (
            <span className="sb-parent-mark" aria-label={`spawned by ${parentName}`}>
              ↳
            </span>
          )}
          {mark}
          <span className="sb-label">{session.name}</span>
        </button>
      )}

      {error && (
        <p className="data px-3 pb-1 text-[10px]" style={{ color: "var(--st-blocked)" }}>
          {error}
        </p>
      )}

      {confirming ? (
        <div className="sb-confirm">
          <span className="micro" style={{ color: "var(--text-soft)" }}>
            {session.kind === "claude"
              ? "ENDS THIS CLAUDE SESSION"
              : session.kind === "pi"
                ? "ENDS THIS PI SESSION"
                : session.kind === "dsh"
                  ? "ENDS THIS DEEPSEEK HARNESS"
                  : session.kind === "codex"
                    ? "ENDS THIS CODEX SESSION"
                    : "ENDS THIS SHELL"}{" "}
            ON THE SERVER
          </span>
          <div className="flex gap-2">
            <button type="button" className="chip" style={{ color: "var(--st-blocked)" }} onClick={() => removeSession(session.id)}>
              close it
            </button>
            <button type="button" className="chip" onClick={() => setConfirming(false)}>
              cancel
            </button>
          </div>
        </div>
      ) : (
        <button
          type="button"
          aria-label={`Close ${session.name}`}
          title="Close this session (asks first)"
          className="sb-x sb-act sb-hover"
          onClick={(e) => {
            e.stopPropagation();
            setConfirming(true);
          }}
        >
          <CloseIcon />
        </button>
      )}
    </div>
  );
}

// ── a tab inside an expanded space ───────────────────────────────────────────

function TabBlock({
  projectId,
  tab,
  index,
  model,
  activeTab,
  visible,
}: {
  projectId: string;
  tab: Tab;
  index: number;
  model: Model;
  activeTab: boolean;
  visible: Set<string> | null;
}) {
  const sessions = useWorkspace((s) => s.sessions);
  const active = useWorkspace((s) => s.activeSession);
  const selectTab = useWorkspace((s) => s.selectTab);
  const focusLeaf = useWorkspace((s) => s.focusLeaf);
  const renameTab = useWorkspace((s) => s.renameTab);
  const [menu, setMenu] = useState<MenuAt | null>(null);
  const [editing, setEditing] = useState(false);

  const ids = sessionsInTab(tab);
  if (visible && !ids.some((id) => visible.has(id))) return null;
  const nameOf = (id: string) => sessions.find((s) => s.id === id)?.name;
  const label = tabLabel(tab, index, nameOf);
  const state = rollup(ids.map((id) => model.stateOf[id]));

  const leafRow = (leaf: Leaf) => {
    if (leaf.pane.kind === "session") {
      const session = sessions.find((s) => s.id === (leaf.pane as { id: string }).id);
      if (!session || (visible && !visible.has(session.id))) return null;
      return (
        <SessionItem
          key={leaf.id}
          session={session}
          state={model.stateOf[session.id] ?? null}
          active={session.id === active}
          onScreen={model.onScreen.has(session.id)}
          parentName={session.parentId ? nameOf(session.parentId) : undefined}
        />
      );
    }
    if (visible) return null;
    return (
      <button
        key={leaf.id}
        type="button"
        className="sb-row-main"
        onClick={() => focusLeaf(projectId, tab.id, leaf.id)}
        title={leaf.pane.kind === "host" ? "Host: processes, ports, metrics" : "Files"}
      >
        <span className="sb-state" aria-hidden="true" />
        <span className="sb-label" style={{ color: "var(--overlay1, var(--text-soft))" }}>
          {leaf.pane.kind}
        </span>
      </button>
    );
  };

  return (
    <div>
      <div
        className="sb-tab"
        data-active={activeTab}
        onClick={() => selectTab(projectId, tab.id)}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY });
        }}
        title={`Tab ${index + 1}: ${label}`}
      >
        {menu && (
          <MenuSurface at={menu} onClose={() => setMenu(null)} minWidth={190}>
            <MenuItem
              label="Rename tab"
              onClick={() => {
                setMenu(null);
                setEditing(true);
              }}
            />
            {/* The same confirmation the tab bar's ✕ uses — it names what ends. */}
            <MenuItem
              label="Close tab…"
              destructive
              onClick={() => {
                setMenu(null);
                requestClose({ projectId, tabId: tab.id });
              }}
            />
          </MenuSurface>
        )}
        <StateMark state={state} />
        {editing ? (
          <input
            autoFocus
            defaultValue={label}
            aria-label="Tab name"
            className="data inset min-w-0 flex-1 px-1 text-[11px] outline-none"
            style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
            onClick={(e) => e.stopPropagation()}
            onFocus={(e) => e.currentTarget.select()}
            onBlur={(e) => {
              renameTab(projectId, tab.id, e.currentTarget.value);
              setEditing(false);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") {
                e.currentTarget.value = label;
                e.currentTarget.blur();
              }
            }}
          />
        ) : (
          <span className="sb-label">
            <span style={{ color: "var(--overlay0, var(--text-faint))" }}>{index + 1} </span>
            {label}
          </span>
        )}
      </div>
      <div className="ml-[10px]">{leaves(tab.root).map(leafRow)}</div>
    </div>
  );
}

// ── a space (project) ────────────────────────────────────────────────────────

function BranchLine({ branch }: { branch: GitBranch }) {
  return (
    <span className="sb-sub" title={branch.upstream ? `${branch.branch} → ${branch.upstream}` : `${branch.branch} (no upstream)`}>
      <span className="truncate">{branch.branch}</span>
      {branch.upstream && (
        <>
          <span>·</span>
          <span className="sb-up">↑{branch.ahead}</span>
          <span className="sb-down">↓{branch.behind}</span>
        </>
      )}
    </span>
  );
}

/**
 * "This project has a tile" — a small tile glyph with its tile number, drawn
 * only while there is more than one tile. The focused tile's is in the accent
 * (the row's own accent edge says the same), the others muted, so the sidebar
 * answers "which projects am I looking at" without counting panes.
 */
function TileMark({ n, focused }: { n: number; focused: boolean }) {
  return (
    <span className="sb-tile" data-focused={focused} title={`On screen — tile ${n}${focused ? " (focused)" : ""}`}>
      <svg width="9" height="9" viewBox="0 0 10 10" aria-hidden="true">
        <rect x="0.5" y="0.5" width="9" height="9" fill="none" stroke="currentColor" strokeWidth="1" />
        <rect x="2.5" y="2.5" width="5" height="5" fill={focused ? "currentColor" : "none"} stroke="currentColor" strokeWidth="1" />
      </svg>
      <span>{n}</span>
    </span>
  );
}

function SpaceRow({
  space,
  server,
  model,
  expanded,
  onToggle,
  branch,
}: {
  space: SpaceModel;
  server: Server;
  model: Model;
  expanded: boolean;
  onToggle: () => void;
  branch: GitBranch | null | undefined;
}) {
  const { project, number, state, visible } = space;
  const sessions = useWorkspace((s) => s.sessions);
  const openFiles = useWorkspace((s) => s.openFiles);
  const selectProject = useWorkspace((s) => s.selectProject);
  const openBeside = useWorkspace((s) => s.openBeside);
  const closeTile = useWorkspace((s) => s.closeTile);
  const openSession = useWorkspace((s) => s.openSession);
  const addSession = useWorkspace((s) => s.addSession);
  const removeProject = useWorkspace((s) => s.removeProject);
  const renameProject = useWorkspace((s) => s.renameProject);
  const target = useWorkspace((s) => s.targetOfProject(project.id));
  const [menu, setMenu] = useState<MenuAt | null>(null);
  const [editing, setEditing] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [adding, setAdding] = useState(false);
  const [addingPi, setAddingPi] = useState(false);
  const [addingCodex, setAddingCodex] = useState(false);

  const mine = useMemo(() => sessions.filter((s) => s.projectId === project.id), [sessions, project.id]);
  const selected = model.layout.activeProject === project.id;
  const tileNo = model.tiles.get(project.id);
  const tileId = tileNo ? tileOf(model.layout, project.id)?.id : undefined;
  // While searching, a space with matching sessions opens to show them: a hit
  // hidden inside a closed group is a search that says "found" and shows nothing.
  const open = expanded || (visible !== null && visible.size > 0);
  const pt = tabsOf(model.layout, project.id);
  const located = new Set(pt.tabs.flatMap(sessionsInTab));
  const orphans = mine.filter((s) => !located.has(s.id) && (!visible || visible.has(s.id)));

  const newTerminal = () => openSession(addSession(project.id, "terminal"));
  // No dialog: dsh has nothing to resume or choose at launch — its own UI
  // lists and resumes conversations. The pane says what it is doing.
  const newDsh = () => openSession(addSession(project.id, "dsh"));

  /**
   * ✦ and π **ask** before creating — skip-permissions, which provider, and
   * whether this is new work or yesterday's continued are the operator's calls.
   */
  const addClaude = (choice: { skipPermissions: boolean; modelProfile?: string; resume?: string; name?: string }) => {
    const id = addSession(project.id, "claude", {
      skipPermissions: choice.skipPermissions,
      modelProfile: choice.modelProfile,
      resume: choice.resume,
      name: choice.name?.trim() || undefined,
    });
    openSession(id);
    setAdding(false);
  };

  return (
    <div>
      <div
        className="sb-space"
        data-active={selected}
        role="button"
        tabIndex={0}
        data-shown={tileNo ? true : undefined}
        // ⌘-click (Ctrl-click off macOS) opens the project in a tile beside
        // the focused one; a plain click focuses its tile when it has one, and
        // otherwise shows it in the focused tile — the single-tile behaviour.
        onClick={(e) => ((isMac() ? e.metaKey : e.ctrlKey) ? openBeside(project.id) : selectProject(project.id))}
        onKeyDown={(e) => {
          if (e.key === "Enter" && e.target === e.currentTarget) selectProject(project.id);
        }}
        draggable={!editing}
        onDragStart={(e) => {
          e.dataTransfer.setData(PROJECT_DRAG_TYPE, project.id);
          e.dataTransfer.effectAllowed = "move";
          window.dispatchEvent(new CustomEvent(PROJECT_DRAG_EVENT, { detail: true }));
        }}
        onDragEnd={() => window.dispatchEvent(new CustomEvent(PROJECT_DRAG_EVENT, { detail: false }))}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY });
        }}
        title={project.running ? "Sessions adopted from this host" : `${project.folder}\non ${serverLabel(server)}`}
      >
        {menu && (
          <MenuSurface at={menu} onClose={() => setMenu(null)} minWidth={220}>
            <MenuItem label="New terminal" onClick={() => (setMenu(null), newTerminal())} />
            <MenuItem label="New Claude session…" onClick={() => (setMenu(null), setAdding(true))} />
            <MenuItem label="New pi session…" onClick={() => (setMenu(null), setAddingPi(true))} />
            <MenuItem label="New DeepSeek Harness" onClick={() => (setMenu(null), newDsh())} />
            <MenuItem label="New Codex session…" onClick={() => (setMenu(null), setAddingCodex(true))} />
            <MenuItem label="Open files" onClick={() => (setMenu(null), openFiles(project.id))} />
            <MenuDivider />
            {/* Whichever applies, never both: "open beside" on a project
                already on screen would only focus it, which a click does. */}
            {!tileNo && !selected && (
              <MenuItem
                label={`Open beside${isMac() ? "  (⌘-click)" : "  (Ctrl-click)"}`}
                onClick={() => (setMenu(null), openBeside(project.id))}
              />
            )}
            {tileId && (
              <MenuItem label="Close its tile (sessions keep running)" onClick={() => (setMenu(null), closeTile(tileId))} />
            )}
            {(!!tileId || (!tileNo && !selected)) && <MenuDivider />}
            <MenuItem label="Rename" onClick={() => (setMenu(null), setEditing(true))} />
            <MenuItem label="Remove project…" destructive onClick={() => (setMenu(null), setConfirming(true))} />
          </MenuSurface>
        )}
        <div className="sb-line">
          <span className="sb-num">{number}</span>
          <button
            type="button"
            className="sb-toggle"
            aria-expanded={open}
            aria-label={open ? `Hide the tabs of ${project.label}` : `Show the tabs of ${project.label}`}
            onClick={(e) => {
              e.stopPropagation();
              onToggle();
            }}
          >
            {open ? "▾" : "▸"}
          </button>
          <StateMark state={state} />
          {tileNo && <TileMark n={tileNo} focused={selected} />}
          {editing ? (
            <input
              autoFocus
              defaultValue={project.label}
              spellCheck={false}
              aria-label="Project name"
              className="data inset min-w-0 flex-1 px-1 text-[12px] outline-none"
              style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
              onClick={(e) => e.stopPropagation()}
              onBlur={(e) => {
                const next = e.currentTarget.value.trim();
                if (next && next !== project.label) renameProject(project.id, next);
                setEditing(false);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
                if (e.key === "Escape") {
                  e.currentTarget.value = project.label;
                  e.currentTarget.blur();
                }
              }}
            />
          ) : (
            <span className="sb-label" style={project.running ? { color: "var(--overlay0, var(--text-faint))" } : undefined}>
              {project.label}
            </span>
          )}
          {/* The verbs, shown on hover (and in the right-click menu): files,
              terminal, Claude, pi, remove. */}
          <span className="sb-actions flex shrink-0 items-center" onClick={(e) => e.stopPropagation()}>
            <button type="button" className="sb-act sb-hover" onClick={() => openFiles(project.id)} aria-label={`Open files in ${project.label}`} title="Open files">
              <FolderIcon />
            </button>
            <button type="button" className="sb-act sb-hover" onClick={newTerminal} aria-label={`New terminal in ${project.label}`} title="New terminal">
              <TerminalMark color="currentColor" />
            </button>
            <button type="button" className="sb-act sb-hover" onClick={() => setAdding(true)} aria-label={`New Claude session in ${project.label}`} title="New Claude session">
              <ClaudeMark color="currentColor" />
            </button>
            <button type="button" className="sb-act sb-hover" onClick={() => setAddingPi(true)} aria-label={`New pi session in ${project.label}`} title="New or resume a pi session">
              <PiMark color="currentColor" />
            </button>
            <button type="button" className="sb-act sb-hover" onClick={newDsh} aria-label={`New DeepSeek Harness in ${project.label}`} title="New DeepSeek Harness (its Web UI in a pane)">
              <DshMark color="currentColor" />
            </button>
            <button type="button" className="sb-act sb-hover" onClick={() => setAddingCodex(true)} aria-label={`New Codex session in ${project.label}`} title="New or resume a Codex session">
              <CodexMark color="currentColor" />
            </button>
            <button type="button" className="sb-act sb-hover" onClick={() => setConfirming(true)} aria-label={`Remove ${project.label}`} title="Remove this project">
              <CloseIcon />
            </button>
          </span>
        </div>
        {branch && <BranchLine branch={branch} />}
      </div>

      {adding && (
        /* A modal: opened by a deliberate click, so it cannot steal a typist's keys. */
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ duration: 0.12 }}
          className="fixed inset-0 z-[95] grid place-items-start justify-center pt-[9vh]"
          style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
          onClick={() => setAdding(false)}
        >
          <div className="window w-[min(560px,92vw)]" onClick={(e) => e.stopPropagation()}>
            <QuickAdd
              label={project.label}
              where={`${project.folder} on ${serverLabel(server)}`}
              // Claude sessions only: a Codex or dsh session's profile is of
              // another kind and would be refused by a Claude start.
              inheritedProfile={mine.find((s) => s.kind === "claude" && s.modelProfile)?.modelProfile}
              busy={false}
              target={target}
              folder={project.folder}
              onCancel={() => setAdding(false)}
              onCreate={addClaude}
            />
          </div>
        </motion.div>
      )}

      {addingPi && (
        <PiSessionPicker
          target={target}
          onCancel={() => setAddingPi(false)}
          onChoose={(choice) => {
            const id = addSession(project.id, "pi", {
              resume: choice.resume,
              cwd: choice.cwd,
              name: choice.name?.trim() || undefined,
            });
            openSession(id);
            setAddingPi(false);
          }}
        />
      )}

      {addingCodex && (
        <CodexSessionPicker
          target={target}
          onCancel={() => setAddingCodex(false)}
          onChoose={(choice) => {
            const id = addSession(project.id, "codex", {
              resume: choice.resume,
              cwd: choice.cwd,
              name: choice.name?.trim() || undefined,
              modelProfile: choice.modelProfile,
              bypassApprovals: choice.bypassApprovals || undefined,
            });
            openSession(id);
            setAddingCodex(false);
          }}
        />
      )}

      {/* `removeProject` ends every session in it, each a real kill on the host,
          so the count is the whole message. */}
      {confirming && (
        <div className="sb-confirm pl-8">
          <span className="micro leading-relaxed" style={{ color: "var(--text-soft)" }}>
            {mine.length > 0
              ? `ENDS ${mine.length} SESSION${mine.length === 1 ? "" : "S"} ON THE SERVER`
              : "REMOVES THIS PROJECT FROM THE LIST"}
          </span>
          <div className="flex gap-2">
            <button
              type="button"
              className="chip"
              style={{ color: "var(--st-blocked)" }}
              onClick={() => {
                setConfirming(false);
                removeProject(project.id);
              }}
            >
              remove it
            </button>
            <button type="button" className="chip" onClick={() => setConfirming(false)}>
              cancel
            </button>
          </div>
        </div>
      )}

      {open && (
        <div className="sb-children">
          {pt.tabs.map((tab, i) => (
            <TabBlock
              key={tab.id}
              projectId={project.id}
              tab={tab}
              index={i}
              model={model}
              activeTab={selected && pt.activeTab === tab.id}
              visible={visible}
            />
          ))}
          {orphans.map((s) => (
            <SessionItem
              key={s.id}
              session={s}
              state={model.stateOf[s.id] ?? null}
              active={false}
              onScreen={model.onScreen.has(s.id)}
            />
          ))}
          {pt.tabs.length === 0 && orphans.length === 0 && (
            <p className="sb-empty">{" no tabs — add one with the terminal or ✦ verbs"}</p>
          )}
        </div>
      )}
    </div>
  );
}

// ── a machine and its spaces ─────────────────────────────────────────────────

function MachineGroup({
  machine,
  model,
  folded,
  onToggle,
  expanded,
  onToggleSpace,
  onNewProject,
}: {
  machine: MachineModel;
  model: Model;
  folded: boolean;
  onToggle: () => void;
  expanded: Set<string>;
  onToggleSpace: (id: string) => void;
  onNewProject: (serverId: string) => void;
}) {
  const { server, state, spaces } = machine;
  const [menu, setMenu] = useState<MenuAt | null>(null);
  const [attaching, setAttaching] = useState(false);
  const [confirming, setConfirming] = useState(false);
  /** What the last disconnect did, shown beside the control that did it. */
  const [note, setNote] = useState<string | null>(null);
  const allProjects = useWorkspace((s) => s.projects);
  const allSessions = useWorkspace((s) => s.sessions);
  // Subscribed so the connection label follows handles coming and going.
  useWorkspace((s) => s.live);
  const openHost = useWorkspace((s) => s.openHost);
  const removeServer = useWorkspace((s) => s.removeServer);
  const branches = useBranches();

  const projectCount = useMemo(() => allProjects.filter((p) => p.serverId === server.id).length, [allProjects, server.id]);
  const sessionCount = useMemo(() => {
    const ids = new Set(allProjects.filter((p) => p.serverId === server.id).map((p) => p.id));
    return allSessions.filter((s) => ids.has(s.projectId)).length;
  }, [allProjects, allSessions, server.id]);
  const open = !folded || model.searching;
  const local = !server.target.host;
  const connected = isServerConnected(server.id);
  const [editing, setEditing] = useState(false);

  /** Disconnect, and say what it did. Shared by the menu and Connection settings. */
  const disconnect = async (): Promise<string> => {
    // Stop the sidebar's own branch poll first, so it is not what dials the
    // host straight back.
    markDisconnected(server.id);
    const r = await api.serverDisconnect(server.target);
    return r.closed
      ? "disconnected · sessions still running · reconnects on next use"
      : r.evicted > 0
        ? "forgotten · nothing was open"
        : "was not connected";
  };

  return (
    <div>
      <div
        className="sb-machine"
        onContextMenu={(e) => {
          e.preventDefault();
          setNote(null);
          setMenu({ x: e.clientX, y: e.clientY });
        }}
      >
        {menu && (
          <MenuSurface at={menu} onClose={() => setMenu(null)} minWidth={230}>
            <MenuItem label="Attach a running session…" onClick={() => (setMenu(null), setAttaching(true))} />
            <MenuDivider />
            {!local && (
              <MenuItem
                label="Connection settings…"
                hint="key"
                onClick={() => (setMenu(null), setEditing(true))}
              />
            )}
            {/* Disconnect keeps the server here, and the sessions running. */}
            <MenuItem
              label="Disconnect"
              hint="stays in the list"
              onClick={() => {
                setMenu(null);
                setNote("disconnecting…");
                void disconnect()
                  .then(setNote)
                  .catch((e) => setNote(e instanceof Error ? e.message : String(e)));
              }}
            />
            <MenuItem label="Remove server…" destructive onClick={() => (setMenu(null), setConfirming(true))} />
          </MenuSurface>
        )}
        <button type="button" className="sb-toggle" onClick={onToggle} aria-expanded={open} aria-label={open ? "Fold this machine" : "Unfold this machine"}>
          {open ? "▾" : "▸"}
        </button>
        <StateMark state={state} />
        <button
          type="button"
          className="sb-machine-name"
          onClick={onToggle}
          title={`${serverLabel(server)}\nClick to ${open ? "fold" : "unfold"}`}
        >
          {serverLabel(server)}
        </button>
        <span className="sb-conn" title={connected ? "connected" : "not connected — reconnects on next use"}>
          {local ? "local" : connected ? "ssh" : "ssh ·off"}
        </span>
        <button type="button" className="sb-act" onClick={() => openHost(server.id)} aria-label="Host" title="Host: processes, ports, metrics">
          <HostIcon />
        </button>
        <button
          type="button"
          className="sb-act"
          onClick={() => onNewProject(server.id)}
          aria-label={`New project on ${serverLabel(server)}`}
          title="Add a project (folder) on this server"
        >
          <PlusIcon />
        </button>
        <button type="button" className="sb-act sb-hover" onClick={() => setConfirming(true)} aria-label={`Remove ${serverLabel(server)}`} title="Remove this server">
          <CloseIcon />
        </button>
      </div>

      {editing && (
        <ServerConnectionSettings server={server} onClose={() => setEditing(false)} onReconnect={disconnect} />
      )}
      {attaching && (
        <RunningSessions serverId={server.id} target={server.target} label={serverLabel(server)} onClose={() => setAttaching(false)} />
      )}
      {note && (
        <p className="micro px-3 pb-1" style={{ color: "var(--text-soft)" }}>
          {note.toUpperCase()}
        </p>
      )}
      {/* Removing a server cascades to its projects and their sessions, each a
          real kill on the host — so the counts are stated before the press. */}
      {confirming && (
        <div className="sb-confirm">
          <span className="micro leading-relaxed" style={{ color: "var(--text-soft)" }}>
            {sessionCount > 0
              ? `ENDS ${sessionCount} SESSION${sessionCount === 1 ? "" : "S"} AND ${projectCount} PROJECT${projectCount === 1 ? "" : "S"} ON THE SERVER`
              : "REMOVES THIS SERVER FROM THE LIST"}
          </span>
          <div className="flex gap-2">
            <button
              type="button"
              className="chip"
              style={{ color: "var(--st-blocked)" }}
              onClick={() => {
                setConfirming(false);
                removeServer(server.id);
              }}
            >
              remove it
            </button>
            <button type="button" className="chip" onClick={() => setConfirming(false)}>
              cancel
            </button>
          </div>
        </div>
      )}

      {open &&
        spaces.map((space) => (
          <SpaceRow
            key={space.project.id}
            space={space}
            server={server}
            model={model}
            expanded={expanded.has(space.project.id)}
            onToggle={() => onToggleSpace(space.project.id)}
            branch={branches[space.project.id]?.value}
          />
        ))}
      {open && !model.searching && spaces.length === 0 && <p className="sb-empty">{" no projects — add one with +"}</p>}
    </div>
  );
}

// ── the compact column ───────────────────────────────────────────────────────

function CompactRail({ model, sort, onExpand }: { model: Model; sort: AgentSort; onExpand: () => void }) {
  const selectProject = useWorkspace((s) => s.selectProject);
  const openBeside = useWorkspace((s) => s.openBeside);
  const activate = useWorkspace((s) => s.activate);
  const active = useWorkspace((s) => s.activeSession);
  const agents = sortAgents(model.agents, sort);

  return (
    <aside className="sb sb-compact" aria-label="Sidebar (compact)">
      <button type="button" className="sb-cell" onClick={onExpand} title="Expand the sidebar" aria-label="Expand the sidebar">
        »
      </button>
      {model.needing > 0 && (
        <span className="sb-needs text-center" title={`${model.needing} need you`}>
          {model.needing}
        </span>
      )}
      <div className="sb-scroll" style={{ flex: "0 1 auto", maxHeight: "55%" }}>
        {model.machines.flatMap((m) =>
          m.spaces.map((sp) => (
            <button
              key={sp.project.id}
              type="button"
              className="sb-cell"
              data-active={model.layout.activeProject === sp.project.id}
              data-shown={model.tiles.has(sp.project.id) ? true : undefined}
              onClick={(e) => ((isMac() ? e.metaKey : e.ctrlKey) ? openBeside(sp.project.id) : selectProject(sp.project.id))}
              title={`${sp.number} ${sp.project.label} — ${serverLabel(m.server)}${model.tiles.has(sp.project.id) ? `\non screen, tile ${model.tiles.get(sp.project.id)}` : ""}${sp.state ? `\n${STATE_LABEL[sp.state]}` : ""}`}
            >
              <span>{sp.number}</span>
              <StateMark state={sp.state} />
            </button>
          )),
        )}
      </div>
      <div className="sb-rule" />
      <div className="sb-scroll" style={{ flex: "1 1 0%", maxHeight: "45%" }}>
        {agents.map((a) => (
          <button
            key={a.session.id}
            type="button"
            className="sb-cell"
            data-active={a.session.id === active}
            onClick={() => activate(a.session.id)}
            title={`${a.where}\n${a.session.name}\n${STATE_LABEL[a.state]}`}
          >
            <StateMark state={a.state} />
          </button>
        ))}
      </div>
    </aside>
  );
}

// ── the sidebar ──────────────────────────────────────────────────────────────

export function WorkspaceRail({
  onConnectServer,
  onNewProject,
}: {
  /** Opens the host-picker (ssh-config aliases + manual) to connect a Server. */
  onConnectServer: () => void;
  /** Opens the folder-picker on a Server to create a Project. */
  onNewProject: (serverId: string) => void;
}) {
  const servers = useWorkspace((s) => s.servers);
  const collapsed = useWorkspace((s) => s.railCollapsed);
  const toggleRail = useWorkspace((s) => s.toggleRail);
  const active = useWorkspace((s) => s.activeSession);
  const { width: railWidth, startResize } = useRailWidth("rmux.rail.width", RAIL_DEFAULT);

  // Not persisted: a filter that survives a restart opens the app to a sidebar
  // that looks like sessions have gone missing.
  const [query, setQuery] = useState("");
  const model = useSidebarModel(query);
  const [folded, toggleFold] = useToggleSet(FOLD_KEY);
  const [expanded, toggleExpanded] = useToggleSet(EXPANDED_KEY);
  const [sort, setSort] = useState<AgentSort>(() => (readString(SORT_KEY) === "priority" ? "priority" : "grouped"));
  const [split, setSplit] = useState(() => {
    const n = Number(readString(SPLIT_KEY));
    return Number.isFinite(n) && n >= SPLIT_MIN && n <= SPLIT_MAX ? n : 0.6;
  });
  const body = useRef<HTMLDivElement>(null);
  const aside = useRef<HTMLElement>(null);
  /** When the sidebar itself was last pressed — a switch made by clicking a
   *  row is already where the operator is looking, so it is not flashed. */
  const pressedAt = useRef(0);

  // **Follow the active session.** A switch from ⌘K, a notification card or a
  // shortcut lands on a session the sidebar may be showing nowhere — its
  // machine folded, its project closed, its row scrolled off — so finding
  // which row you are now in meant scrolling and reading. On every switch the
  // row is unfolded into existence, scrolled into view (only if it is not
  // already), and briefly flashed so the eye lands on it. Unfolding is
  // remembered like a manual unfold: it is what the operator would have done.
  useEffect(() => {
    if (!active || collapsed) return;
    const st = useWorkspace.getState();
    const session = st.sessions.find((x) => x.id === active);
    const project = session && st.projects.find((x) => x.id === session.projectId);
    if (!project) return;
    if (folded.has(project.serverId)) toggleFold(project.serverId);
    if (!expanded.has(project.id)) toggleExpanded(project.id);
    const flash = Date.now() - pressedAt.current > 800;
    // Two frames: the unfold has to render before the row has a box.
    let raf = requestAnimationFrame(() => {
      raf = requestAnimationFrame(() => {
        const rows = aside.current?.querySelectorAll<HTMLElement>(`[data-session="${CSS.escape(active)}"]`) ?? [];
        rows.forEach((row) => {
          const pane = row.closest<HTMLElement>(".sb-scroll");
          // Centre the row in its own scroller. `scrollIntoView({block:"nearest"})`
          // parked it at the very edge, under the sticky project header, so the
          // row it was meant to reveal was the one thing hidden. Only when the
          // switch came from elsewhere (⌘K, a shortcut): a row clicked in the
          // sidebar is already where the hand is and must not move under it.
          if (pane && flash) {
            const r = row.getBoundingClientRect();
            const c = pane.getBoundingClientRect();
            const target = pane.scrollTop + (r.top - c.top) - (pane.clientHeight - r.height) / 2;
            const max = pane.scrollHeight - pane.clientHeight;
            pane.scrollTo({ top: Math.max(0, Math.min(max, target)), behavior: "smooth" });
          }
          if (flash) {
            row.classList.remove("sb-flash");
            void row.offsetWidth; // restart the animation on a repeat switch
            row.classList.add("sb-flash");
          }
        });
      });
    });
    return () => cancelAnimationFrame(raf);
    // Only a change of session should move the sidebar; folding by hand
    // afterwards must not be undone.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, collapsed]);

  // Branch lines are drawn only at full width; the compact column has none, so
  // it does not poll (a widget that is not shown must not run).
  useBranchPoll(!collapsed);

  const startSplit = (e: React.PointerEvent) => {
    e.preventDefault();
    const el = body.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    let latest = split;
    const move = (ev: PointerEvent) => {
      latest = Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, (ev.clientY - rect.top) / rect.height));
      setSplit(latest);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      writeString(SPLIT_KEY, String(latest));
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  if (collapsed) return <CompactRail model={model} sort={sort} onExpand={toggleRail} />;

  const agents = sortAgents(model.agents, sort);
  const pct = `${(split * 100).toFixed(2)}%`;
  const rest = `${((1 - split) * 100).toFixed(2)}%`;

  return (
    <aside
      ref={aside}
      className="sb"
      style={{ width: railWidth }}
      aria-label="Sidebar"
      onPointerDownCapture={() => (pressedAt.current = Date.now())}
    >
      <RailGrip side="right" onPointerDown={(e) => startResize(e, "left")} />

      <div className="sb-top">
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              setQuery("");
              e.currentTarget.blur();
            }
          }}
          placeholder="filter spaces and agents"
          aria-label="Filter spaces and agents"
          spellCheck={false}
          className="sb-search"
        />
        {query && (
          <button type="button" className="sb-act" onClick={() => setQuery("")} aria-label="Clear search" title="Clear (Esc)">
            <CloseIcon />
          </button>
        )}
        {model.needing > 0 && (
          <span className="sb-needs" title="blocked, or finished and not looked at yet">
            {model.needing} need you
          </span>
        )}
      </div>

      <div ref={body} className="flex min-h-0 flex-1 flex-col">
        <div className="sb-scroll" style={{ flex: `0 0 calc(${pct} - 3px)`, maxHeight: `calc(${pct} - 3px)` }}>
          <div className="sb-head">{" spaces"}</div>
          {model.machines.map((m) => (
            <MachineGroup
              key={m.server.id}
              machine={m}
              model={model}
              folded={folded.has(m.server.id)}
              onToggle={() => toggleFold(m.server.id)}
              expanded={expanded}
              onToggleSpace={toggleExpanded}
              onNewProject={onNewProject}
            />
          ))}
          {model.searching && model.machines.length === 0 && <p className="sb-empty">{" no matching spaces"}</p>}
          {servers.length === 0 && <p className="sb-empty">{" no servers yet — press new"}</p>}
        </div>

        <div
          className="sb-divider"
          role="separator"
          aria-orientation="horizontal"
          title="Drag to resize"
          onPointerDown={startSplit}
        />

        <div className="sb-scroll" style={{ flex: "1 1 0%", maxHeight: `calc(${rest} - 2px)` }}>
          <div className="sb-head">
            <span>{" agents"}</span>
            <button
              type="button"
              className="sb-sort"
              title="Sort: grouped by machine and project, or by priority (blocked, done, working, idle)"
              onClick={() => {
                const next = sort === "grouped" ? "priority" : "grouped";
                setSort(next);
                writeString(SORT_KEY, next);
              }}
            >
              {sort}
            </button>
          </div>
          {agents.map((a) => (
            <SessionItem
              key={a.session.id}
              session={a.session}
              state={a.state}
              active={a.session.id === active}
              onScreen={model.onScreen.has(a.session.id)}
              where={a.where}
              parentName={a.parentName}
            />
          ))}
          {agents.length === 0 && <p className="sb-empty">{" no matching agents"}</p>}
        </div>
      </div>

      <div className="sb-foot">
        <button type="button" onClick={onConnectServer} title="Connect a server (⌘N)">
          {" new"}
        </button>
        <button type="button" onClick={toggleRail} title="Collapse the sidebar" aria-label="Collapse the sidebar">
          «
        </button>
      </div>
    </aside>
  );
}

/**
 * Which account a chained session runs on **now** — the sidebar's half of the
 * loud-switch rule. Only for a chain: a fixed profile cannot change under the
 * operator, so naming it on every row would be noise.
 */
function ProfileLine({ session }: { session: SessionV3 }) {
  const chained = isChainId(session.modelProfile);
  const names = useProfileNames(chained);
  if (!chained) return null;
  const active = session.activeProfile;
  const chain = names[session.modelProfile!] ?? "chain";
  return (
    <span className="sb-parent" title={`Fallback chain ${chain}`}>
      ⇄ {active ? (names[active] ?? active) : "first in chain"} · {chain}
    </span>
  );
}

// ── glyphs ───────────────────────────────────────────────────────────────────
/** Terminal glyph: a prompt mark (`>_`). Rule 3 — inline SVG, square caps. */
function TerminalMark({ color = "var(--text-faint)" }: { color?: string }) {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth="2.4"
      strokeLinecap="square"
      aria-hidden="true"
      className="shrink-0"
    >
      <path d="M5 7l5 5-5 5M13 17h6" />
    </svg>
  );
}

/**
 * The Claude mark — the official lobehub Claude glyph
 * (https://lobehub.com/icons/claude). Rule 3: inline SVG, no glyph font, no
 * emoji. A fill path, not a stroke, so it takes `fill`, not `stroke`; the colour
 * comes from the tokens so it follows the theme.
 */
function ClaudeMark({ color = "var(--text-soft)" }: { color?: string }) {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill={color}
      fillRule="evenodd"
      aria-hidden="true"
      className="shrink-0"
    >
      <path d="M4.709 15.955l4.72-2.647.08-.23-.08-.128H9.2l-.79-.048-2.698-.073-2.339-.097-2.266-.122-.571-.121L0 11.784l.055-.352.48-.321.686.06 1.52.103 2.278.158 1.652.097 2.449.255h.389l.055-.157-.134-.098-.103-.097-2.358-1.596-2.552-1.688-1.336-.972-.724-.491-.364-.462-.158-1.008.656-.722.881.06.225.061.893.686 1.908 1.476 2.491 1.833.365.304.145-.103.019-.073-.164-.274-1.355-2.446-1.446-2.49-.644-1.032-.17-.619a2.97 2.97 0 01-.104-.729L6.283.134 6.696 0l.996.134.42.364.62 1.414 1.002 2.229 1.555 3.03.456.898.243.832.091.255h.158V9.01l.128-1.706.237-2.095.23-2.695.08-.76.376-.91.747-.492.584.28.48.685-.067.444-.286 1.851-.559 2.903-.364 1.942h.212l.243-.242.985-1.306 1.652-2.064.73-.82.85-.904.547-.431h1.033l.76 1.129-.34 1.166-1.064 1.347-.881 1.142-1.264 1.7-.79 1.36.073.11.188-.02 2.856-.606 1.543-.28 1.841-.315.833.388.091.395-.328.807-1.969.486-2.309.462-3.439.813-.042.03.049.061 1.549.146.662.036h1.622l3.02.225.79.522.474.638-.079.485-1.215.62-1.64-.389-3.829-.91-1.312-.329h-.182v.11l1.093 1.068 2.006 1.81 2.509 2.33.127.578-.322.455-.34-.049-2.205-1.657-.851-.747-1.926-1.62h-.128v.17l.444.649 2.345 3.521.122 1.08-.17.353-.608.213-.668-.122-1.374-1.925-1.415-2.167-1.143-1.943-.14.08-.674 7.254-.316.37-.729.28-.607-.461-.322-.747.322-1.476.389-1.924.315-1.53.286-1.9.17-.632-.012-.042-.14.018-1.434 1.967-2.18 2.945-1.726 1.845-.414.164-.717-.37.067-.662.401-.589 2.388-3.036 1.44-1.882.93-1.086-.006-.158h-.055L4.132 18.56l-1.13.146-.487-.456.061-.746.231-.243 1.908-1.312-.006.006z" />
    </svg>
  );
}

/**
 * The pi mark — pi is an agent like Claude, so it earns an agent glyph. There is
 * no official brand icon, so this is the Greek letter itself, drawn as text
 * rather than an SVG path: `π` reads cleanly at 12px and needs no glyph font
 * (it is a base-plane Unicode letter every bundled face carries). Rule 3 — no
 * emoji, and the colour comes from the tokens so it follows the theme.
 */
function PiMark({ color = "var(--text-soft)" }: { color?: string }) {
  return (
    <span
      aria-hidden="true"
      className="shrink-0"
      style={{
        display: "inline-block",
        width: 12,
        fontSize: 13,
        fontWeight: 700,
        lineHeight: "12px",
        textAlign: "center",
        color,
      }}
    >
      π
    </span>
  );
}

/**
 * The DeepSeek Harness mark — `DS` set as text, like pi's `π`: there is no
 * brand glyph we may ship, and two letters read at 12px where a drawn whale
 * would not. Colour from the tokens.
 */
export function DshMark({ color = "var(--text-soft)" }: { color?: string }) {
  return (
    <span
      aria-hidden="true"
      className="shrink-0"
      style={{
        display: "inline-block",
        width: 14,
        fontSize: 9,
        fontWeight: 700,
        letterSpacing: "-0.02em",
        lineHeight: "12px",
        textAlign: "center",
        color,
      }}
    >
      DS
    </span>
  );
}

/**
 * The Codex mark — `>_` set as text, like pi's `π` and dsh's `DS`: there is no
 * brand glyph we may ship, and a prompt caret reads as "a coding agent in a
 * terminal" at 12px. Colour from the tokens.
 */
export function CodexMark({ color = "var(--text-soft)" }: { color?: string }) {
  return (
    <span
      aria-hidden="true"
      className="shrink-0"
      style={{
        display: "inline-block",
        width: 14,
        fontSize: 10,
        fontWeight: 700,
        letterSpacing: "-0.06em",
        lineHeight: "12px",
        textAlign: "center",
        color,
      }}
    >
      {">_"}
    </span>
  );
}

function CloseIcon() {
  return (
    <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="square" aria-hidden="true">
      <path d="M18 6L6 18M6 6l12 12" />
    </svg>
  );
}

function PlusIcon() {
  return (
    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="square" aria-hidden="true">
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

/** Folder glyph — the explicit "open this project's files" affordance, because
 *  the uppercase label reads as a section header, not a button. Rule 3. */
function FolderIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true">
      <path d="M3 6v13h18V8h-9l-2-2H3z" />
    </svg>
  );
}

/** Server-rack glyph for HOST — the machine itself, not one of its readouts
 *  (processes, ports, metrics all belong to the box). Stacked rectangles fit the
 *  zero-radius language; the two square LEDs are where a status tint could go. */
function HostIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true">
      <rect x="3" y="4" width="18" height="7" />
      <rect x="3" y="13" width="18" height="7" />
      <path d="M7 7.5h.01" />
      <path d="M7 16.5h.01" />
    </svg>
  );
}
