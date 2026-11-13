import { useEffect, useRef, useState } from "react";

import { leaves, tabLabel, tabsOf, type Direction } from "../lib/layout-tree";
import { label as chordLabel, readBindings, requestClose, type ActionId } from "../lib/shortcuts";
import { useWorkspace } from "../lib/workspace";
import { beginSessionDrag, currentSessionDrag, endSessionDrag } from "../lib/session-drag";
import { MenuItem, MenuSurface, type MenuAt } from "./Menu";

/**
 * The tab bar over the pane area, herdr style.
 *
 * The focused tab is the one loud thing — accent ground, dark text — and the
 * rest are muted; a tab the operator named is bold, one named by its focused
 * session is not, so the strip says which labels will stay put. ` + ` makes a
 * tab (a new terminal in this project); double-click renames inline; drag
 * along the strip reorders, and drag onto a pane splits that pane with this
 * tab beside it (`session-drag.ts`); the strip scrolls sideways rather than wrapping, because a second
 * row of tabs would take a row from every terminal under it.
 *
 * The pane tools on the right act on the focused pane — the mouse route to
 * what the shortcuts do, with the chord in each tooltip so the keyboard route is
 * discoverable from the control itself.
 *
 * **One per tile.** With a project grid each tile carries its own project's
 * bar; only the focused tile's active tab wears the accent, so "which tile am I
 * in" is answered by the one loud element on screen. The pane tools act on the
 * focused pane — pressing one in another tile works because the tile's
 * mousedown has already focused it (`ProjectTile`).
 */
export function TabBar({
  projectId: pid,
  focused = true,
  onCloseTile,
}: {
  projectId: string;
  /** This tile holds the keyboard. A single tile always does. */
  focused?: boolean;
  /** Present only when there is more than one tile — the last one has nowhere
   *  to go, so it gets no ✕ rather than a dead one. */
  onCloseTile?: () => void;
}) {
  const layout = useWorkspace((s) => s.layout);
  const sessions = useWorkspace((s) => s.sessions);
  const projects = useWorkspace((s) => s.projects);
  const selectTab = useWorkspace((s) => s.selectTab);
  const renameTab = useWorkspace((s) => s.renameTab);
  const moveTab = useWorkspace((s) => s.moveTab);
  const newTab = useWorkspace((s) => s.newTab);
  const splitPane = useWorkspace((s) => s.splitPane);
  const toggleZoom = useWorkspace((s) => s.toggleZoom);
  const swapPane = useWorkspace((s) => s.swapPane);

  const [editing, setEditing] = useState<string | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null);
  const [swapMenu, setSwapMenu] = useState<MenuAt | null>(null);
  const strip = useRef<HTMLDivElement>(null);

  const project = projects.find((p) => p.id === pid);
  const { tabs, activeTab: activeId } = tabsOf(layout, pid);
  const activeOne = tabs.find((t) => t.id === activeId);
  const at = activeOne ? { projectId: pid, tab: activeOne } : null;
  const paneCount = at ? leaves(at.tab.root).length : 0;
  const nameOf = (id: string) => sessions.find((s) => s.id === id)?.name;
  const bindings = readBindings();
  const hint = (id: ActionId) => (bindings[id] ? ` (${chordLabel(bindings[id])})` : "");

  // The active tab scrolls into view, so a tab chosen from the rail or by a
  // shortcut is never selected off the edge of the strip.
  useEffect(() => {
    const el = strip.current?.querySelector<HTMLElement>('[data-active="true"]');
    el?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [activeId, pid]);

  return (
    <div className="tab-bar" data-focused={focused}>
      <div className="tab-strip" ref={strip}>
        {tabs.map((tab, i) => {
          const active = tab.id === activeId;
          const text = tabLabel(tab, i, nameOf);
          return (
            <div
              key={tab.id}
              className="tab"
              data-active={active}
              data-renamed={!!tab.renamed}
              data-drop={dropAt === i}
              draggable={editing !== tab.id}
              title={`${text}${tab.zoomed ? " — zoomed" : ""}\nDouble-click to rename · drag to reorder, or onto a pane to split it`}
              // On click, not mousedown: a press that becomes a drag must not
              // switch to the dragged tab first, or the only pane left on
              // screen is the one being dragged and there is nowhere to drop it.
              // A click is not fired after a drag, so a plain press still selects.
              onClick={(e) => {
                if (e.button === 0 && !active) selectTab(pid, tab.id);
              }}
              onDoubleClick={() => setEditing(tab.id)}
              onDragStart={(e) => {
                e.dataTransfer.setData("text/x-rmux-tab", String(i));
                // The same drag can land on a pane, where it splits that pane
                // with this tab's panes beside it.
                beginSessionDrag(e.dataTransfer, { kind: "tab", projectId: pid, tabId: tab.id });
              }}
              onDragOver={(e) => {
                if (!e.dataTransfer.types.includes("text/x-rmux-tab")) return;
                // Reorder within this project's strip only — an index from
                // another tile's strip names a different tab here.
                const src = currentSessionDrag();
                if (src?.kind === "tab" && src.projectId !== pid) return;
                e.preventDefault();
                setDropAt(i);
              }}
              onDragLeave={() => setDropAt((d) => (d === i ? null : d))}
              onDrop={(e) => {
                const from = Number(e.dataTransfer.getData("text/x-rmux-tab"));
                setDropAt(null);
                endSessionDrag();
                if (Number.isInteger(from)) moveTab(pid, from, i);
              }}
              onDragEnd={() => {
                setDropAt(null);
                endSessionDrag();
              }}
            >
              {editing === tab.id ? (
                <input
                  autoFocus
                  className="tab-rename"
                  defaultValue={tab.renamed ? tab.name : ""}
                  placeholder={text}
                  spellCheck={false}
                  onMouseDown={(e) => e.stopPropagation()}
                  onClick={(e) => e.stopPropagation()}
                  onBlur={(e) => {
                    renameTab(pid, tab.id, e.currentTarget.value);
                    setEditing(null);
                  }}
                  onKeyDown={(e) => {
                    // Enter commits through blur, so there is one path that
                    // writes; Escape leaves without one.
                    if (e.key === "Enter") e.currentTarget.blur();
                    if (e.key === "Escape") setEditing(null);
                  }}
                />
              ) : (
                <span className="tab-label">
                  {text}
                  {tab.zoomed ? " Z" : ""}
                </span>
              )}
              <button
                type="button"
                className="tab-close"
                aria-label={`Close tab ${text}`}
                title="Close this tab — asks first if it ends a session"
                onMouseDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  requestClose({ projectId: pid, tabId: tab.id });
                }}
              >
                <svg width="7" height="7" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="square" aria-hidden="true">
                  <path d="M18 6L6 18M6 6l12 12" />
                </svg>
              </button>
            </div>
          );
        })}
        <button
          type="button"
          className="tab-add"
          aria-label={`New tab in ${project?.label ?? "this project"}`}
          title={`New tab — a terminal in ${project?.label ?? "this project"}${hint("tab.new")}`}
          onClick={() => newTab(pid)}
        >
          +
        </button>
      </div>

      {at && (
        <div className="pane-tools">
          <button
            type="button"
            className="pane-tool"
            aria-label="Split right"
            title={`Split right — a new terminal beside this pane${hint("pane.splitRight")}`}
            onClick={() => splitPane("right")}
          >
            <PaneGlyph d="M4 4h16v16H4zM12 4v16" />
          </button>
          <button
            type="button"
            className="pane-tool"
            aria-label="Split down"
            title={`Split down — a new terminal below this pane${hint("pane.splitDown")}`}
            onClick={() => splitPane("down")}
          >
            <PaneGlyph d="M4 4h16v16H4zM4 12h16" />
          </button>
          <button
            type="button"
            className="pane-tool"
            aria-label={at.tab.zoomed ? "Unzoom" : "Zoom pane"}
            aria-pressed={!!at.tab.zoomed}
            disabled={paneCount < 2 && !at.tab.zoomed}
            title={`${at.tab.zoomed ? "Show every pane again" : "Zoom — this pane fills the tab"}${hint("pane.zoom")}`}
            onClick={toggleZoom}
          >
            <PaneGlyph d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />
          </button>
          <button
            type="button"
            className="pane-tool"
            aria-label="Swap pane"
            disabled={paneCount < 2}
            title="Swap this pane with a neighbour"
            onClick={(e) => setSwapMenu({ x: e.clientX, y: e.clientY })}
          >
            <PaneGlyph d="M7 7h13l-4-4M17 17H4l4 4" />
          </button>
          <button
            type="button"
            className="pane-tool"
            aria-label="Close pane"
            title={`Close this pane — asks first if it ends a session${hint("pane.close")}`}
            onClick={() => requestClose({ projectId: at.projectId, tabId: at.tab.id, leafId: at.tab.focus })}
          >
            <PaneGlyph d="M18 6L6 18M6 6l12 12" />
          </button>
        </div>
      )}

      {onCloseTile && (
        <button
          type="button"
          className="pane-tool tile-close"
          aria-label={`Close the ${project?.label ?? "project"} tile`}
          title={`Close this tile — ${project?.label ?? "the project"} leaves the screen; its sessions keep running`}
          onClick={onCloseTile}
        >
          <PaneGlyph d="M4 4h16v16H4zM9 12h6" />
        </button>
      )}

      {swapMenu && (
        <MenuSurface at={swapMenu} onClose={() => setSwapMenu(null)} minWidth={150}>
          {(["left", "right", "up", "down"] as Direction[]).map((d) => (
            <MenuItem
              key={d}
              label={`Swap ${d}`}
              onClick={() => {
                setSwapMenu(null);
                swapPane(d);
              }}
            />
          ))}
        </MenuSurface>
      )}
    </div>
  );
}

function PaneGlyph({ d }: { d: string }) {
  return (
    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true">
      <path d={d} />
    </svg>
  );
}
