/**
 * The window's drag handle.
 *
 * The window is `transparent` with `titleBarStyle: "Overlay"` and a hidden title,
 * which is what lets the desktop show through the glass — but it also means the
 * OS gives us no title bar to grab. Without an element carrying
 * `data-tauri-drag-region`, the window cannot be moved at all.
 *
 * Two details are easy to get wrong and both produce a window that silently
 * refuses to move:
 *
 * 1. `plugin:window|start_dragging` is a **plugin** command, so it is always
 *    ACL-checked. `core:default` does *not* include it — it must be granted
 *    explicitly in `capabilities/default.json`. A missing grant rejects the call
 *    with no visible error. `tests/window_drag.rs` guards this.
 * 2. A **bare** `data-tauri-drag-region` only drags on *direct* clicks on that
 *    exact element, so any child (a label, an icon) becomes a dead spot. `"deep"`
 *    makes the whole subtree draggable, which is what a title bar wants.
 *    Genuinely clickable children (buttons, links, inputs) still block dragging
 *    automatically, so they keep working without extra markup.
 *
 * The band is deliberately generous — this is the only way to move the window, so
 * a thin strip is frustrating to hit. The left padding clears the macOS traffic
 * lights, which float over the content in this style.
 */

/** Height of the drag band. Exported so layouts can offset content by it. */
export const TITLE_BAR_HEIGHT = 56;

export function TitleBar({ children }: { children?: React.ReactNode }) {
  const isMac = navigator.userAgent.includes("Mac");

  return (
    <div
      data-tauri-drag-region="deep"
      className="fixed inset-x-0 top-0 z-50 flex items-center justify-between pr-4"
      style={{ height: TITLE_BAR_HEIGHT, paddingLeft: isMac ? 82 : 16 }}
    >
      <span className="micro select-none">rmux</span>
      {children}
    </div>
  );
}
