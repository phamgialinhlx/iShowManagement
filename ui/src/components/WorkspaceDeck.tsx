import { useMemo } from "react";

import { useWorkspace } from "../lib/workspace";
import { gridDims, isFocus, layoutPanes, warmSessions } from "../lib/grid";
import { reattachName, type PaneRef, type SessionV3 } from "../lib/workspace-model";
import { ClaudeSessionPane } from "./ClaudeSessionPane";
import { TerminalView } from "./Terminal";
import { HostPanel } from "./HostPanel";
import { FilesPane } from "./FilesPane";

/**
 * The workbench main area, v3 — a **pane manager** (ADR-002).
 *
 * The grid tiles Panes, and a Pane holds a *content source*: a session (a Claude
 * TUI or a shell), a Server's host panel, or a Project's files. Replaces
 * `SessionDeck`, whose cells could only hold sessions.
 *
 * Focus mode (1×1) shows a single tile; the other presets (1×2, 1×3, 2×2, 3×3,
 * 4×4) tile several. Clicking a tile focuses it, so the rail's "open into the
 * focused tile" verb has a target.
 */

function paneKey(ref: PaneRef | null, index: number): string {
  if (!ref || ref.kind === "empty") return `empty-${index}`;
  if (ref.kind === "session") return `session:${ref.id}`;
  if (ref.kind === "host") return `host:${ref.serverId}`;
  return `files:${ref.projectId}`;
}

/** A single session pane — Claude (with its sub-tabs) or a shell, by kind. */
function SessionPane({ session }: { session: SessionV3 }) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const live = useWorkspace((s) => s.live[session.id]);
  const setLive = useWorkspace((s) => s.setLive);
  const clearLive = useWorkspace((s) => s.clearLive);

  if (session.kind === "claude") {
    return <ClaudeSessionPane session={session} />;
  }

  return (
    <TerminalView
      target={target}
      cwd={project?.folder}
      session={reattachName(session)}
      ptyId={live}
      onOpened={(id) => setLive(session.id, id)}
      onExit={() => clearLive(session.id)}
    />
  );
}

/** A server's host pane — processes, ports, metrics. */
function HostPane({ serverId }: { serverId: string }) {
  const server = useWorkspace((s) => s.servers.find((sv) => sv.id === serverId));
  if (!server) return <PaneMissing what="server" />;
  return <HostPanel target={server.target} />;
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

function Pane({ pane }: { pane: PaneRef }) {
  const session = useWorkspace((s) =>
    pane.kind === "session" ? s.sessions.find((x) => x.id === pane.id) : undefined,
  );
  if (pane.kind === "session") {
    return session ? <SessionPane session={session} /> : <PaneMissing what="session" />;
  }
  if (pane.kind === "host") return <HostPane serverId={pane.serverId} />;
  if (pane.kind === "files") return <FilesPane projectId={pane.projectId} />;
  // kind "empty" — the deck renders the placeholder itself; nothing mounts here.
  return null;
}

/** How many sessions stay mounted in focus mode. Each warm pane holds an xterm
 *  (and a WebGL context when GPU rendering is on), so this is deliberately
 *  small — enough to make alternating between a few sessions instant. */
const KEEP_WARM = 4;

export function WorkspaceDeck() {
  const sessions = useWorkspace((s) => s.sessions);
  const panes = useWorkspace((s) => s.panes);
  const tabs = useWorkspace((s) => s.tabs);
  const active = useWorkspace((s) => s.activeSession);
  const recent = useWorkspace((s) => s.recentSessions);
  const grid = useWorkspace((s) => s.grid);
  const focusedCell = useWorkspace((s) => s.focusedCell);
  const focusCell = useWorkspace((s) => s.focusCell);
  const activate = useWorkspace((s) => s.activate);
  const assignPane = useWorkspace((s) => s.assignPane);

  const cells = useMemo(
    () => layoutPanes(panes, tabs, sessions, active, grid),
    [panes, tabs, sessions, active, grid],
  );

  // The focus-mode warm set. The visible pane is prepended so an explicitly
  // assigned cell 0 that differs from the active session is always mounted.
  // An explicitly cleared tile counts as nothing open — focus mode must show
  // its "pick a session" state, never a blank pane.
  const first = cells[0] ?? null;
  const only = first && first.kind === "empty" ? null : first;
  const visibleSession = isFocus(grid) && only?.kind === "session" ? only.id : null;
  const warm = useMemo(() => {
    if (!isFocus(grid)) return [];
    const order = visibleSession ? [visibleSession, ...recent] : recent;
    // Only tabbed sessions may stay warm: a session whose tab was closed must
    // not keep an xterm and a WebGL context mounted behind display:none.
    const tabbed = new Set(tabs.flatMap((t) => (t.kind === "session" ? [t.id] : [])));
    return warmSessions(active, order, sessions.filter((s) => tabbed.has(s.id)), KEEP_WARM);
  }, [grid, visibleSession, active, recent, sessions, tabs]);

  if (!isFocus(grid)) {
    const { rows, cols } = gridDims(grid);
    return (
      <div
        className="grid min-h-0 flex-1 gap-[1px]"
        style={{
          gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`,
          gridTemplateRows: `repeat(${rows}, minmax(0, 1fr))`,
        }}
      >
        {cells.map((pane, index) => (
          <div
            key={paneKey(pane, index)}
            className="panel relative flex min-h-0 flex-col overflow-hidden"
            onMouseDownCapture={(e) => {
              // The clear control must not focus-and-activate the tile it is
              // clearing (capture runs parent-first, so the guard lives here).
              if ((e.target as HTMLElement).closest("[data-pane-clear]")) return;
              focusCell(index);
              if (pane?.kind === "session") activate(pane.id);
            }}
            style={{
              outline:
                focusedCell === index
                  ? "2px solid var(--text)"
                  : pane?.kind === "session" && pane.id === active
                    ? "1px solid color-mix(in srgb, var(--text) 45%, transparent)"
                    : "1px solid transparent",
              outlineOffset: -1,
              background: pane ? undefined : "var(--app-bg)",
              // Scope layout and paint to the cell: a rail animating its width
              // reflows the deck every frame, and without containment that
              // reflow walks into all sixteen terminal subtrees.
              contain: "layout paint",
            }}
          >
            {pane && pane.kind !== "empty" ? (
              <>
                <Pane pane={pane} />
                <button
                  type="button"
                  data-pane-clear
                  aria-label="Hide from this tile"
                  title="Hide from this tile (tab stays open — does not end the session)"
                  className="pane-clear absolute right-1 top-1 z-10"
                  onClick={() => assignPane(index, { kind: "empty" })}
                >
                  <svg
                    width="11"
                    height="11"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="square"
                    aria-hidden="true"
                  >
                    <path d="M18 6L6 18M6 6l12 12" />
                  </svg>
                </button>
              </>
            ) : (
              <div className="grid h-full place-items-center">
                <span className="micro" style={{ color: "var(--text-faint)" }}>
                  EMPTY — CLICK A TAB, OR A NODE IN THE RAIL
                </span>
              </div>
            )}
          </div>
        ))}
      </div>
    );
  }

  // Focus mode: one pane visible, the warm set mounted behind it.
  //
  // Switching sessions used to remount the pane (a `key` per pane), disposing
  // the xterm and replaying scrollback over SSH on every switch — the slow
  // path, taken every time. Now the warm sessions stay mounted and hidden with
  // `display:none`: xterm stops painting a hidden terminal on its own (its
  // renderer watches intersection), the 0×0 guard keeps a hidden pane from
  // resizing the far side, and switching back is a display toggle — instant,
  // with scrollback and selection intact. Keys are session identity, so the
  // set reordering never remounts a member. Host and files panes still swap by
  // key; they hold no terminal, so a remount is cheap.
  //
  // Each wrapper carries `flex-1` because this parent is a flex *row*: without
  // it the pane sizes to its content width, not the deck.
  return (
    <div className="flex min-h-0 flex-1">
      {warm.map((session) => (
        <div
          key={`session:${session.id}`}
          className="relative min-h-0 flex-1 flex-col overflow-hidden"
          style={{
            display: session.id === visibleSession ? "flex" : "none",
            contain: "layout paint",
          }}
        >
          <SessionPane session={session} />
        </div>
      ))}
      {only && only.kind !== "session" && (
        <div
          key={paneKey(only, 0)}
          className="relative flex min-h-0 flex-1 flex-col overflow-hidden"
        >
          <Pane pane={only} />
        </div>
      )}
      {/* A cell holding a session that no longer exists — the warm set drops
          stale ids, so say so instead of showing nothing at all. */}
      {visibleSession && !warm.some((s) => s.id === visibleSession) && (
        <div className="flex min-h-0 flex-1 flex-col">
          <PaneMissing what="session" />
        </div>
      )}
      {!only && (
        <div className="grid h-full flex-1 place-items-center">
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            nothing open — connect a server, then + shell
          </span>
        </div>
      )}
    </div>
  );
}
