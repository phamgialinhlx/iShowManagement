import { createContext, useContext, useMemo, useRef, type DragEvent, type MouseEvent, type ReactNode } from "react";

import type { MoveSource } from "../lib/layout-tree";
import { beginSessionDrag, endSessionDrag } from "../lib/session-drag";
import type { MenuAt } from "./Menu";

/**
 * A pane's header is the handle for moving that pane — drag it onto another
 * pane's half, onto a tab, or onto `+` (`session-drag.ts`); right-click it for
 * "Move to new tab" / "Move to tab". `SplitTab` provides what the pane is; a
 * header reads it here, so the four kinds of pane header need no props for it
 * and a pane drawn outside a tab (the mobile bundle) simply gets no handle.
 */
type PaneDragValue = { source: MoveSource; openMenu: (at: MenuAt) => void };

const PaneDragContext = createContext<PaneDragValue | null>(null);

export function PaneDragProvider({
  source,
  openMenu,
  children,
}: {
  source: MoveSource;
  openMenu: (at: MenuAt) => void;
  children: ReactNode;
}) {
  // Keyed on the source's fields, not its identity, so a re-render of the tab
  // does not re-render every pane under it.
  const key = JSON.stringify(source);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const value = useMemo(() => ({ source, openMenu }), [key, openMenu]);
  return <PaneDragContext.Provider value={value}>{children}</PaneDragContext.Provider>;
}

/** Controls inside a header keep their own press; a drag starts only from the
 *  name or the blank part of the bar. */
const INTERACTIVE = "button, a, input, textarea, select, [role='button'], [contenteditable='true']";

/**
 * Props for a pane header element. Empty outside a tab.
 *
 * - **Only from the name or blank space.** The press origin is recorded on
 *   mousedown and a drag that began on a button is cancelled — `dragstart`'s
 *   own target is the draggable header, not what was under the pointer.
 * - The mousedown is **not** stopped: the pane's focus-on-mousedown rule still
 *   runs, so pressing the header still gives the pane the keyboard.
 * - It is the header, never the terminal, that is draggable — a drag inside the
 *   terminal still selects text.
 */
export function usePaneHeaderDrag(): Record<string, unknown> {
  const ctx = useContext(PaneDragContext);
  const origin = useRef<Element | null>(null);
  if (!ctx) return {};
  return {
    draggable: true,
    "data-pane-header": "",
    onMouseDown: (e: MouseEvent<HTMLElement>) => {
      origin.current = e.target as Element;
    },
    onDragStart: (e: DragEvent<HTMLElement>) => {
      const from = origin.current;
      origin.current = null;
      if (from && from !== e.currentTarget && from.closest?.(INTERACTIVE)) {
        e.preventDefault();
        return;
      }
      beginSessionDrag(e.dataTransfer, ctx.source);
    },
    onDragEnd: () => endSessionDrag(),
    onContextMenu: (e: MouseEvent<HTMLElement>) => {
      if ((e.target as Element).closest?.("input, textarea")) return;
      e.preventDefault();
      ctx.openMenu({ x: e.clientX, y: e.clientY });
    },
  };
}
