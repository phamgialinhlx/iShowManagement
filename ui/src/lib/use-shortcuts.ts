import { useEffect } from "react";

import { deckCells } from "./grid";
import {
  match,
  moveFocus,
  readBindings,
  requestView,
  type ActionId,
  type Bindings,
} from "./shortcuts";
import { useWorkspace } from "./workspace";

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
        case "view.jira": {
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
          const { deck, panes } = state;
          if (deckCells(deck) <= 1) return; // nowhere to go in focus mode
          const from = state.focusedCell ?? 0;
          const to = moveFocus(from, action.slice("pane.".length) as "left", deck.cols, deck.rows);
          if (to === from) return;
          state.focusCell(to);
          // Follow the focus with the session, so the rail, the widgets and the
          // status all describe the tile being looked at. An empty tile is a
          // real destination — you move into it to open something there — so it
          // takes the focus and simply leaves the active session alone.
          const landed = panes[to];
          if (landed?.kind === "session") state.activate(landed.id);
          return;
        }

        case "progress":
          onProgress(!progressOpen);
          return;
      }
    };

    const onKey = (e: KeyboardEvent) => {
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
