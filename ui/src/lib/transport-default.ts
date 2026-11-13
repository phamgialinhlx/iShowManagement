import { useSyncExternalStore } from "react";

import type { SessionV3 } from "./workspace-model";

/**
 * Which transport a session uses when it has not chosen one itself.
 *
 * **Default: low latency (mosh).** The operator asked for rmux to prefer mosh
 * unless told otherwise. It used to be strictly opt-in per session because a
 * low-latency pane keeps a screen, not a scrollback history — that trade-off is
 * now made once, here, by the operator, and Session Settings › TRANSPORT still
 * overrides it either way for one session.
 *
 * Preferring mosh can never leave a pane unconnectable: a host without a usable
 * `mosh-server` (or a local session) falls back to plain ssh inside
 * `terminal_open`, and TRANSPORT says so.
 *
 * Stored per machine, shared by every window; `storage` events keep windows in
 * step, the same channel the appearance settings use.
 */
const KEY = "rmux.transport.default";

export type DefaultTransport = "mosh" | "ssh";

/** Absent means **mosh** — only an explicit "ssh" opts out. */
function read(): DefaultTransport {
  try {
    return localStorage.getItem(KEY) === "ssh" ? "ssh" : "mosh";
  } catch {
    return "mosh";
  }
}

let current = read();
const listeners = new Set<() => void>();

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  const onStorage = (e: StorageEvent) => {
    if (e.key !== KEY) return;
    current = read();
    fn();
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(fn);
    window.removeEventListener("storage", onStorage);
  };
}

export function defaultTransport(): DefaultTransport {
  return current;
}

export function setDefaultTransport(next: DefaultTransport): void {
  current = next;
  try {
    localStorage.setItem(KEY, next);
  } catch {
    // Unwritable storage costs persistence, not the effect for this run.
  }
  for (const fn of listeners) fn();
}

export function useDefaultTransport(): DefaultTransport {
  return useSyncExternalStore(subscribe, defaultTransport, defaultTransport);
}

/**
 * The transport a session actually asks for: its own choice when it made one,
 * otherwise the app-wide default. `undefined` on the session means "follow the
 * default" — sessions saved before this setting existed never stored a choice,
 * so they follow it too.
 */
export function effectiveLowLatency(
  session: Pick<SessionV3, "lowLatency"> | null | undefined,
  fallback: DefaultTransport = current,
): boolean {
  if (session?.lowLatency !== undefined) return session.lowLatency;
  return fallback === "mosh";
}

/** Hook form, so a pane re-renders when the default changes. */
export function useEffectiveLowLatency(
  session: Pick<SessionV3, "lowLatency"> | null | undefined,
): boolean {
  return effectiveLowLatency(session, useDefaultTransport());
}
