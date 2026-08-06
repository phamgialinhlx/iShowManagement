import { useMemo } from "react";

import { useWorkspace } from "../lib/workspace";
import { isFocus, layoutPanes } from "../lib/grid";
import { paneRefKey, type TabRef } from "../lib/workspace-model";
import { ClaudeMark, FilesMark, HostMark, TerminalMark, serverLabel } from "./WorkspaceRail";

/**
 * The tab bar — the workbench's "what is open" axis (the deck grid is "where
 * it shows"). Modeled on meowork's, which the operator uses on instinct: one
 * chip per open resource in creation order, deduped by identity; clicking a
 * chip reveals it in the focused tile (or focuses the tile already showing
 * it); the ✕ closes the tab and **never** the session — killing lives in the
 * rail behind a confirm, and nowhere else. Hidden while nothing is open, like
 * meowork's; the empty deck says what to do instead.
 */
export function TabBar() {
  const tabs = useWorkspace((s) => s.tabs);
  const panes = useWorkspace((s) => s.panes);
  const sessions = useWorkspace((s) => s.sessions);
  const projects = useWorkspace((s) => s.projects);
  const servers = useWorkspace((s) => s.servers);
  const active = useWorkspace((s) => s.activeSession);
  const grid = useWorkspace((s) => s.grid);
  const focusedCell = useWorkspace((s) => s.focusedCell);
  // Safe to subscribe: setStatus bails without a new object when nothing
  // changed, so this only re-renders on a real status transition.
  const runtime = useWorkspace((s) => s.runtime);
  const revealTab = useWorkspace((s) => s.revealTab);
  const closeTab = useWorkspace((s) => s.closeTab);

  const cells = useMemo(
    () => layoutPanes(panes, tabs, sessions, active, grid),
    [panes, tabs, sessions, active, grid],
  );
  /** First cell showing each identity — the superscript in the chip. */
  const cellOf = useMemo(() => {
    const m = new Map<string, number>();
    cells.forEach((p, i) => {
      if (p && p.kind !== "empty") {
        const key = paneRefKey(p);
        if (!m.has(key)) m.set(key, i);
      }
    });
    return m;
  }, [cells]);

  if (tabs.length === 0) return null;

  const titleOf = (t: TabRef): string | null => {
    if (t.kind === "session") return sessions.find((s) => s.id === t.id)?.name ?? null;
    if (t.kind === "host") {
      const server = servers.find((s) => s.id === t.serverId);
      return server ? serverLabel(server) : null;
    }
    return projects.find((p) => p.id === t.projectId)?.label ?? null;
  };

  const glyphOf = (t: TabRef) => {
    if (t.kind === "host") return <HostMark />;
    if (t.kind === "files") return <FilesMark />;
    const session = sessions.find((s) => s.id === t.id);
    if (session?.kind !== "claude") return <TerminalMark />;
    // The glyph carries the session's attention state, same palette as the rail.
    const status = runtime[t.id]?.status;
    const color =
      status === "waiting"
        ? "rgb(var(--primary))"
        : status === "working"
          ? "rgb(var(--busy))"
          : "var(--text-soft)";
    return <ClaudeMark color={color} />;
  };

  return (
    <div
      className="flex shrink-0 items-end gap-[2px] overflow-x-auto border-b px-1 pt-1"
      style={{ borderColor: "var(--border)" }}
    >
      {tabs.map((t) => {
        const title = titleOf(t);
        // Entity vanished — the store strips these; skipping is belt-and-braces.
        if (title === null) return null;
        const key = paneRefKey(t);
        const shownAt = cellOf.get(key) ?? -1;
        const visible = shownAt !== -1;
        const focused = visible && (isFocus(grid) ? shownAt === 0 : shownAt === focusedCell);
        return (
          <button
            key={key}
            type="button"
            onClick={() => revealTab(t)}
            title={title}
            className="group flex max-w-[180px] shrink-0 items-center gap-1.5 px-2 py-[4px]"
            style={{
              background: focused
                ? "var(--hover)"
                : visible
                  ? "color-mix(in srgb, var(--text) 4%, transparent)"
                  : "transparent",
              color: focused ? "var(--text)" : visible ? "var(--text-soft)" : "var(--text-faint)",
              boxShadow: focused ? "inset 0 2px 0 var(--text)" : "none",
            }}
          >
            {glyphOf(t)}
            <span className="data min-w-0 flex-1 truncate text-[11px]">{title}</span>
            {visible && !isFocus(grid) && (
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                {shownAt + 1}
              </span>
            )}
            {/* span, not button: a button may not nest inside a button. */}
            <span
              role="button"
              aria-label={`Close tab ${title}`}
              title="Close tab (the session keeps running — end it from the rail)"
              className="invisible shrink-0 group-hover:visible"
              style={{ color: "var(--text-faint)" }}
              onClick={(e) => {
                e.stopPropagation();
                closeTab(t);
              }}
            >
              <svg
                width="9"
                height="9"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2.5"
                strokeLinecap="square"
                aria-hidden="true"
              >
                <path d="M18 6L6 18M6 6l12 12" />
              </svg>
            </span>
          </button>
        );
      })}
    </div>
  );
}
