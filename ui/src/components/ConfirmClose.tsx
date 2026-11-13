import { useEffect, useRef, useState } from "react";

import { findLeaf, sessionsInTab, tabsOf } from "../lib/layout-tree";
import { CLOSE_EVENT, type CloseRequest } from "../lib/shortcuts";
import { useWorkspace } from "../lib/workspace";

/**
 * herdr's `confirm_close`: closing a pane or a tab that holds a session ends
 * that session's work on the host, exactly as the rail's ✕ does — so it asks
 * first, and says **what ends**.
 *
 * A generic "its sessions will be ended" is a sentence people click past; the
 * count and the names are what make the confirmation worth reading (the rule the
 * rail's own `ENDS 3 SESSIONS…` follows). Closing only host or files panes ends
 * nothing and closes without asking — a prompt for a free action teaches the
 * operator to press through prompts.
 *
 * The dangerous choice is never the default: **cancel** holds the focus, so a
 * reflexive Enter keeps the work. Escape cancels too. Stepping away without
 * killing is Detach, in the rail's right-click menu, and the prompt says so.
 *
 * Every close goes through here — the tab ✕, the pane-tools ✕ and the keyboard
 * — via `CLOSE_EVENT`, so no path can skip it.
 */
export function ConfirmClose() {
  const [ask, setAsk] = useState<CloseRequest | null>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const closeTab = useWorkspace((s) => s.closeTab);
  const closeLeaf = useWorkspace((s) => s.closeLeaf);

  useEffect(() => {
    const onAsk = (e: Event) => {
      const req = (e as CustomEvent<CloseRequest>).detail;
      if (!req) return;
      const state = useWorkspace.getState();
      const tab = tabsOf(state.layout, req.projectId).tabs.find((t) => t.id === req.tabId);
      if (!tab) return;
      const ends = req.leafId
        ? (() => {
            const leaf = findLeaf(tab.root, req.leafId!);
            return leaf?.pane.kind === "session" ? [leaf.pane.id] : [];
          })()
        : sessionsInTab(tab);
      if (ends.length === 0) {
        // Nothing on the host ends — just close it.
        if (req.leafId) state.closeLeaf(req.projectId, req.tabId, req.leafId);
        else state.closeTab(req.projectId, req.tabId);
        return;
      }
      setAsk(req);
    };
    window.addEventListener(CLOSE_EVENT, onAsk);
    return () => window.removeEventListener(CLOSE_EVENT, onAsk);
  }, []);

  useEffect(() => {
    if (!ask) return;
    cancel.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setAsk(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [ask]);

  const layout = useWorkspace((s) => s.layout);
  const sessions = useWorkspace((s) => s.sessions);
  const serverOf = useWorkspace((s) => s.serverOf);
  if (!ask) return null;

  const tab = tabsOf(layout, ask.projectId).tabs.find((t) => t.id === ask.tabId);
  const ids = !tab
    ? []
    : ask.leafId
      ? (() => {
          const leaf = findLeaf(tab.root, ask.leafId!);
          return leaf?.pane.kind === "session" ? [leaf.pane.id] : [];
        })()
      : sessionsInTab(tab);
  const ending = ids.flatMap((id) => {
    const s = sessions.find((x) => x.id === id);
    return s ? [s] : [];
  });
  // The tab or session went away while the prompt was open — nothing to ask.
  if (!tab || ending.length === 0) return null;
  const host = serverOf(ending[0]!.id)?.target.host ?? "this machine";
  const n = ending.length;

  const confirm = () => {
    setAsk(null);
    if (ask.leafId) closeLeaf(ask.projectId, ask.tabId, ask.leafId);
    else closeTab(ask.projectId, ask.tabId);
  };

  return (
    <div
      className="fixed inset-0 z-[96] grid place-items-start justify-center pt-[14vh]"
      style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
      onMouseDown={() => setAsk(null)}
    >
      <div
        role="alertdialog"
        aria-label="Confirm close"
        className="window flex w-[min(440px,92vw)] max-h-[70vh] flex-col gap-3 overflow-y-auto p-4"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <span className="micro" style={{ color: "var(--text)" }}>
          {`ENDS ${n} SESSION${n === 1 ? "" : "S"} ON ${host.toUpperCase()}`}
        </span>
        {/* Bounded: a tab can hold any number of panes. */}
        <ul className="data flex max-h-[30vh] flex-col gap-1 overflow-y-auto text-[11px]" style={{ color: "var(--text-soft)" }}>
          {ending.map((s) => (
            <li key={s.id} className="truncate">
              {s.kind === "terminal" ? "shell" : s.kind} · {s.name}
            </li>
          ))}
        </ul>
        <span className="data text-[11px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
          {n === 1 ? "Its shell or conversation is stopped on the host." : "Their shells and conversations are stopped on the host."}{" "}
          To step away and keep it running, use Detach in the rail's right-click menu.
        </span>
        <div className="flex justify-end gap-2">
          <button ref={cancel} type="button" className="chip" onClick={() => setAsk(null)}>
            cancel
          </button>
          <button type="button" className="chip" style={{ color: "rgb(var(--primary))" }} onClick={confirm}>
            {n === 1 ? "end it" : `end all ${n}`}
          </button>
        </div>
      </div>
    </div>
  );
}
