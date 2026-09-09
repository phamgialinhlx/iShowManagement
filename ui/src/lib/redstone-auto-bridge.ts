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
  set: (id: ServerId, entry: AutoBridgeEntry) => void;
  clear: () => void;
};

/**
 * The status map the panel subscribes to. A standalone store rather than a limb
 * of the workspace store, because it is a fact about Redstone enrolment, not
 * about the Server → Project → Session tree, and folding it in would re-render
 * every rail consumer on each `bridging` tick.
 */
export const useAutoBridgeStore = create<AutoBridgeState>((set) => ({
  statuses: {},
  set: (id, entry) => set((s) => ({ statuses: { ...s.statuses, [id]: entry } })),
  clear: () => set({ statuses: {} }),
}));

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

/** Circuit breaker: never start passes back-to-back. The engine is already
 *  idempotent (the `processed` set) and serial, but a pathological burst of
 *  `servers`-list changes should not fire a session+capabilities IPC on each —
 *  cap it to one pass per `MIN_GAP_MS`. */
let lastRunAt = 0;
const MIN_GAP_MS = 2000;

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
export async function runAutoBridge(): Promise<void> {
  // Coalesce: if a pass is already in flight, let it finish rather than start a
  // second one racing the same host list.
  if (running) return running;
  // Circuit breaker: refuse a pass that follows the last one too closely, so a
  // burst of triggers cannot fire back-to-back session/capabilities probes.
  const now = Date.now();
  if (now - lastRunAt < MIN_GAP_MS) return;
  lastRunAt = now;
  running = (async () => {
    try {
      // Guard 1 — signed in and enabled.
      if (!autoBridgeEnabled()) return;
      let session;
      try {
        session = await api.redstoneSession();
      } catch {
        return;
      }
      if (!session) return;

      // Guard 2 — this deployment offers the bridge.
      const address = baseAddress();
      if (!address) return;
      let caps;
      try {
        caps = await api.redstoneCapabilities(address);
      } catch {
        return;
      }
      if (!caps?.bridge) return;

      const { statuses, set } = useAutoBridgeStore.getState();
      const servers = useWorkspace.getState().servers;

      for (const server of servers) {
        // Already reached a terminal outcome this session — leave it be.
        if (processed.has(server.id)) continue;
        // Do not re-mark one mid-flight (a second overlapping pass, or a status
        // already set to bridging).
        if (statuses[server.id]?.status === "bridging") continue;

        set(server.id, { status: "bridging" });

        try {
          const st = await api.redstoneHostStatus(server.target);
          if (st.enrolled) {
            // The orphaned-token guard: a host already carrying a token must not
            // be re-minted.
            set(server.id, { status: "skipped", detail: "already bridged" });
            processed.add(server.id);
            continue;
          }
        } catch (e) {
          // Status unreadable (host down, ssh refused). Record and move on;
          // aborting the whole pass would strand every server behind this one.
          set(server.id, {
            status: "failed",
            detail: e instanceof Error ? e.message : String(e),
          });
          processed.add(server.id);
          continue;
        }

        try {
          await api.redstoneEnrol(server.target);
          set(server.id, { status: "bridged" });
        } catch (e) {
          const message = e instanceof Error ? e.message : String(e);
          if (looksIncapable(message)) {
            set(server.id, { status: "skipped", detail: "can't bridge (Windows)" });
          } else {
            set(server.id, { status: "failed", detail: message });
          }
        }
        processed.add(server.id);
      }
    } finally {
      running = null;
    }
  })();
  return running;
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

  // Sign-in, sign-out and the toggle.
  useEffect(() => {
    if (!isTauri()) return;
    const onSignal = () => evaluate();
    window.addEventListener(AUTO_BRIDGE_EVENT, onSignal);
    return () => window.removeEventListener(AUTO_BRIDGE_EVENT, onSignal);
  }, [evaluate]);
}
