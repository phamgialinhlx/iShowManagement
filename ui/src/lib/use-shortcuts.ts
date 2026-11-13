import { useEffect } from "react";

import { activeTab, findLeaf, tileOf } from "./layout-tree.ts";
import {
  match,
  readBindings,
  requestClose,
  requestView,
  type ActionId,
  type Bindings,
} from "./shortcuts";
import { useWorkspace } from "./workspace";
import { prefixOwns } from "./prefix-runtime.ts";
import { requestQuickSwitch } from "./quick-switch.ts";
import { requestQuickOpen } from "./quick-open.ts";

/**
 * Bind the keyboard to the workbench.
 *
 * One listener for the whole app rather than one per pane — the same reasoning
 * as the attention watcher: a per-pane handler in a 4×4 fires sixteen times per
 * keystroke and every one of them has to work out whether it is the focused
 * pane. The single listener asks the store once.
 *
 * ## `keydown` on `window`, in the bubble phase
 *
 * Capture would beat xterm to the key, which sounds right and is not: it would
 * take the keystroke even when the shortcut is *unbound*, because deciding
 * requires running the match first and by then the terminal's own handler has
 * been skipped. Bubbling means xterm sees it, does its thing, and we only
 * intervene on a chord it does not use.
 *
 * `preventDefault` is called **only on a match**, for the same reason.
 */
export function useShortcuts(options: {
  /** Whether the Progress page is open, so its shortcut can close it. */
  progressOpen: boolean;
  onProgress: (open: boolean) => void;
}): void {
  const { progressOpen, onProgress } = options;

  useEffect(() => {
    let bindings: Bindings = readBindings();
    const reload = () => {
      bindings = readBindings();
    };
    window.addEventListener("rmux:shortcuts-changed", reload);
    window.addEventListener("storage", reload);

    const run = (action: ActionId) => {
      const state = useWorkspace.getState();
      const active = state.activeSession;

      switch (action) {
        case "view.claude":
        case "view.files":
        case "view.transcript":
        case "view.jira":
        case "view.git": {
          // Addressed to the *active session's* pane, which owns the state. A
          // pane that is not on screen ignores it, so nothing changes under a
          // tile the operator cannot see.
          if (active) requestView(active, action.slice("view.".length) as "claude");
          return;
        }

        case "session.terminal": {
          const session = state.sessions.find((s) => s.id === active);
          if (!session) return;
          // Reuse before create. Pressing this twice should return to the shell
          // you opened, not litter the rail with `sh 1`, `sh 2`, `sh 3` —
          // and each new one is a real process on a real host.
          const existing = state.sessions.find(
            (s) => s.kind === "terminal" && s.projectId === session.projectId,
          );
          state.openSession(existing ? existing.id : state.addSession(session.projectId, "terminal"));
          return;
        }

        case "pane.left":
        case "pane.right":
        case "pane.up":
        case "pane.down": {
          // Geometric, in the split tree: the pane actually beside this one,
          // not the next in some list. At an edge nothing moves — the store
          // treats "no neighbour" as a no-op, as the grid's clamp did. The
          // store also hands the landed terminal the keyboard: moving the ring
          // without the cursor is worse than not moving at all.
          state.focusDirection(action.slice("pane.".length) as "left");
          return;
        }

        case "pane.splitRight":
          state.splitPane("right");
          return;
        case "pane.splitDown":
          state.splitPane("down");
          return;

        case "pane.close": {
          // Never closed from the keyboard directly: a session pane's close
          // ends its work on the host, so it goes through the same confirmation
          // the pane's own ✕ does (`ConfirmClose`, mounted by the deck).
          const at = activeTab(state.layout);
          const leaf = at ? findLeaf(at.tab.root, at.tab.focus) : null;
          if (at && leaf) requestClose({ projectId: at.projectId, tabId: at.tab.id, leafId: leaf.id });
          return;
        }

        case "pane.zoom":
          state.toggleZoom();
          return;

        case "tab.new":
          state.newTab();
          return;
        case "tab.next":
          state.cycleTab(1);
          return;
        case "tab.prev":
          state.cycleTab(-1);
          return;

        case "progress":
          onProgress(!progressOpen);
          return;

        case "quickSwitch":
          requestQuickSwitch();
          return;

        case "quickOpen":
          requestQuickOpen();
          return;

        case "tile.openBeside":
          state.openNextBeside();
          return;
        case "tile.close": {
          // Removes the tile only — nothing ends, so nothing to confirm. With
          // one tile there is nowhere for the project to go: a no-op.
          const pid = state.layout.activeProject;
          const tile = pid && state.layout.view ? tileOf(state.layout, pid) : null;
          if (tile) state.closeTile(tile.id);
          return;
        }
      }
    };

    const onKey = (e: KeyboardEvent) => {
      // The prefix layer took this key, or a prefix/navigate/resize mode owns
      // the keyboard — a Mod chord must not also fire underneath it.
      if (prefixOwns(e)) return;
      const action = match(e, bindings);
      if (!action) return;
      e.preventDefault();
      e.stopPropagation();
      run(action);
    };

    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("rmux:shortcuts-changed", reload);
      window.removeEventListener("storage", reload);
    };
  }, [progressOpen, onProgress]);
}
