import { useEffect, useRef } from "react";

import { useAlerts } from "./alerts";
import { api } from "./api";
import { activeTab, focusedLeaf, leaves, tileOf } from "./layout-tree.ts";
import { resizeStep, wrapIndex, type Effect, type PrefixActionId } from "./prefix-mode.ts";
import {
  installWindowPrefix,
  prefixToast,
  setFocusedSessionSource,
  setPrefixDispatcher,
  usePrefixMode,
} from "./prefix-runtime.ts";
import { requestClose } from "./shortcuts";
import { requestQuickSwitch } from "./quick-switch.ts";
import { useWorkspace } from "./workspace";

/**
 * The spaces (projects) in sidebar order: servers in their order, each one's
 * projects in store order — what the rail draws. Navigate mode walks this list.
 */
export function spaceOrder(): string[] {
  const s = useWorkspace.getState();
  return s.servers.flatMap((sv) => s.projects.filter((p) => p.serverId === sv.id).map((p) => p.id));
}

/**
 * Bind the prefix layer's effects to the workbench store. Mounted once, by the
 * workbench, beside `useShortcuts`.
 */
export function usePrefixKeys(options: {
  /** Open the existing new-project flow for a server (or connect one). */
  onNewProject: (serverId: string | null) => void;
}): void {
  const onNewProject = useRef(options.onNewProject);
  onNewProject.current = options.onNewProject;

  useEffect(() => {
    setFocusedSessionSource(() => {
      const leaf = focusedLeaf(useWorkspace.getState().layout);
      return leaf?.pane.kind === "session" ? leaf.pane.id : null;
    });

    const run = (action: PrefixActionId) => {
      const state = useWorkspace.getState();
      const at = activeTab(state.layout);
      const leaf = at ? focusedLeaf(state.layout) : null;
      const pid = at?.projectId ?? state.layout.activeProject;

      switch (action) {
        case "help":
          usePrefixMode.setState({ helpOpen: true });
          return;
        case "settings":
          void api.openSettings().catch(() => prefixToast("settings open in the desktop app"));
          return;
        case "sidebar":
          state.toggleRail();
          return;
        case "jumpLatest": {
          // The newest outstanding alert first — that is "the latest
          // notification" — then anything waiting that raised none.
          const alerts = [...useAlerts.getState().alerts].sort((a, b) => b.at - a.at);
          const known = (id: string) => state.sessions.some((s) => s.id === id);
          const alert = alerts.find((a) => known(a.sessionId));
          const waiting = state.sessions.find((s) => state.runtime[s.id]?.status === "waiting");
          const id = alert?.sessionId ?? waiting?.id;
          if (!id) {
            prefixToast("nothing waiting");
            return;
          }
          state.activate(id);
          return;
        }
        case "project.new": {
          const project = state.projects.find((p) => p.id === pid);
          onNewProject.current(project?.serverId ?? state.servers[0]?.id ?? null);
          return;
        }
        case "project.rename": {
          const project = state.projects.find((p) => p.id === pid);
          if (!project) return prefixToast("no project");
          usePrefixMode.setState({
            dialog: { kind: "rename", what: "project", id: project.id, value: project.label },
          });
          return;
        }
        case "project.close": {
          const project = state.projects.find((p) => p.id === pid);
          if (!project) return prefixToast("no project");
          // Nothing on the host ends → nothing to ask, the `ConfirmClose` rule.
          if (!state.sessions.some((s) => s.projectId === project.id)) {
            state.removeProject(project.id);
            return;
          }
          usePrefixMode.setState({ dialog: { kind: "closeProject", id: project.id } });
          return;
        }
        case "goto":
          requestQuickSwitch();
          return;
        case "tab.new":
          state.newTab();
          return;
        case "tab.rename":
          if (!at) return prefixToast("no tab");
          usePrefixMode.setState({
            dialog: {
              kind: "rename",
              what: "tab",
              id: at.tab.id,
              projectId: at.projectId,
              value: at.tab.renamed ? at.tab.name : "",
            },
          });
          return;
        case "tab.close":
          if (at) requestClose({ projectId: at.projectId, tabId: at.tab.id });
          return;
        case "tab.prev":
          state.cycleTab(-1);
          return;
        case "tab.next":
          state.cycleTab(1);
          return;
        case "pane.left":
        case "pane.right":
        case "pane.up":
        case "pane.down":
          state.focusDirection(action.slice("pane.".length) as "left");
          return;
        case "pane.swapLeft":
        case "pane.swapRight":
        case "pane.swapUp":
        case "pane.swapDown":
          state.swapPane(action.slice("pane.swap".length).toLowerCase() as "left");
          return;
        case "pane.next":
        case "pane.prev": {
          if (!at) return;
          const all = leaves(at.tab.root);
          if (all.length < 2) return;
          const i = all.findIndex((l) => l.id === at.tab.focus);
          const next = all[wrapIndex(i + (action === "pane.next" ? 1 : -1), all.length)]!;
          state.focusLeaf(at.projectId, at.tab.id, next.id);
          return;
        }
        case "pane.splitRight":
          state.splitPane("right");
          return;
        case "pane.splitDown":
          state.splitPane("down");
          return;
        case "pane.close":
          if (at && leaf) requestClose({ projectId: at.projectId, tabId: at.tab.id, leafId: leaf.id });
          return;
        case "pane.zoom":
          state.toggleZoom();
          return;
        case "pane.rename": {
          const pane = leaf?.pane;
          const session =
            pane?.kind === "session" ? state.sessions.find((s) => s.id === pane.id) : undefined;
          if (!session) return prefixToast("this pane has no session to rename");
          usePrefixMode.setState({
            dialog: { kind: "rename", what: "pane", id: session.id, value: session.name },
          });
          return;
        }
        case "pane.break": {
          // tmux `break-pane`: the focused pane becomes a tab of its own, right
          // after this one. Structural — nothing ends.
          if (!at || !leaf) return;
          const source =
            leaf.pane.kind === "session"
              ? ({ kind: "session", sessionId: leaf.pane.id } as const)
              : ({ kind: "leaf", projectId: at.projectId, tabId: at.tab.id, leafId: leaf.id } as const);
          if (!state.moveToNewTab(source, at.projectId)) prefixToast("this pane is already alone in its tab");
          return;
        }
        case "mode.navigate": {
          const order = spaceOrder();
          const current = state.layout.activeProject;
          usePrefixMode.setState({
            navTarget: current && order.includes(current) ? current : (order[0] ?? null),
          });
          return;
        }
        case "mode.resize":
          return;
        case "tile.openBeside":
          if (!state.openNextBeside()) prefixToast("every project is already on screen");
          return;
        case "tile.close": {
          const tile = pid ? tileOf(state.layout, pid) : null;
          if (!tile || !state.layout.view) return prefixToast("only one tile — nothing to close");
          state.closeTile(tile.id);
          return;
        }
      }
    };

    const dispatch = (effect: Effect) => {
      const state = useWorkspace.getState();
      switch (effect.kind) {
        case "run":
          run(effect.action);
          return;
        case "selectTab":
          state.selectTabIndex(effect.index);
          return;
        case "focus":
          state.focusDirection(effect.dir);
          return;
        case "navMove": {
          const order = spaceOrder();
          if (!order.length) return;
          const at = order.indexOf(usePrefixMode.getState().navTarget ?? "");
          const next = order[wrapIndex((at < 0 ? 0 : at) + effect.delta, order.length)]!;
          usePrefixMode.setState({ navTarget: next });
          return;
        }
        case "navSelect": {
          const target = usePrefixMode.getState().navTarget;
          usePrefixMode.setState({ navTarget: null });
          if (target) state.selectProject(target);
          return;
        }
        case "resize": {
          const at = activeTab(state.layout);
          if (!at) return;
          const step = resizeStep(at.tab.root, at.tab.focus, effect.dir);
          if (step) state.setSplitRatio(at.projectId, at.tab.id, step.splitId, step.ratio);
          return;
        }
        case "sendPrefix":
          return; // handled in the runtime: it is a key, not a store call
      }
    };

    const uninstall = setPrefixDispatcher(dispatch);
    const unlisten = installWindowPrefix();
    // Leaving navigate mode by any route (Escape) drops the highlight.
    const unsub = usePrefixMode.subscribe((s, prev) => {
      if (prev.mode === "navigate" && s.mode !== "navigate" && s.navTarget) {
        usePrefixMode.setState({ navTarget: null });
      }
    });
    return () => {
      uninstall();
      unlisten();
      unsub();
    };
  }, []);
}
