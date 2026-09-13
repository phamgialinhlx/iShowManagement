import { useCallback, useEffect, useRef } from "react";
import { create } from "zustand";

import { api, isTauri } from "./api";
import { useWorkspace } from "./workspace";
import type { ServerId } from "./workspace-model";

/**
 * Auto-bridge every server to Redstone, quietly.
 *
 * When rmux is signed in to Redstone and the deployment offers the bridge, every
 * server in the workspace is enrolled without a button being pressed — on
 * sign-in, on app start when already signed in, and when a new server is
 * connected. The operator asked for "just bridge everything"; this is the engine
 * that does it while keeping the invariants that make per-host enrolment safe.
 *
 * The rules it must not break:
 *
 * - **Per-host token, never a shared credential.** Each host goes through
 *   `redstoneEnrol` on its own — never batched into one token.
 * - **Never re-enrol.** `redstoneEnrol` is *not* idempotent: re-minting orphans
 *   the old token in Redstone's host table. So every host is gated on
 *   `redstoneHostStatus(target).enrolled === false` before it is enrolled.
 * - **Quiet.** This runs unattended. It never throws to its caller and never
 *   pops a dialog — every outcome, success or failure, lands in a per-server
 *   status row the panel reads. A thrown error here would be a dialog over work
 *   the operator is doing with their hands.
 * - **Serial.** Each host is awaited before the next; enrolling in parallel
 *   risks N password prompts at once on password hosts, which is exactly the
 *   fail2ban-shaped failure the bridge's own design warns about.
 * - **Skip the incapable.** A Windows / agent-incapable host throws from
 *   `redstoneEnrol` with a "persistence is not enabled on Windows" style message;
 *   that is "can't bridge", not a fault, so it is recorded as `skipped`.
 */

/** boolean, default **off** — opt-in. A visible toggle writes this. */
const PREF_KEY = "rmux.redstone.autoBridge";

/** Same address the panel probes capabilities against. */
const ADDRESS_KEY = "rmux.redstone.address";

export type AutoBridgeStatus =
  | "pending"
  | "bridging"
  | "bridged"
  | "skipped"
  | "failed";

export type AutoBridgeEntry = {
  status: AutoBridgeStatus;
  /** Why it was skipped, or the error — shown beside the row. */
  detail?: string;
};

type AutoBridgeState = {
  /** Per-`serverId` outcome, read by the panel. */
  statuses: Record<ServerId, AutoBridgeEntry>;
  /** One line on what the engine last did — shown in the panel so a sweep that
   *  never reaches the per-server loop is not an invisible "everything waiting".
   *  This is the "never leave a blank screen / report the outcome" rule applied
   *  to an unattended action: when it does nothing, it must say why. */
  diag?: string;
  set: (id: ServerId, entry: AutoBridgeEntry) => void;
  setDiag: (diag: string) => void;
  clear: () => void;
};

/** Where the engine publishes its status so *other* windows can display it.
 *  The engine runs only in the workbench window (see `useAutoBridge`); the
 *  Settings window is a separate webview with its own store and must never run
 *  the sweep itself (its long SSH-backed commands crash the app if the window
 *  closes mid-flight). So the workbench writes here and Settings mirrors it. */
const STATUS_KEY = "rmux.redstone.autoBridge.status";

function loadStatus(): { statuses: Record<ServerId, AutoBridgeEntry>; diag?: string } {
  try {
    const raw = localStorage.getItem(STATUS_KEY);
    if (raw) return JSON.parse(raw) as { statuses: Record<ServerId, AutoBridgeEntry>; diag?: string };
  } catch {
    // Unreadable/absent — start empty.
  }
  return { statuses: {} };
}

function saveStatus(statuses: Record<ServerId, AutoBridgeEntry>, diag?: string): void {
  try {
    localStorage.setItem(STATUS_KEY, JSON.stringify({ statuses, diag }));
  } catch {
    // A window that cannot persist still works locally; other windows just will
    // not mirror it.
  }
}

/**
 * The status map the panel subscribes to. A standalone store rather than a limb
 * of the workspace store, because it is a fact about Redstone enrolment, not
 * about the Server → Project → Session tree, and folding it in would re-render
 * every rail consumer on each `bridging` tick.
 *
 * Every mutation is mirrored to `localStorage` so the Settings window (a separate
 * webview) can display what the workbench engine is doing without running the
 * engine — and hydrated from it on creation, so a window opened mid-sweep shows
 * the current state rather than a blank list.
 */
export const useAutoBridgeStore = create<AutoBridgeState>((set) => {
  const initial = loadStatus();
  return {
    statuses: initial.statuses,
    diag: initial.diag,
    set: (id, entry) =>
      set((s) => {
        const statuses = { ...s.statuses, [id]: entry };
        saveStatus(statuses, s.diag);
        return { statuses };
      }),
    setDiag: (diag) =>
      set((s) => {
        saveStatus(s.statuses, diag);
        return { diag };
      }),
    clear: () => {
      saveStatus({}, undefined);
      set({ statuses: {}, diag: undefined });
    },
  };
});

// Cross-window display mirror. A `storage` event fires only in *other* windows,
// so the workbench's writes reach Settings here with no loop and no command. We
// set state directly (not through the actions above) so mirroring does not write
// back to `localStorage`.
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key !== STATUS_KEY) return;
    const next = loadStatus();
    useAutoBridgeStore.setState({ statuses: next.statuses, diag: next.diag });
  });
}

/**
 * Servers that have reached a terminal outcome this session, so re-invocation
 * does not re-run them. Cleared by `resetAutoBridge` on sign-out; a *new* server
 * (a serverId not in this set) is still processed on the next run.
 *
 * Module-level rather than in the store: it is a guard, not something the UI
 * renders, and putting it in the store would make every membership check a
 * subscription.
 */
const processed = new Set<ServerId>();

/** Coalesces overlapping invocations into one serial pass. */
let running: Promise<void> | null = null;

/** The auto-bridge preference. **Default off — opt-in.** Auto-enrolling every
 *  server the operator has ever connected to would push a Redstone credential
 *  onto boxes they may not have meant to (a shared box, a client's server), so
 *  the operator turns this on once; thereafter it bridges every server
 *  automatically on each sign-in. Off by default also means the engine does not
 *  run at all until asked — the conservative posture for an unattended action. */
export function autoBridgeEnabled(): boolean {
  try {
    return localStorage.getItem(PREF_KEY) === "true";
  } catch {
    return false;
  }
}

export function setAutoBridgeEnabled(on: boolean): void {
  try {
    localStorage.setItem(PREF_KEY, on ? "true" : "false");
  } catch {
    // A machine that cannot store the preference still works; it just does not
    // remember the toggle across launches.
  }
}

/** Normalise the stored address to a base URL, the way the panel does. */
function baseAddress(): string | null {
  let typed: string;
  try {
    typed = (localStorage.getItem(ADDRESS_KEY) ?? "").trim();
  } catch {
    return null;
  }
  if (!typed) return null;
  const withScheme = typed.startsWith("http") ? typed : `https://${typed}`;
  return withScheme.replace(/\/+$/, "");
}

/** A Windows / agent-incapable host reports this from `redstoneEnrol`; it means
 *  "can't bridge", not a hard failure. */
function looksIncapable(message: string): boolean {
  return /persistence|windows|agent/i.test(message);
}

/**
 * Enrol every not-yet-bridged server, once each per session.
 *
 * Best-effort: it resolves whatever happens and never throws. All the gating
 * lives *here* rather than in the caller, so the hook can fire it freely on any
 * of its triggers without duplicating the conditions.
 */
export function runAutoBridge(): Promise<void> {
  // Coalesce: if a pass is already in flight, let it finish rather than start a
  // second one racing the same host list — but *record* that a fresh trigger
  // arrived, so the in-flight pass runs once more when it ends. This is the only
  // throttle; there is no time-based circuit breaker. A back-to-back trigger is
  // exactly what happens when the servers list hydrates a moment after app start:
  // the first pass may read an empty list, and the second, on the loaded list, is
  // the one that does the work. A cooldown that dropped it — or a plain coalesce
  // that swallowed it while the first pass was still awaiting session/caps —
  // stranded every server on "waiting" for good, because nothing re-triggers once
  // the list is stable. Idempotence is carried by the `processed` set, not a clock.
  if (running) {
    rerunRequested = true;
    return running;
  }
  running = (async () => {
    try {
      do {
        rerunRequested = false;
        await onePass();
      } while (rerunRequested);
    } finally {
      running = null;
    }
  })();
  return running;
}

/** Set while a pass is in flight to mean "the inputs changed — go again". */
let rerunRequested = false;

/** How long any one host gets before the sweep gives up on it and moves on.
 *  Generous enough for a first enrol, which uploads the ~1 MB agent over ssh on a
 *  slow link, but bounded so one unreachable host (a private-LAN alias off the
 *  VPN, a box that is simply down) cannot strand every server queued behind it —
 *  the serial sweep would otherwise sit on it for the OS TCP timeout, ~75s, each. */
const HOST_TIMEOUT_MS = 45_000;

/** Marker on the error a timeout throws, so the loop can tell "took too long" from
 *  a real failure and phrase it as a skip rather than an alarm. */
const TIMED_OUT = "__rmux_timed_out__";

/** Resolve `p`, or reject after `ms` with a tagged timeout error. The underlying
 *  IPC is not cancellable, so it runs to completion in the background — harmless:
 *  a late enrol just bridges the host anyway, and the next sweep sees it enrolled.
 *  What this buys is that the *sweep* stops waiting and moves to the next host. */
function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(TIMED_OUT)), ms);
  });
  return Promise.race([p, timeout]).finally(() => clearTimeout(timer));
}

/** One sweep over the current servers list. Re-reads `servers` each call, so a
 *  re-run after hydration sees the loaded list. Never throws. */
async function onePass(): Promise<void> {
  const { setDiag } = useAutoBridgeStore.getState();

  // Guard 1 — signed in and enabled.
  if (!autoBridgeEnabled()) return setDiag("auto-bridge is off");
  let session;
  try {
    session = await api.redstoneSession();
  } catch (e) {
    return setDiag(`can't read the Redstone session: ${e instanceof Error ? e.message : e}`);
  }
  if (!session) return setDiag("not signed in to Redstone");

  // Guard 2 — this deployment offers the bridge. The base URL comes from the
  // *session* rmux just fetched, not a separate localStorage key. A session
  // restored from the keychain (already signed in, never re-signed-in in this
  // build) leaves `rmux.redstone.address` empty, and reading that instead
  // stranded every server on "waiting" — the engine bailed here before the
  // per-server loop ever ran. The stored address is only a fallback.
  const address = (session.baseUrl ?? "").trim().replace(/\/+$/, "") || baseAddress();
  if (!address) return setDiag("no Redstone address");
  let caps;
  try {
    caps = await api.redstoneCapabilities(address);
  } catch (e) {
    return setDiag(`can't reach ${address}: ${e instanceof Error ? e.message : e}`);
  }
  if (!caps?.bridge) return setDiag(`${address} does not offer the bridge`);

  const { statuses, set } = useAutoBridgeStore.getState();
  const servers = useWorkspace.getState().servers;
  if (servers.length === 0) return setDiag("no servers in the workspace yet");
  setDiag(`bridging ${servers.length} server${servers.length === 1 ? "" : "s"}…`);

  for (const server of servers) {
    // Already reached a terminal outcome this session — leave it be.
    if (processed.has(server.id)) continue;
    // Do not re-mark one mid-flight (a second overlapping pass, or a status
    // already set to bridging).
    if (statuses[server.id]?.status === "bridging") continue;

    set(server.id, { status: "bridging" });

    let st;
    try {
      st = await withTimeout(api.redstoneHostStatus(server.target), HOST_TIMEOUT_MS);
    } catch (e) {
      // Status unreadable (host down, ssh refused) or too slow to answer. Record
      // and move on; aborting the whole pass would strand every server behind
      // this one — which is exactly what an unreachable host used to do.
      const message = e instanceof Error ? e.message : String(e);
      set(
        server.id,
        message === TIMED_OUT
          ? { status: "skipped", detail: "couldn't reach it — timed out" }
          : { status: "failed", detail: message },
      );
      processed.add(server.id);
      continue;
    }

    // Enrolled *and* the bridge is up — nothing to do.
    if (st.enrolled && st.running) {
      set(server.id, { status: "skipped", detail: "already bridged" });
      processed.add(server.id);
      continue;
    }

    try {
      if (st.enrolled) {
        // A token is already on the host but its bridge is down — the state a
        // host is left in when it was enrolled by a build whose start step
        // never actually launched the bridge. Start the bridge on the existing
        // token; re-minting would orphan it.
        await withTimeout(api.redstoneStartBridge(server.target), HOST_TIMEOUT_MS);
      } else {
        await withTimeout(api.redstoneEnrol(server.target), HOST_TIMEOUT_MS);
      }
      set(server.id, { status: "bridged" });
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (message === TIMED_OUT) {
        set(server.id, { status: "skipped", detail: "couldn't reach it — timed out" });
      } else if (looksIncapable(message)) {
        set(server.id, { status: "skipped", detail: "can't bridge (Windows)" });
      } else {
        set(server.id, { status: "failed", detail: message });
      }
    }
    processed.add(server.id);
  }

  // A short tally, so the panel says the sweep finished rather than leaving the
  // in-flight "bridging N…" line up for good.
  const outcomes = useAutoBridgeStore.getState().statuses;
  const done = servers.filter((s) => processed.has(s.id)).length;
  setDiag(`swept ${done}/${servers.length} — ${tally(servers, outcomes)}`);
}

/** "3 bridged, 1 skipped, 1 failed" from the current outcome map. */
function tally(
  servers: { id: ServerId }[],
  outcomes: Record<ServerId, AutoBridgeEntry>,
): string {
  const count = (s: AutoBridgeStatus) =>
    servers.filter((sv) => outcomes[sv.id]?.status === s).length;
  const parts = [
    [count("bridged"), "bridged"],
    [count("skipped"), "skipped"],
    [count("failed"), "failed"],
  ] as const;
  const shown = parts.filter(([n]) => n > 0).map(([n, label]) => `${n} ${label}`);
  return shown.length ? shown.join(", ") : "nothing to do";
}

/**
 * Forget every outcome and every processed id, so the next sign-in re-runs the
 * whole sweep from scratch. Called on sign-out.
 */
export function resetAutoBridge(): void {
  processed.clear();
  useAutoBridgeStore.getState().clear();
}

/**
 * Sign-in, sign-out and the toggle all fire this. The hook re-reads the session
 * on it and either sweeps or resets — one signal, and the truth is re-derived
 * rather than carried in the event.
 */
export const AUTO_BRIDGE_EVENT = "rmux:redstone-auto-bridge";

/** Poke the mounted hook after a state change it cannot otherwise see. */
export function notifyAutoBridge(): void {
  try {
    window.dispatchEvent(new Event(AUTO_BRIDGE_EVENT));
  } catch {
    // No window (non-browser test context) — the hook is not mounted anyway.
  }
}

/**
 * Drive the sweep from the top of the app, regardless of which panel is open.
 *
 * Mounted once (in the workbench). It fires `runAutoBridge` — which carries all
 * the guards itself — on app start, whenever the servers list changes (a new
 * server to bridge), and whenever `notifyAutoBridge` is dispatched (sign-in,
 * sign-out, toggle). A observed sign-out (present → absent) resets, so a fresh
 * sign-in re-runs the whole sweep. Every trigger is debounced ~300ms so a burst
 * of store updates coalesces into a single pass.
 */
export function useAutoBridge(): void {
  const servers = useWorkspace((s) => s.servers);
  const present = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const evaluate = useCallback(() => {
    if (!isTauri()) return;
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      void (async () => {
        let session;
        try {
          session = await api.redstoneSession();
        } catch {
          // Unknown — leave `present` as it was rather than flipping on a blip,
          // which would wipe good statuses on a transient IPC failure.
          return;
        }
        if (session) {
          present.current = true;
          void runAutoBridge();
        } else if (present.current) {
          // Observed transition to signed-out — clear so the next sign-in re-runs.
          present.current = false;
          resetAutoBridge();
        }
      })();
    }, 300);
  }, []);

  // App start (already signed in) and every servers-list change. The guards
  // inside `runAutoBridge` decide whether anything actually happens.
  useEffect(() => {
    evaluate();
  }, [servers, evaluate]);

  // Sign-in, sign-out and the toggle — within this window.
  useEffect(() => {
    if (!isTauri()) return;
    const onSignal = () => evaluate();
    window.addEventListener(AUTO_BRIDGE_EVENT, onSignal);
    return () => window.removeEventListener(AUTO_BRIDGE_EVENT, onSignal);
  }, [evaluate]);

  // The toggle lives in the Settings window, which does not run the engine, so
  // flipping it there reaches the engine only through `localStorage`. When the
  // preference turns on from another window, re-sweep here — clearing the
  // processed set first so a re-enable genuinely retries rather than reporting
  // the last run's outcomes. (A `storage` event never fires in the window that
  // wrote it, so this is the cross-window half of the same signal.)
  useEffect(() => {
    if (!isTauri()) return;
    const onStorage = (e: StorageEvent) => {
      if (e.key !== PREF_KEY) return;
      if (e.newValue === "true") {
        processed.clear();
        evaluate();
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [evaluate]);
}
