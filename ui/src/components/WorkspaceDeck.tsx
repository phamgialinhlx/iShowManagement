import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { invoke } from "../lib/bridge";

import { useWorkspace } from "../lib/workspace";
import {
  UNIT,
  activeTab,
  activeTabOf,
  findLeaf,
  leafRects,
  leaves,
  splitRects,
  viewTree,
  warmTabs,
  type Direction,
  type Leaf,
  type MoveSource,
  type ProjectLeaf,
  type Rect,
  type Split,
  type SplitOf,
  type Tab,
  type RatioOverride,
} from "../lib/layout-tree";
import { basename, reattachName, type PaneRef, type ProjectId, type SessionV3 } from "../lib/workspace-model";
import { ClaudeSessionPane } from "./ClaudeSessionPane";
import { PiSessionPane } from "./PiSessionPane";
import { DshSessionPane } from "./DshSessionPane";
import { CodexSessionPane } from "./CodexSessionPane";
import { TerminalView } from "./Terminal";
import { HostPanel } from "./HostPanel";
import { FilesPane } from "./FilesPane";
import { PaneHeader } from "./PaneHeader";
import { SessionSettings } from "./SessionSettings";
import { BackView, GEAR_PATH, Icon, IconButton, RELOAD_PATH } from "./SessionPaneChrome";
import { TabBar } from "./TabBar";
import { ConfirmClose } from "./ConfirmClose";
import { useEndedChildren } from "../lib/children-watch";
import { requestClose } from "../lib/shortcuts";
import { effectiveLowLatency } from "../lib/transport-default";
import { currentSessionDrag, endSessionDrag, isSessionDrag, useSessionDrag } from "../lib/session-drag";
import { PaneDragProvider } from "./PaneDrag";
import { PaneMoveMenu } from "./PaneMoveMenu";
import type { MenuAt } from "./Menu";

/** What dragging a pane's header moves: its session (the same move as its
 *  sidebar row), or — for a host or files pane — the pane by position. */
function leafSource(projectId: ProjectId, tabId: string, leaf: Leaf): MoveSource {
  return leaf.pane.kind === "session"
    ? { kind: "session", sessionId: leaf.pane.id }
    : { kind: "leaf", projectId, tabId, leafId: leaf.id };
}

/**
 * The workbench main area, v4 — herdr's model: the active **project**'s **tabs**,
 * each a binary **split tree** of panes (`layout-tree.ts`).
 *
 * A pane holds a *content source*: a session (a Claude TUI or a shell), a
 * Server's host panel, or a Project's files. This replaced the equal-size grid
 * ("deck"), which could only say "N equal tiles, filled in rail order".
 */

/**
 * A plain shell pane — the terminal, plus the one sub-view it has.
 *
 * **Settings is not a Claude luxury.** TRANSPORT is a property of the
 * connection, and `terminal_open` has carried `low_latency` from the start — but
 * the only way to reach the control was the Claude pane's gear, so a shell on a
 * 271ms link could not be switched to the low-latency transport at all. That is
 * backwards: a shell is precisely where each keystroke's round trip is felt.
 * `SessionSettings` renders only the sections that apply to the kind, so what a
 * shell gets here is TRANSPORT and nothing Claude-shaped.
 *
 * **The terminal is hidden, never unmounted.** The PTY outlives this component,
 * but tearing the view down on every glance at the settings would detach and
 * replay the scrollback each time; `display: none` is what the Claude and pi
 * panes already do with their TUIs, and it is what this follows. Coming back is
 * handled by `TerminalView`'s own `ResizeObserver`, which bails while the host
 * measures 0x0 and fires again with the real size the moment it is shown — so
 * the stale cell grid is re-fitted with no second mechanism invented here.
 */
function ShellSessionPane({ session }: { session: SessionV3 }) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const live = useWorkspace((s) => s.live[session.id]);
  const setLive = useWorkspace((s) => s.setLive);
  const clearLive = useWorkspace((s) => s.clearLive);
  // Ephemeral and local to the pane, like the agent panes' view: persisting it
  // would reopen someone into a settings screen they closed days ago, and a
  // shared one would switch all sixteen tiles at once.
  const [view, setView] = useState<"shell" | "settings">("shell");

  /**
   * RELOAD — force a reattach when the pane has gone quiet.
   *
   * `TerminalView` recovers on its own only when the `ssh` attach *exits*, and
   * the keepalive gives that 90 seconds (`ServerAliveInterval=15` x
   * `ServerAliveCountMax=6`). On a 271ms bufferbloated link a stall inside that
   * window is a pane fed by a stream that stopped: nothing arrives, typing goes
   * nowhere, and before this the only way out was closing the pane — which sends
   * `terminal_close`, and `terminal_close` carries an `agent kill`. That is the
   * difference this control exists to avoid.
   *
   * Three steps, and the order is the whole correctness of it:
   *
   * 1. **Let go of the local handle.** `live[session.id]` names an attachment
   *    inside the connection that just stalled; asking to reattach to it yields
   *    "no such session" forever, which is the shape of a pane that never comes
   *    back. It is also what makes the next attach take the *named* path.
   * 2. **`terminal_detach`, never `terminal_close`.** It takes neither a target
   *    nor a session name — there is nothing to say to the far side — so it has
   *    no path that can end a shell. Skipping it would abandon the wedged `ssh`
   *    child and leave one behind per press.
   * 3. **Bump the generation**, which rebuilds the terminal and attaches under
   *    `reattachName(session)` — the one name the daemon already holds. (Deriving
   *    a different one would *create* a second shell beside the one being
   *    adopted, with nothing pointing at the original.)
   *
   * Guarded by state rather than a ref, unlike the Claude pane's: there is no
   * poll re-reading a stale closure here, only a pointer, and the button is
   * `disabled` for as long as the flag is set — so a double-click cannot fire two
   * reattaches.
   */
  const [generation, setGeneration] = useState(0);
  const [reattaching, setReattaching] = useState(false);
  const reload = () => {
    if (reattaching) return;
    setReattaching(true);
    const stale = live;
    clearLive(session.id);
    if (stale) void invoke("terminal_detach", { id: stale }).catch(() => {});
    setGeneration((n) => n + 1);
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* A shell says nothing about which session or machine it is — the prompt
          is the *host's* idea of that, and in a grid of four they all look
          alike. Folder first, session name second — the same order as the
          Claude pane's header, so a grid of mixed panes reads as one thing.

          Dropped while the settings are open: `BackView` brings its own bar,
          and two bars stacked is the double-header the agent panes exist to
          avoid. */}
      {view === "shell" && (
        <PaneHeader
          label={project ? basename(project.folder) : session.name}
          detail={project ? session.name : undefined}
          actions={
            <>
              <IconButton label="Session settings" onClick={() => setView("settings")}>
                <Icon d={GEAR_PATH} />
              </IconButton>
              <IconButton
                label={
                  reattaching
                    ? "Reattaching…"
                    : `Reattach this session — reconnects to ${target.host ?? "this machine"} without ending your work`
                }
                disabled={reattaching}
                onClick={reload}
              >
                <Icon d={RELOAD_PATH} />
              </IconButton>
            </>
          }
        />
      )}
      <div className="relative min-h-0 flex-1">
        <div className="h-full" style={{ display: view === "shell" ? "block" : "none" }}>
          <TerminalView
            target={target}
            // Adopted sessions carry no folder of ours: the synthetic project
            // that holds them is a container, not a directory anyone chose.
            cwd={session.hostName ? undefined : project?.folder}
            session={reattachName(session)}
            sessionId={session.id}
            lowLatency={effectiveLowLatency(session)}
            run={session.run}
            ptyId={live}
            generation={generation}
            onOpened={(id) => {
              setLive(session.id, id);
              // The attach landed, so the control is live again. Cleared on the
              // *handle*, not on a timer: a reattach over a 271ms link takes as
              // long as it takes, and a button that re-enables itself early
              // invites a second one on top of the first.
              setReattaching(false);
            }}
            onAttachFailed={() => setReattaching(false)}
            onExit={() => clearLive(session.id)}
          />
        </div>

        {view === "settings" && (
          <BackView label="SETTINGS" backLabel="Back to the terminal" onBack={() => setView("shell")}>
            <SessionSettings sessionId={session.id} />
          </BackView>
        )}
      </div>
    </div>
  );
}

/** A single session pane — Claude, pi or a shell, by kind. Exported for the
 *  mobile bundle's TERMINAL view, which reuses the desktop pane as it is. */
export function SessionPane({ session }: { session: SessionV3 }) {
  if (session.kind === "claude") return <ClaudeSessionPane session={session} />;
  if (session.kind === "pi") return <PiSessionPane session={session} />;
  if (session.kind === "dsh") return <DshSessionPane session={session} />;
  if (session.kind === "codex") return <CodexSessionPane session={session} />;
  return <ShellSessionPane session={session} />;
}

/** A server's host pane — processes, ports, metrics. */
function HostPane({ serverId }: { serverId: string }) {
  const server = useWorkspace((s) => s.servers.find((sv) => sv.id === serverId));
  if (!server) return <PaneMissing what="server" />;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <PaneHeader label={serverId} detail="host" />
      <div className="min-h-0 flex-1">
        <HostPanel target={server.target} serverId={server.id} />
      </div>
    </div>
  );
}

/** A project's files. The tab strip names *files*; this names the project. */
function FilesPaneTile({ projectId }: { projectId: string }) {
  const project = useWorkspace((s) => s.projects.find((p) => p.id === projectId));
  return (
    <div className="flex h-full min-h-0 flex-col">
      <PaneHeader label={project?.label ?? "files"} detail={project?.folder} />
      <div className="min-h-0 flex-1">
        <FilesPane projectId={projectId} />
      </div>
    </div>
  );
}

function PaneMissing({ what }: { what: string }) {
  return (
    <div className="grid h-full place-items-center">
      <span className="micro" style={{ color: "var(--text-faint)" }}>
        {what} no longer exists
      </span>
    </div>
  );
}

/**
 * A spawned child that the host no longer lists has ended there. Its pane is
 * **kept** — removing it would move the layout under the operator's hands, and
 * whatever it printed last may be the very thing they want to read — and this
 * strip says so and offers the close, through the usual confirmation.
 */
function ChildEnded({ name, onClose }: { name: string; onClose: () => void }) {
  return (
    <div className="child-ended" role="status">
      <span className="micro truncate" style={{ color: "var(--text-soft)" }}>
        {name} ended on the host
      </span>
      <button type="button" className="chip" onClick={onClose} title="Close this pane (asks first)">
        close
      </button>
    </div>
  );
}

function Pane({ pane, at }: { pane: PaneRef; at: { projectId: ProjectId; tabId: string; leafId: string } }) {
  const session = useWorkspace((s) =>
    pane.kind === "session" ? s.sessions.find((x) => x.id === pane.id) : undefined,
  );
  const ended = useEndedChildren();
  if (pane.kind === "session") {
    if (!session) return <PaneMissing what="session" />;
    // One structure whether or not the strip shows: the strip occupies its own
    // slot, so the pane below it is never remounted — a remount reattaches, and
    // attaching to a name the host no longer holds would *create* a fresh shell.
    return (
      <div className="flex h-full min-h-0 flex-col">
        {ended.has(session.id) && <ChildEnded name={session.name} onClose={() => requestClose(at)} />}
        <div className="min-h-0 flex-1">
          <SessionPane session={session} />
        </div>
      </div>
    );
  }
  if (pane.kind === "host") return <HostPane serverId={pane.serverId} />;
  return <FilesPaneTile projectId={pane.projectId} />;
}

/**
 * How many tabs stay mounted while hidden — the warm set. Each live pane holds
 * an xterm and a WebGL context, so this bounds them by what was looked at
 * recently rather than by everything opened this run; a tab past it unmounts,
 * and coming back reattaches (scrollback replayed) to the same shells and
 * Claude under `rmux-agent`, so nothing is lost but a moment.
 */
const WARM_TABS = 4;

const pct = (n: number) => `${(n * 100).toFixed(4)}%`;

/**
 * One tab's panes, absolutely placed from the tree's computed rectangles.
 *
 * Flat rather than nested on purpose: a split, close or swap then *moves* boxes
 * keyed by leaf id instead of re-nesting them, so React keeps every pane mounted
 * through any reshape — a terminal is never torn down and replayed because its
 * neighbour was split.
 *
 * **Flat across tiles too.** Every tab is a direct child of the pane area, keyed
 * by tab id and placed over its tile's body by `frame`. Nesting it inside the
 * tile would remount every terminal in a project the moment it moved to another
 * tile (a drop on a tile edge, or a tile closing beside it); placed like this,
 * moving a project between tiles is a change of four percentages.
 *
 * A divider drag is held as a local override and committed once, on release.
 * The panes still follow the pointer — each `TerminalView` refits locally on
 * every `ResizeObserver` callback and tells the far side only once the size
 * settles (`terminal-resize.ts`) — but the store, `localStorage` and every other
 * window's `storage` listener are not written sixty times a second.
 */
function SplitTab({
  projectId,
  tab,
  frame,
  tileFocused,
}: {
  projectId: ProjectId;
  tab: Tab;
  /** The tile rectangle this tab is drawn in, or `null` while warm and hidden. */
  frame: Rect | null;
  tileFocused: boolean;
}) {
  const visible = !!frame;
  const focusLeaf = useWorkspace((s) => s.focusLeaf);
  const setSplitRatio = useWorkspace((s) => s.setSplitRatio);
  const box = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<RatioOverride>(null);
  // A pane header's right-click menu (`PaneMoveMenu`), by leaf.
  const [menu, setMenu] = useState<{ leafId: string; at: MenuAt } | null>(null);
  const openMenuFor = useCallback((leafId: string) => (at: MenuAt) => setMenu({ leafId, at }), []);
  const menuOpeners = useMemo(() => new Map<string, (at: MenuAt) => void>(), []);
  const openerFor = (leafId: string) => {
    let f = menuOpeners.get(leafId);
    if (!f) menuOpeners.set(leafId, (f = openMenuFor(leafId)));
    return f;
  };

  const rects = useMemo(() => leafRects(tab.root, undefined, drag), [tab.root, drag]);
  // Outer splits are drawn *last*, so where an inner divider meets the outer
  // one in a T, the outer seam is the one under the pointer — otherwise a grab
  // at the junction resizes the small inner split instead of the one aimed at.
  const dividers = useMemo(() => splitRects(tab.root, undefined, drag).reverse(), [tab.root, drag]);
  const leafList = useMemo(() => leaves(tab.root), [tab.root]);
  const zoomed = tab.zoomed && !!findLeaf(tab.root, tab.focus);
  const multi = leafList.length > 1;

  const startDrag = (split: Split, rect: Rect) => (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const el = box.current;
    if (!el) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    const bounds = el.getBoundingClientRect();
    const ratioAt = (ev: { clientX: number; clientY: number }) =>
      split.dir === "row"
        ? ((ev.clientX - bounds.left) / bounds.width - rect.x) / rect.w
        : ((ev.clientY - bounds.top) / bounds.height - rect.y) / rect.h;
    const target = e.currentTarget;
    let last = split.ratio;
    const move = (ev: PointerEvent) => {
      last = ratioAt(ev);
      setDrag({ splitId: split.id, ratio: last });
    };
    const up = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
      target.removeEventListener("pointercancel", up);
      setDrag(null);
      setSplitRatio(projectId, tab.id, split.id, last);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
    target.addEventListener("pointercancel", up);
    setDrag({ splitId: split.id, ratio: split.ratio });
  };

  return (
    <div
      ref={box}
      className="split-tab"
      data-dragging={drag ? "true" : undefined}
      data-tab-project={projectId}
      // Hidden, never unmounted, while warm. A hidden terminal host measures
      // 0x0 and its `ResizeObserver` bails rather than telling the far side the
      // window collapsed; it refits with the real size the moment this shows.
      style={
        frame
          ? {
              display: "block",
              left: pct(frame.x),
              top: `calc(${pct(frame.y)} + var(--tile-bar-h))`,
              width: pct(frame.w),
              height: `calc(${pct(frame.h)} - var(--tile-bar-h))`,
            }
          : { display: "none" }
      }
    >
      {leafList.map((leaf) => {
        const r = rects[leaf.id];
        if (!r) return null;
        const focused = leaf.id === tab.focus;
        const shown = !zoomed || focused;
        const place = zoomed ? { x: 0, y: 0, w: 1, h: 1 } : r;
        return (
          <div
            key={leaf.id}
            className="split-leaf"
            // The accent frame marks the pane that has the keyboard, so only
            // the focused tile draws it — four tiles each claiming a focused
            // pane is the "screen says one thing, keys do another" failure.
            data-focused={focused && multi && tileFocused ? "true" : undefined}
            style={{
              display: shown ? "flex" : "none",
              left: pct(place.x),
              top: pct(place.y),
              width: pct(place.w),
              height: pct(place.h),
            }}
            // Clicking a pane focuses it (and its tile). `keyboard: false` — the
            // click already put the cursor where it landed (the terminal's own
            // mousedown handler), and taking it would pull focus off a pressed
            // control.
            onMouseDownCapture={() => {
              if (!focused || !tileFocused) focusLeaf(projectId, tab.id, leaf.id, { keyboard: false });
              else if (leaf.pane.kind === "session") useWorkspace.getState().activate(leaf.pane.id, { keyboard: false });
            }}
          >
            {/* `.panel` keeps the appearance system's tint on every pane. The
                frame carries the border; the terminal host sits inside it. */}
            <div className="panel split-frame">
              <PaneDragProvider source={leafSource(projectId, tab.id, leaf)} openMenu={openerFor(leaf.id)}>
                <Pane pane={leaf.pane} at={{ projectId, tabId: tab.id, leafId: leaf.id }} />
              </PaneDragProvider>
            </div>
            {visible && shown && <PaneDropZone projectId={projectId} tabId={tab.id} leafId={leaf.id} />}
          </div>
        );
      })}

      {visible &&
        !zoomed &&
        dividers.map(({ split, rect }) => {
          const ratio = drag && drag.splitId === split.id ? drag.ratio : split.ratio;
          const r = Math.min(0.9, Math.max(0.1, ratio));
          const style =
            split.dir === "row"
              ? { left: pct(rect.x + rect.w * r), top: pct(rect.y), height: pct(rect.h) }
              : { top: pct(rect.y + rect.h * r), left: pct(rect.x), width: pct(rect.w) };
          return (
            <div
              key={split.id}
              className="split-divider"
              data-dir={split.dir}
              data-dragging={drag?.splitId === split.id ? "true" : undefined}
              role="separator"
              aria-orientation={split.dir === "row" ? "vertical" : "horizontal"}
              aria-valuenow={Math.round(r * 100)}
              title="Drag to resize"
              style={style}
              onPointerDown={startDrag(split, rect)}
            />
          );
        })}

      {menu &&
        (() => {
          const leaf = findLeaf(tab.root, menu.leafId);
          if (!leaf) return null;
          return (
            <PaneMoveMenu
              at={menu.at}
              projectId={projectId}
              tabId={tab.id}
              source={leafSource(projectId, tab.id, leaf)}
              onClose={() => setMenu(null)}
            />
          );
        })()}
    </div>
  );
}

/** The half of a pane nearest a point — the side a dropped session takes. */
function nearestSide(fx: number, fy: number): Direction {
  const d: [Direction, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["up", fy],
    ["down", 1 - fy],
  ];
  d.sort((p, q) => p[1] - q[1]);
  return d[0]![0];
}

const HALF: Record<Direction, { left: string; top: string; width: string; height: string }> = {
  left: { left: "0", top: "0", width: "50%", height: "100%" },
  right: { left: "50%", top: "0", width: "50%", height: "100%" },
  up: { left: "0", top: "0", width: "100%", height: "50%" },
  down: { left: "0", top: "50%", width: "100%", height: "50%" },
};

/**
 * Drag-to-split. While a session or a tab is being dragged
 * (`session-drag.ts`), every visible pane gets a transparent layer over it —
 * xterm's canvas swallows drag events, so the pane itself cannot be the target
 * — and the half nearest the pointer is washed in the accent, which is exactly
 * where the session will land. Absent the rest of the time, so a file dropped
 * on Claude still reaches Claude.
 *
 * No zone on the dragged pane itself or inside the subtree being dragged (a
 * no-op), and on another server's pane a refusal with the reason beside it —
 * a project belongs to one server. dsh panes are native webviews drawn above
 * the page, so this layer is under them and they simply take no drop.
 */
function PaneDropZone({ projectId, tabId, leafId }: { projectId: ProjectId; tabId: string; leafId: string }) {
  const source = useSessionDrag();
  const layout = useWorkspace((s) => s.layout);
  const [side, setSide] = useState<Direction | null>(null);
  useEffect(() => {
    if (!source) setSide(null);
  }, [source]);
  const refusal = useMemo(
    () => (source ? useWorkspace.getState().moveRefusal(source, { projectId, tabId, leafId }) : null),
    // `layout` because the answer depends on where things are now.
    [source, projectId, tabId, leafId, layout],
  );
  if (!source) return null;
  return (
    <div
      className="pane-drop-layer"
      data-refused={refusal ?? undefined}
      onDragOver={(e) => {
        if (!isSessionDrag(e.dataTransfer)) return;
        e.preventDefault();
        if (refusal) {
          e.dataTransfer.dropEffect = "none";
          return;
        }
        e.dataTransfer.dropEffect = "move";
        const b = e.currentTarget.getBoundingClientRect();
        if (b.width <= 0 || b.height <= 0) return;
        const next = nearestSide((e.clientX - b.left) / b.width, (e.clientY - b.top) / b.height);
        setSide((d) => (d === next ? d : next));
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setSide(null);
      }}
      onDrop={(e) => {
        if (!isSessionDrag(e.dataTransfer)) return;
        e.preventDefault();
        e.stopPropagation();
        const src = currentSessionDrag();
        const b = e.currentTarget.getBoundingClientRect();
        const at = b.width > 0 && b.height > 0
          ? nearestSide((e.clientX - b.left) / b.width, (e.clientY - b.top) / b.height)
          : side;
        setSide(null);
        endSessionDrag();
        if (src && at && !refusal) useWorkspace.getState().moveBeside(src, { projectId, tabId, leafId }, at);
      }}
    >
      {!refusal && side && <div className="pane-drop-half" data-side={side} style={HALF[side]} />}
      {refusal === "server" && (
        <span className="pane-drop-hint micro">a session can’t move to another server</span>
      )}
    </div>
  );
}

/**
 * One tile of the project grid: that project's tab bar over an empty body the
 * project's active tab is drawn into (by the pane area, flat — see `SplitTab`).
 * A mousedown anywhere in it focuses the tile *before* the click lands, so a
 * pane tool pressed in another tile acts on that tile's pane, not this one's.
 */
function ProjectTile({
  tile,
  rect,
  focused,
  multi,
  hasTab,
}: {
  tile: ProjectLeaf;
  rect: Rect;
  focused: boolean;
  multi: boolean;
  hasTab: boolean;
}) {
  const project = useWorkspace((s) => s.projects.find((p) => p.id === tile.project));
  const selectProject = useWorkspace((s) => s.selectProject);
  const closeTile = useWorkspace((s) => s.closeTile);
  return (
    <div
      className="project-tile"
      data-tile-project={tile.project}
      data-focused={multi ? focused : undefined}
      style={{ left: pct(rect.x), top: pct(rect.y), width: pct(rect.w), height: pct(rect.h) }}
      onMouseDownCapture={() => {
        if (!focused) selectProject(tile.project);
      }}
    >
      <TabBar
        projectId={tile.project}
        focused={focused || !multi}
        onCloseTile={multi ? () => closeTile(tile.id) : undefined}
      />
      {!hasTab && (
        <div className="pane-empty">
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            {`no tabs in ${project?.label ?? "this project"} — + opens a terminal, or pick a session in the rail`}
          </span>
        </div>
      )}
    </div>
  );
}

/** A sidebar project being dragged — `WorkspaceRail` announces it, so the drop
 *  layer exists only while there is something to drop and never sits over a
 *  terminal otherwise (a screenshot dropped on Claude must still reach it). */
export const PROJECT_DRAG_EVENT = "rmux:project-drag";
export const PROJECT_DRAG_TYPE = "text/x-rmux-project";

/** Which edge of which tile a point is nearest — the drop zone under it. */
function dropZone(
  tiles: { tile: ProjectLeaf; rect: Rect }[],
  x: number,
  y: number,
): { tileId: string; side: Direction } | null {
  const hit = tiles.find(({ rect: r }) => x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h);
  if (!hit) return null;
  const r = hit.rect;
  const fx = (x - r.x) / r.w;
  const fy = (y - r.y) / r.h;
  const d: [Direction, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["up", fy],
    ["down", 1 - fy],
  ];
  d.sort((p, q) => p[1] - q[1]);
  return { tileId: hit.tile.id, side: d[0]![0] };
}

export function WorkspaceDeck() {
  const layout = useWorkspace((s) => s.layout);
  const recentTabs = useWorkspace((s) => s.recentTabs);
  const setViewRatio = useWorkspace((s) => s.setViewRatio);
  const placeProject = useWorkspace((s) => s.placeProject);

  const grid = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<RatioOverride>(null);
  const [projectDrag, setProjectDrag] = useState(false);
  const [drop, setDrop] = useState<{ tileId: string; side: Direction } | null>(null);

  useEffect(() => {
    const on = (e: Event) => {
      const active = !!(e as CustomEvent<boolean>).detail;
      setProjectDrag(active);
      if (!active) setDrop(null);
    };
    window.addEventListener(PROJECT_DRAG_EVENT, on);
    return () => window.removeEventListener(PROJECT_DRAG_EVENT, on);
  }, []);

  const tree = viewTree(layout);
  const tileRects = useMemo(() => (tree ? leafRects(tree, undefined, drag) : {}), [tree, drag]);
  const tiles = useMemo(
    () => (tree ? leaves(tree).map((tile) => ({ tile, rect: tileRects[tile.id] ?? UNIT })) : []),
    [tree, tileRects],
  );
  const viewDividers = useMemo(() => (tree ? splitRects(tree, undefined, drag).reverse() : []), [tree, drag]);
  const multi = tiles.length > 1;

  // Every tab, by id, with its project — warm tabs may belong to a project
  // that is in no tile.
  const allTabs = useMemo(() => {
    const out = new Map<string, { projectId: ProjectId; tab: Tab }>();
    for (const [projectId, pt] of Object.entries(layout.projects)) {
      for (const tab of pt.tabs) out.set(tab.id, { projectId, tab });
    }
    return out;
  }, [layout]);

  // On screen: each tile's active tab, in the tile's rectangle.
  const onScreen = useMemo(() => {
    const out = new Map<string, Rect>();
    for (const { tile, rect } of tiles) {
      const tab = activeTabOf(layout, tile.project);
      if (tab) out.set(tab.id, rect);
    }
    return out;
  }, [tiles, layout]);

  // Mounted: everything on screen, plus the warm set of recent tabs behind it.
  const at = activeTab(layout);
  const mounted = useMemo(() => {
    const warm = warmTabs(at?.tab.id ?? null, recentTabs, (id) => allTabs.has(id), WARM_TABS);
    return [...new Set([...onScreen.keys(), ...warm])];
  }, [at?.tab.id, recentTabs, allTabs, onScreen]);

  const startViewDrag = (split: SplitOf<ProjectLeaf>, rect: Rect) => (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const el = grid.current;
    if (!el) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    const bounds = el.getBoundingClientRect();
    const ratioAt = (ev: { clientX: number; clientY: number }) =>
      split.dir === "row"
        ? ((ev.clientX - bounds.left) / bounds.width - rect.x) / rect.w
        : ((ev.clientY - bounds.top) / bounds.height - rect.y) / rect.h;
    const target = e.currentTarget;
    let last = split.ratio;
    const move = (ev: PointerEvent) => {
      last = ratioAt(ev);
      setDrag({ splitId: split.id, ratio: last });
    };
    const up = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
      target.removeEventListener("pointercancel", up);
      setDrag(null);
      setViewRatio(split.id, last);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
    target.addEventListener("pointercancel", up);
    setDrag({ splitId: split.id, ratio: split.ratio });
  };

  const zoneAt = (e: { clientX: number; clientY: number }) => {
    const el = grid.current;
    if (!el) return null;
    const b = el.getBoundingClientRect();
    if (b.width <= 0 || b.height <= 0) return null;
    return dropZone(tiles, (e.clientX - b.left) / b.width, (e.clientY - b.top) / b.height);
  };
  const dropRect = drop ? tiles.find((t) => t.tile.id === drop.tileId)?.rect : undefined;

  return (
    <div className="pane-area">
      <div ref={grid} className="tile-grid" data-dragging={drag ? "true" : undefined} data-tiles={tiles.length}>
        {tiles.map(({ tile, rect }) => (
          <ProjectTile
            key={tile.id}
            tile={tile}
            rect={rect}
            focused={tile.project === layout.activeProject}
            multi={multi}
            hasTab={!!activeTabOf(layout, tile.project)}
          />
        ))}
        {mounted.map((id) => {
          const entry = allTabs.get(id);
          if (!entry) return null;
          return (
            <SplitTab
              key={id}
              projectId={entry.projectId}
              tab={entry.tab}
              frame={onScreen.get(id) ?? null}
              tileFocused={entry.projectId === layout.activeProject}
            />
          );
        })}
        {viewDividers.map(({ split, rect }) => {
          const ratio = drag && drag.splitId === split.id ? drag.ratio : split.ratio;
          const r = Math.min(0.9, Math.max(0.1, ratio));
          const style =
            split.dir === "row"
              ? { left: pct(rect.x + rect.w * r), top: pct(rect.y), height: pct(rect.h) }
              : { top: pct(rect.y + rect.h * r), left: pct(rect.x), width: pct(rect.w) };
          return (
            <div
              key={split.id}
              className="split-divider tile-divider"
              data-dir={split.dir}
              data-dragging={drag?.splitId === split.id ? "true" : undefined}
              role="separator"
              aria-orientation={split.dir === "row" ? "vertical" : "horizontal"}
              aria-valuenow={Math.round(r * 100)}
              title="Drag to resize the projects"
              style={style}
              onPointerDown={startViewDrag(split, rect)}
            />
          );
        })}
        {projectDrag && tiles.length > 0 && (
          <div
            className="tile-drop-layer"
            onDragOver={(e) => {
              if (!e.dataTransfer.types.includes(PROJECT_DRAG_TYPE)) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              const z = zoneAt(e);
              setDrop((d) => (d?.tileId === z?.tileId && d?.side === z?.side ? d : z));
            }}
            onDragLeave={(e) => {
              if (e.currentTarget === e.target) setDrop(null);
            }}
            onDrop={(e) => {
              e.preventDefault();
              const pid = e.dataTransfer.getData(PROJECT_DRAG_TYPE);
              const z = zoneAt(e);
              setDrop(null);
              setProjectDrag(false);
              if (pid && z) placeProject(pid, z.tileId, z.side);
            }}
          >
            {drop && dropRect && (
              <div
                className="tile-drop-line"
                data-side={drop.side}
                style={{
                  left: pct(drop.side === "right" ? dropRect.x + dropRect.w : dropRect.x),
                  top: pct(drop.side === "down" ? dropRect.y + dropRect.h : dropRect.y),
                  width: drop.side === "left" || drop.side === "right" ? undefined : pct(dropRect.w),
                  height: drop.side === "up" || drop.side === "down" ? undefined : pct(dropRect.h),
                }}
              />
            )}
          </div>
        )}
        {!tree && (
          <div className="pane-empty">
            <span className="micro" style={{ color: "var(--text-faint)" }}>
              nothing open — pick a session, or connect a server
            </span>
          </div>
        )}
      </div>
      <ConfirmClose />
    </div>
  );
}
