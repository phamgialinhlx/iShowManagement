import { useEffect, useState } from "react";

import { api, type FallbackBinding, type FallbackLog, type FallbackRecord, type TargetRef } from "./api";

/**
 * What the host knows about a chained session's account: which member it is on
 * now, which are spent, and the last switch.
 *
 * **The daemon decides; the app only reads.** Detection and switching run on
 * the host (so they work with rmux closed); this is the mirror that makes a
 * switch *loud* — the pane strip, the sidebar's profile name, Session
 * Settings. One poll per host, shared by every pane that asks, and only while
 * at least one chained session is mounted: a host with no chains is never
 * asked anything.
 */
export type FallbackState = {
  binding?: FallbackBinding;
  /** The newest record for this session, if any. */
  last?: FallbackRecord;
  error?: string;
};

const POLL_MS = 15_000;

type Entry = {
  target: TargetRef;
  log?: FallbackLog;
  error?: string;
  listeners: Set<() => void>;
  timer?: ReturnType<typeof setInterval>;
};

const entries = new Map<string, Entry>();

function poll(entry: Entry) {
  api
    .claudeFallbackLog(entry.target)
    .then((log) => {
      entry.log = log;
      entry.error = undefined;
    })
    .catch((e) => {
      // Kept beside the last good answer rather than replacing it: a dropped
      // connection must not make a switch that happened disappear.
      entry.error = typeof e === "string" ? e : String(e);
    })
    .finally(() => entry.listeners.forEach((l) => l()));
}

/** Ask now, rather than at the next tick — after the operator switches back. */
export function refreshFallback(target: TargetRef) {
  const entry = entries.get(JSON.stringify(target));
  if (entry) poll(entry);
}

export function selectFallback(log: FallbackLog | undefined, session: string): FallbackState {
  if (!log) return {};
  const binding = log.bindings.find((b) => b.session === session);
  const mine = log.switches.filter((r) => r.session === session);
  return { binding, last: mine[mine.length - 1] };
}

export function useFallbackState(target: TargetRef | undefined, session: string, enabled: boolean): FallbackState {
  const [state, setState] = useState<FallbackState>({});
  const key = target ? JSON.stringify(target) : "";
  useEffect(() => {
    if (!enabled || !target) return;
    let entry = entries.get(key);
    if (!entry) {
      entry = { target, listeners: new Set() };
      entries.set(key, entry);
    }
    const e = entry;
    const listener = () => setState({ ...selectFallback(e.log, session), error: e.error });
    e.listeners.add(listener);
    if (!e.timer) {
      poll(e);
      e.timer = setInterval(() => poll(e), POLL_MS);
    } else {
      listener();
    }
    return () => {
      e.listeners.delete(listener);
      if (e.listeners.size === 0) {
        if (e.timer) clearInterval(e.timer);
        entries.delete(key);
      }
    };
    // `enabled` is a dependency, or switching a chain back on would never restart the poll.
  }, [key, session, enabled]); // eslint-disable-line react-hooks/exhaustive-deps
  return enabled ? state : {};
}

/** `14:02` in the operator's own clock. */
export function clock(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** The strip's sentence for a record — said in full, never as an icon. */
export function describeSwitch(r: FallbackRecord): string {
  const from = r.fromLabel ?? r.from;
  const to = r.toLabel ?? r.to;
  const resets = r.resetsAt ? ` — resets ${clock(r.resetsAt)}` : "";
  if (r.outcome === "switched") return `Switched to ${to} after a ${r.reason} on ${from} at ${clock(r.at)}${resets}`;
  if (r.outcome === "switched_back") return `Moved back to ${to} from ${from} at ${clock(r.at)}`;
  if (r.outcome.startsWith("end_of_chain")) {
    return `Every account in the chain is spent: ${r.reason} on ${from} at ${clock(r.at)}${resets}. The session is waiting.`;
  }
  return `Could not switch from ${from} after a ${r.reason} at ${clock(r.at)}: ${r.outcome.replace(/^failed: /, "")}`;
}

/** Earlier members whose window has passed — what "switch back" may offer. Never acted on automatically. */
export function recovered(binding: FallbackBinding | undefined, nowSeconds: number): string[] {
  const chain = binding?.binding?.chain;
  if (!chain) return [];
  const upto = chain.members.indexOf(chain.active);
  return chain.members.slice(0, Math.max(upto, 0)).filter((m) => {
    const e = chain.exhausted?.[m];
    return !!e && nowSeconds >= (e.resetsAt ?? e.at + 5 * 3600);
  });
}

let namesCache: Promise<Record<string, string>> | null = null;

/** Profile and chain names by id, read once per window and refreshed on demand. */
export function invalidateProfileNames() {
  namesCache = null;
}

export function useProfileNames(enabled: boolean): Record<string, string> {
  const [names, setNames] = useState<Record<string, string>>({});
  useEffect(() => {
    if (!enabled) return;
    if (!namesCache) {
      namesCache = Promise.all([
        api.modelProfiles().catch(() => []),
        api.modelChains().catch(() => ({ chains: [], defaults: {} })),
      ]).then(([profiles, view]) => {
        const out: Record<string, string> = {};
        for (const p of profiles) out[p.id] = p.name;
        for (const c of view.chains) out[c.id] = c.name;
        return out;
      });
    }
    let live = true;
    void namesCache.then((n) => live && setNames(n));
    return () => {
      live = false;
    };
  }, [enabled]);
  return names;
}
