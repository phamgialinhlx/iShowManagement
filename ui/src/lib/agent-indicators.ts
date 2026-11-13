import { useSyncExternalStore } from "react";

import { parseIndicatorStyle, type IndicatorStyle } from "./agent-state";

/**
 * Which indicator style the sidebar draws — Settings › Appearance › AGENT STATE.
 *
 * Stored inside `rmux.appearance` with the rest of the staged appearance, so it
 * commits on the same Apply. Settings is its own window, so the workbench hears
 * about a change through the `storage` event, the same channel the rest of
 * appearance already uses.
 */

const KEY = "rmux.appearance";

function read(): IndicatorStyle {
  try {
    const raw = localStorage.getItem(KEY);
    return parseIndicatorStyle(raw ? (JSON.parse(raw) as { agentIndicators?: unknown }).agentIndicators : undefined);
  } catch {
    return "dots";
  }
}

let current: IndicatorStyle = typeof localStorage === "undefined" ? "dots" : read();
const listeners = new Set<() => void>();

if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key !== KEY) return;
    const next = read();
    if (next === current) return;
    current = next;
    for (const l of listeners) l();
  });
}

/** Called by the Apply in the window that wrote it, which gets no `storage` event. */
export function refreshIndicatorStyle() {
  const next = read();
  if (next === current) return;
  current = next;
  for (const l of listeners) l();
}

export function useIndicatorStyle(): IndicatorStyle {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
    () => current,
  );
}
