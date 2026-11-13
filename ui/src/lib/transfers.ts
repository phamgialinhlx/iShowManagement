import { create } from "zustand";

import type { TargetRef } from "./api.ts";

/**
 * Every download and upload, from every entry point, in one app-wide list.
 *
 * Their status used to live in the context menu that started them, so it
 * vanished the moment the menu closed — and a large transfer gave no feedback at
 * all. Here it outlives menus, panes and windows, and a finished entry stays
 * until someone dismisses it.
 *
 * This file is the model (pure, pinned by `ui/transfers-check.ts`) plus a small
 * store. The code that actually moves bytes is `transfer-run.ts`.
 *
 * ## What is persisted, and why so little
 *
 * **Metadata only**, under `rmux.transfers`: names, paths, counts, states. Never
 * file bytes — `localStorage` is one quota shared with the session list, and
 * overflowing it costs someone their sessions. Pruned on every write to the last
 * {@link MAX_ENTRIES} entries and {@link MAX_AGE_MS} of age.
 *
 * ## Interrupted is not failed, and it is never "done"
 *
 * A transfer that was running when rmux closed did not finish, and the app must
 * not say it did. On the next start it reads **interrupted**, with the reason.
 * A download can be retried — its source is still on the server. An upload
 * **cannot**: the webview held the file as a `File` with no path, and that is
 * gone with the page. So an interrupted upload has no retry control at all —
 * never show a control that cannot work — and says to drop the file again.
 */

export type Direction = "up" | "down";

export type TransferState = "queued" | "running" | "done" | "failed" | "cancelled" | "interrupted";

export type TransferEntry = {
  id: string;
  direction: Direction;
  /** As the rail names the server — `user@host`, or "this machine". */
  server: string;
  /** Needed to retry a download. Carries no secrets: a chosen key is a *path*. */
  target: TargetRef;
  remotePath: string;
  /** The file's name, for the row. */
  name: string;
  /** Where a finished download landed on this machine. */
  localPath?: string;
  done: number;
  /** `null` while unknown — drawn as an indeterminate bar, never a guessed percentage. */
  total: number | null;
  state: TransferState;
  /** An upload has two legs at very different speeds; the row says which. */
  phase?: "reading" | "sending";
  error?: string;
  startedAt: number;
  endedAt?: number;
  /** When the current leg began and how far it had got, for the rate. */
  phaseAt?: number;
  /** The upload's staging id, so a cancel from any window can reach it. */
  uploadId?: string;
  /** The window running it. Only that window moves its bytes. */
  owner: string;
  /** The owner's last heartbeat. A running entry whose owner stopped beating was cut off. */
  seenAt: number;
  /** Another window asked the owner to stop it. */
  cancelRequested?: boolean;
};

export const MAX_ENTRIES = 100;
export const MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;
/** A running entry not heartbeaten for this long belongs to a window that is gone. */
export const STALE_MS = 12_000;
export const HEARTBEAT_MS = 3_000;
/** Below this, a finished transfer is not worth a card — the operator just watched it. */
export const NOTIFY_AFTER_MS = 5_000;
export const STORAGE_KEY = "rmux.transfers";

export const ACTIVE: readonly TransferState[] = ["queued", "running"];

export const isActive = (e: TransferEntry) => ACTIVE.includes(e.state);
export const isFinished = (e: TransferEntry) => !isActive(e);

/** The legal moves. Anything else is ignored — a late progress event cannot resurrect a cancelled row. */
const NEXT: Record<TransferState, readonly TransferState[]> = {
  queued: ["running", "cancelled", "failed", "interrupted"],
  running: ["done", "failed", "cancelled", "interrupted"],
  done: [],
  failed: [],
  cancelled: [],
  interrupted: [],
};

export function canMove(from: TransferState, to: TransferState): boolean {
  return NEXT[from].includes(to);
}

export function createEntry(
  init: Pick<TransferEntry, "id" | "direction" | "server" | "target" | "remotePath" | "name" | "owner"> & {
    total?: number | null;
  },
  now: number,
): TransferEntry {
  return {
    ...init,
    done: 0,
    total: init.total ?? null,
    state: "queued",
    startedAt: now,
    seenAt: now,
  };
}

export function start(e: TransferEntry, now: number): TransferEntry {
  if (!canMove(e.state, "running")) return e;
  return { ...e, state: "running", phaseAt: now, seenAt: now };
}

/**
 * Bytes moved. Never more than the total, never backwards within a leg — a
 * bar that overshoots or jitters back is a report nobody can trust.
 */
export function progress(
  e: TransferEntry,
  done: number,
  total: number | null | undefined,
  now: number,
  phase?: TransferEntry["phase"],
): TransferEntry {
  if (e.state !== "running") return e;
  const nextTotal = total === undefined ? e.total : total;
  const newLeg = phase !== undefined && phase !== e.phase;
  let nextDone = Math.max(0, newLeg ? done : Math.max(e.done, done));
  if (nextTotal !== null) nextDone = Math.min(nextDone, nextTotal);
  return {
    ...e,
    done: nextDone,
    total: nextTotal,
    phase: phase ?? e.phase,
    phaseAt: newLeg ? now : e.phaseAt,
    seenAt: now,
  };
}

export function finish(e: TransferEntry, now: number, result?: { bytes?: number; localPath?: string }): TransferEntry {
  if (!canMove(e.state, "done")) return e;
  const total = result?.bytes ?? e.total ?? e.done;
  return { ...e, state: "done", done: total, total, localPath: result?.localPath ?? e.localPath, endedAt: now, seenAt: now, error: undefined };
}

export function fail(e: TransferEntry, message: string, now: number): TransferEntry {
  if (!canMove(e.state, "failed")) return e;
  return { ...e, state: "failed", error: message || "the transfer failed", endedAt: now, seenAt: now };
}

export function cancel(e: TransferEntry, now: number): TransferEntry {
  if (!canMove(e.state, "cancelled")) return e;
  return { ...e, state: "cancelled", endedAt: now, seenAt: now };
}

export function interruptedReason(direction: Direction): string {
  return direction === "down"
    ? "rmux closed before this download finished — nothing was saved under its name."
    : "rmux closed before this upload finished, so it did not complete on the server. Drop the file again to retry.";
}

export function interrupt(e: TransferEntry, now: number): TransferEntry {
  if (!canMove(e.state, "interrupted")) return e;
  return { ...e, state: "interrupted", error: interruptedReason(e.direction), endedAt: now };
}

/** Back to the start, same row. Downloads only — see the module comment. */
export function retry(e: TransferEntry, owner: string, now: number): TransferEntry {
  if (!canRetry(e)) return e;
  return {
    ...e,
    state: "queued",
    done: 0,
    total: null,
    error: undefined,
    endedAt: undefined,
    localPath: undefined,
    phase: undefined,
    cancelRequested: undefined,
    owner,
    startedAt: now,
    seenAt: now,
  };
}

/** Only a download can be retried: the server still has its source; the webview no longer has an upload's. */
export function canRetry(e: TransferEntry): boolean {
  return e.direction === "down" && (e.state === "interrupted" || e.state === "failed" || e.state === "cancelled");
}

export const canCancel = (e: TransferEntry) => isActive(e);

/** `null` when there is no honest percentage to give. */
export function percent(e: TransferEntry): number | null {
  if (e.state === "done") return 100;
  if (e.total === null) return null;
  if (e.total === 0) return e.state === "running" ? null : 0;
  return Math.min(100, Math.floor((e.done / e.total) * 100));
}

/** Bytes per second over the current leg, or `null` before there is anything to divide. */
export function rate(e: TransferEntry, now: number): number | null {
  if (e.state !== "running" || e.phaseAt === undefined) return null;
  const secs = (now - e.phaseAt) / 1000;
  if (secs < 0.5 || e.done <= 0) return null;
  return e.done / secs;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

/**
 * Newest first, bounded in both dimensions on every write. Active entries are
 * never pruned — dropping a running transfer's row would hide work in flight.
 */
export function prune(entries: TransferEntry[], now: number): TransferEntry[] {
  const kept = entries.filter((e) => isActive(e) || now - (e.endedAt ?? e.startedAt) <= MAX_AGE_MS);
  kept.sort((a, b) => b.startedAt - a.startedAt);
  const active = kept.filter(isActive);
  const finished = kept.filter(isFinished).slice(0, Math.max(0, MAX_ENTRIES - active.length));
  return [...active, ...finished].sort((a, b) => b.startedAt - a.startedAt);
}

/** The persisted fields, named one by one, so nothing else can ride along into storage. */
const PERSISTED = [
  "id", "direction", "server", "target", "remotePath", "name", "localPath", "done", "total", "state",
  "phase", "error", "startedAt", "endedAt", "phaseAt", "uploadId", "owner", "seenAt", "cancelRequested",
] as const satisfies readonly (keyof TransferEntry)[];

const TARGET_FIELDS = ["host", "user", "port", "identity"] as const;

export function serialize(entries: TransferEntry[], now: number): string {
  return JSON.stringify(
    prune(entries, now).map((e) => {
      const out: Record<string, unknown> = {};
      for (const k of PERSISTED) if (e[k] !== undefined) out[k] = e[k];
      const t: Record<string, unknown> = {};
      for (const k of TARGET_FIELDS) if (e.target[k] !== undefined) t[k] = e.target[k];
      out.target = t;
      return out;
    }),
  );
}

const STATES: readonly TransferState[] = ["queued", "running", "done", "failed", "cancelled", "interrupted"];

function parseEntry(raw: unknown): TransferEntry | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.id !== "string" || (r.direction !== "up" && r.direction !== "down")) return null;
  if (typeof r.state !== "string" || !STATES.includes(r.state as TransferState)) return null;
  if (typeof r.remotePath !== "string" || typeof r.startedAt !== "number") return null;
  const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : undefined);
  const str = (v: unknown) => (typeof v === "string" ? v : undefined);
  return {
    id: r.id,
    direction: r.direction,
    server: str(r.server) ?? "",
    target: r.target && typeof r.target === "object" ? (r.target as TargetRef) : {},
    remotePath: r.remotePath,
    name: str(r.name) ?? r.remotePath.split("/").pop() ?? r.remotePath,
    localPath: str(r.localPath),
    done: num(r.done) ?? 0,
    total: num(r.total) ?? null,
    state: r.state as TransferState,
    phase: r.phase === "reading" || r.phase === "sending" ? r.phase : undefined,
    error: str(r.error),
    startedAt: r.startedAt,
    endedAt: num(r.endedAt),
    phaseAt: num(r.phaseAt),
    uploadId: str(r.uploadId),
    owner: str(r.owner) ?? "",
    seenAt: num(r.seenAt) ?? r.startedAt,
    cancelRequested: r.cancelRequested === true ? true : undefined,
  };
}

export function parse(raw: string | null): TransferEntry[] {
  if (!raw) return [];
  try {
    const list: unknown = JSON.parse(raw);
    return Array.isArray(list) ? list.map(parseEntry).filter((e): e is TransferEntry => e !== null) : [];
  } catch {
    return [];
  }
}

/**
 * Entries running in a window that is no longer there become interrupted.
 *
 * Judged by heartbeat rather than "every running entry at load", because a
 * second window opening is not a restart: the first is still moving those bytes.
 */
export function reapStale(entries: TransferEntry[], me: string, now: number): TransferEntry[] {
  return entries.map((e) =>
    isActive(e) && e.owner !== me && now - e.seenAt > STALE_MS ? interrupt(e, now) : e,
  );
}

/**
 * Another window wrote the list. Each window is the authority for the
 * transfers it runs; for everything else the incoming list wins.
 */
export function merge(local: TransferEntry[], incoming: TransferEntry[], me: string): TransferEntry[] {
  const mine = new Map(local.filter((e) => e.owner === me).map((e) => [e.id, e]));
  const out: TransferEntry[] = [];
  const seen = new Set<string>();
  for (const e of incoming) {
    const own = mine.get(e.id);
    if (own) {
      // Mine and still moving: keep my copy, but hear a cancel asked for elsewhere.
      // Mine and finished: keep my copy (it may be newer than theirs).
      out.push(e.cancelRequested && isActive(own) ? { ...own, cancelRequested: true } : own);
    } else {
      out.push(e);
    }
    seen.add(e.id);
  }
  // Mine but absent from theirs: still running → keep; finished → dismissed elsewhere.
  for (const e of mine.values()) if (!seen.has(e.id) && isActive(e)) out.push(e);
  return out.sort((a, b) => b.startedAt - a.startedAt);
}

/* ------------------------------------------------------------------ store */

/** This window's identity for ownership. Fresh per page load, which is exactly a "run". */
export const RUN_ID = `w${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;

function readStored(): TransferEntry[] {
  try {
    return parse(globalThis.localStorage?.getItem(STORAGE_KEY) ?? null);
  } catch {
    return [];
  }
}

function writeStored(entries: TransferEntry[]): void {
  try {
    globalThis.localStorage?.setItem(STORAGE_KEY, serialize(entries, Date.now()));
  } catch {
    // Quota or blocked storage: the list still works for this run.
  }
}

type Store = {
  entries: TransferEntry[];
  /** Whether this window's panel is open — a finish seen there needs no card. */
  panelOpen: boolean;
  setPanelOpen: (open: boolean) => void;
  add: (e: TransferEntry) => void;
  /** Apply a model function to one entry. `persist: false` for progress ticks. */
  update: (id: string, fn: (e: TransferEntry) => TransferEntry, persist?: boolean) => void;
  dismiss: (id: string) => void;
  clearFinished: () => void;
  get: (id: string) => TransferEntry | undefined;
};

export const useTransfers = create<Store>((set, get) => {
  const save = () => writeStored(get().entries);
  return {
    entries: reapStale(readStored(), RUN_ID, Date.now()),
    panelOpen: false,
    setPanelOpen: (panelOpen) => set({ panelOpen }),
    add: (e) => {
      set((s) => ({ entries: prune([e, ...s.entries.filter((x) => x.id !== e.id)], Date.now()) }));
      save();
    },
    update: (id, fn, persist = true) => {
      set((s) => ({ entries: s.entries.map((e) => (e.id === id ? fn(e) : e)) }));
      if (persist) save();
    },
    dismiss: (id) => {
      set((s) => ({ entries: s.entries.filter((e) => e.id !== id || isActive(e)) }));
      save();
    },
    clearFinished: () => {
      set((s) => ({ entries: s.entries.filter(isActive) }));
      save();
    },
    get: (id) => get().entries.find((e) => e.id === id),
  };
});

let wired = false;

/**
 * Cross-window sync and the heartbeat. Called once by the control that shows
 * the list; idempotent.
 */
export function wireTransfers(): void {
  if (wired || typeof window === "undefined") return;
  wired = true;
  // Persist whatever reapStale changed at load.
  writeStored(useTransfers.getState().entries);

  window.addEventListener("storage", (ev) => {
    if (ev.key !== STORAGE_KEY) return;
    const incoming = parse(ev.newValue);
    useTransfers.setState((s) => ({ entries: merge(s.entries, incoming, RUN_ID) }));
  });

  // While this window runs anything: beat, and save the progress so far. Also
  // reap entries whose owner window stopped beating.
  window.setInterval(() => {
    const now = Date.now();
    const { entries } = useTransfers.getState();
    const owned = entries.some((e) => e.owner === RUN_ID && isActive(e));
    const reaped = reapStale(entries, RUN_ID, now);
    const changed = reaped.some((e, i) => e !== entries[i]);
    if (!owned && !changed) return;
    useTransfers.setState({
      entries: reaped.map((e) => (e.owner === RUN_ID && isActive(e) ? { ...e, seenAt: now } : e)),
    });
    writeStored(useTransfers.getState().entries);
  }, HEARTBEAT_MS);
}
