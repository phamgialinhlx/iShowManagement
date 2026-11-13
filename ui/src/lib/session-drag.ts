import { useSyncExternalStore } from "react";

import type { MoveSource } from "./layout-tree.ts";

/**
 * Drag a session (a sidebar row, or a tab) onto a pane to split it.
 *
 * **A MIME type of our own**, so nothing else that listens for drops mistakes
 * this for its payload: the Claude pane takes dropped *files* (an image paste)
 * and the file tree takes uploads, and both test for `Files` — a session drag
 * carries none, and a file drag carries no `application/x-rmux-session`.
 *
 * **The source is also held here**, because `dataTransfer.getData` is empty
 * during `dragover` (only `types` is readable until the drop), and the pane
 * overlay must decide *while hovering* whether to show a zone at all — a drop
 * on itself shows none, a drop onto another server is refused.
 *
 * The overlay exists only while `useSessionDrag()` is non-null, so it never sits
 * over a terminal that a file might be dropped on.
 */
export const SESSION_DRAG_TYPE = "application/x-rmux-session";

let current: MoveSource | null = null;
const subs = new Set<() => void>();
const notify = () => subs.forEach((f) => f());

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") endSessionDrag();
}
// Backstops: the drag source may have been unmounted by the move itself (a
// moved tab's element is gone), in which case its own `dragend` never fires.
function onEnd() {
  // After the target's own `drop` handler has read the source.
  setTimeout(endSessionDrag, 0);
}

export function beginSessionDrag(dt: DataTransfer, source: MoveSource): void {
  dt.setData(SESSION_DRAG_TYPE, JSON.stringify(source));
  dt.effectAllowed = "move";
  current = source;
  if (typeof window !== "undefined") {
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("dragend", onEnd, true);
    window.addEventListener("drop", onEnd, true);
  }
  // Deferred a tick: changing the DOM under the pointer inside `dragstart`
  // can make the browser abort the drag before it starts.
  setTimeout(() => {
    if (current === source) notify();
  }, 0);
}

export function endSessionDrag(): void {
  if (typeof window !== "undefined") {
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("dragend", onEnd, true);
    window.removeEventListener("drop", onEnd, true);
  }
  if (!current) return;
  current = null;
  notify();
}

export function currentSessionDrag(): MoveSource | null {
  return current;
}

/** Is this drag event one of ours? */
export function isSessionDrag(dt: DataTransfer | null): boolean {
  return !!dt && dt.types.includes(SESSION_DRAG_TYPE) && !!current;
}

const subscribe = (f: () => void) => {
  subs.add(f);
  return () => void subs.delete(f);
};

export function useSessionDrag(): MoveSource | null {
  return useSyncExternalStore(subscribe, currentSessionDrag, () => null);
}
