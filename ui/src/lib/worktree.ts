import { useEffect, useSyncExternalStore } from "react";

import { api, type TargetRef, type WorktreeEvent, type WorktreeSnapshot } from "./api";
import { listen } from "./bridge";
import { Tree, type Entry, type FileChange } from "./worktree-model.ts";

/**
 * Live worktree mirrors, one per (server, folder) — the runtime half of
 * `worktree-model.ts`.
 *
 * Opened once, kept for the life of the window: the file tree, quick open and
 * the open editors all read the same mirror, and a pane that unmounts (the
 * layout changed) must not throw away a snapshot that took a network round
 * trip to build. Rust holds its own copy, so a page reload re-joins rather than
 * re-scans.
 *
 * ## Two modes, one interface
 *
 * - **live** — `fs-serve` is running there: the whole tree is here, patches
 *   arrive from its watcher, and only ignored directories list on demand.
 * - **on demand** — the shell path (Windows, an older agent): every directory
 *   lists when it is first expanded, as before, and nothing updates by itself.
 *
 * Callers ask `childrenOf` either way and never learn which mode they are in,
 * apart from the label the tree shows so the state is readable without
 * clicking.
 */

export type MirrorStatus = "opening" | "live" | "onDemand" | "down" | "error";

type Lazy = { entries: Entry[] | null; loading: boolean; error: string | null };

export type Mirror = {
  key: string;
  target: TargetRef;
  root: string;
  status: MirrorStatus;
  error: string | null;
  /** Why this host is on the shell path, when it is. */
  reason: string | null;
  tree: Tree;
  scanned: boolean;
  truncated: boolean;
  watchLimited: boolean;
  /** On-demand listings: lazy (ignored) directories, or everything on demand. */
  lazy: Map<string, Lazy>;
  /** Bumped on every change, for subscribers. */
  rev: number;
  id: number | null;
  seq: number;
};

const mirrors = new Map<string, Mirror>();
const byId = new Map<number, Mirror>();
/** Events for a mirror whose snapshot has not resolved yet. */
const early = new Map<number, WorktreeEvent[]>();
const subscribers = new Map<string, Set<() => void>>();
const fileListeners = new Set<(target: TargetRef, abs: string, version: string | null) => void>();
const gitListeners = new Set<(target: TargetRef, root: string) => void>();

export const targetKey = (t: TargetRef) => `${t.host ?? "local"}|${t.user ?? ""}|${t.port ?? ""}`;

const trimRoot = (root: string) => (root.length > 1 ? root.replace(/\/+$/, "") : root);

export const mirrorKey = (target: TargetRef, root: string) => `${targetKey(target)}\0${trimRoot(root)}`;

/** Absolute path of a mirror-relative one. Joined here, not over IPC — the
 *  separator is `/` on every host rmux drives, Windows' Git bash included. */
export function absPath(m: Pick<Mirror, "root">, rel: string): string {
  if (!rel) return m.root;
  return m.root.endsWith("/") ? m.root + rel : `${m.root}/${rel}`;
}

/** The mirror-relative path of an absolute one, or null when it is outside. */
export function relPath(m: Pick<Mirror, "root">, abs: string): string | null {
  if (abs === m.root) return "";
  const prefix = m.root.endsWith("/") ? m.root : `${m.root}/`;
  return abs.startsWith(prefix) ? abs.slice(prefix.length) : null;
}

function bump(m: Mirror) {
  m.rev++;
  for (const fn of subscribers.get(m.key) ?? []) fn();
}

function notifyFiles(m: Mirror, changes: FileChange[]) {
  if (!changes.length || !fileListeners.size) return;
  for (const c of changes) {
    const abs = absPath(m, c.path);
    for (const fn of fileListeners) fn(m.target, abs, c.version);
  }
}

function applyEvent(m: Mirror, ev: WorktreeEvent) {
  if (ev.seq <= m.seq) return;
  m.seq = ev.seq;
  switch (ev.kind) {
    case "patch": {
      const changes = m.tree.apply(ev);
      // A lazy listing under a path that just changed is stale.
      for (const u of ev.upserts ?? []) if (u.kind === "dir" && !u.lazy) m.lazy.delete(u.path);
      if (m.status === "down") m.status = "live";
      notifyFiles(m, changes);
      break;
    }
    case "scanned":
      m.scanned = true;
      m.truncated = ev.truncated;
      m.watchLimited = ev.watchLimited;
      if (m.status === "down") m.status = "live";
      break;
    case "down":
      m.status = "down";
      break;
    case "git":
      for (const fn of gitListeners) fn(m.target, m.root);
      return;
  }
  bump(m);
}

let listening: Promise<unknown> | null = null;
function ensureListening() {
  listening ??= listen<WorktreeEvent>("fs-worktree", ({ payload }) => {
    const m = byId.get(payload.id);
    if (m) {
      applyEvent(m, payload);
      return;
    }
    // Not claimed yet — the snapshot reply may still be in flight. Bounded,
    // so ids no window ever claims (another window's mirrors) cannot grow.
    const queue = early.get(payload.id) ?? [];
    if (queue.length < 5000) queue.push(payload);
    early.set(payload.id, queue);
    if (early.size > 64) early.delete(early.keys().next().value!);
  });
}

function adopt(m: Mirror, snap: WorktreeSnapshot) {
  m.id = snap.id;
  m.seq = snap.seq;
  m.tree = new Tree();
  m.tree.apply({ upserts: snap.entries });
  m.scanned = snap.scanned;
  m.truncated = snap.truncated;
  m.watchLimited = snap.watchLimited;
  m.status = "live";
  m.error = null;
  byId.set(snap.id, m);
  for (const ev of early.get(snap.id) ?? []) applyEvent(m, ev);
  early.delete(snap.id);
  bump(m);
}

/** Retries for a mirror that could not go live for a passing reason. */
const RETRY_MS = [15_000, 30_000, 60_000];

async function open(m: Mirror, attempt = 0) {
  ensureListening();
  await listening;
  try {
    adopt(m, await api.fsWorktreeOpen(m.target, m.root));
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    // A background retry that fails again changes nothing on screen.
    if (attempt > 0) {
      if (attempt < RETRY_MS.length) setTimeout(() => void open(m, attempt + 1), RETRY_MS[attempt]);
      return;
    }
    if (message.startsWith("unsupported")) {
      m.status = "onDemand";
      m.reason = message.replace(/^unsupported:\s*/, "");
    } else if (/unknown command|not a registered command|no such command/i.test(message)) {
      // An older backend without the command at all.
      m.status = "onDemand";
      m.reason = "this build lists folders on demand";
    } else {
      // Could not reach the server's file service right now; the tree still
      // works on demand, says why it is not live, and tries again quietly —
      // going live later swaps the source under the same rows.
      m.status = "onDemand";
      m.reason = message;
      setTimeout(() => void open(m, 1), RETRY_MS[0]);
    }
    bump(m);
  }
}

/** The mirror for a folder, opening it on first use. */
export function mirrorFor(target: TargetRef, root: string): Mirror {
  const key = mirrorKey(target, root);
  let m = mirrors.get(key);
  if (!m) {
    m = {
      key,
      target,
      root: trimRoot(root),
      status: "opening",
      error: null,
      reason: null,
      tree: new Tree(),
      scanned: false,
      truncated: false,
      watchLimited: false,
      lazy: new Map(),
      rev: 0,
      id: null,
      seq: 0,
    };
    mirrors.set(key, m);
    void open(m);
  }
  return m;
}

/** Every mirror already open on a target — for editors matching a path. */
export function mirrorsOn(target: TargetRef): Mirror[] {
  const k = targetKey(target);
  return [...mirrors.values()].filter((m) => targetKey(m.target) === k);
}

function subscribe(key: string, fn: () => void): () => void {
  let set = subscribers.get(key);
  if (!set) subscribers.set(key, (set = new Set()));
  set.add(fn);
  return () => set!.delete(fn);
}

/** Subscribe a component to a mirror; returns it and its revision. */
export function useMirror(target: TargetRef, root: string | null): Mirror | null {
  const m = root === null ? null : mirrorFor(target, root);
  useSyncExternalStore(
    (fn) => (m ? subscribe(m.key, fn) : () => {}),
    () => m?.rev ?? -1,
  );
  return m;
}

/** Hear about files changing on disk, anywhere a mirror watches. */
export function onFileChange(fn: (target: TargetRef, abs: string, version: string | null) => void): () => void {
  fileListeners.add(fn);
  return () => fileListeners.delete(fn);
}

/** Hear that something under a worktree's `.git` moved (HEAD, the index). */
export function onGitChange(fn: (target: TargetRef, root: string) => void): () => void {
  gitListeners.add(fn);
  return () => gitListeners.delete(fn);
}

/** The latest version the watcher reported for an absolute path, if any. */
export function versionOf(target: TargetRef, abs: string): string | null {
  for (const m of mirrorsOn(target)) {
    if (m.status !== "live") continue;
    const rel = relPath(m, abs);
    if (rel === null) continue;
    const e = m.tree.entries.get(rel);
    if (e?.kind === "file") return e.version;
  }
  return null;
}

const kindOf = (k: string): Entry["kind"] => (k === "directory" ? "dir" : k === "symlink" ? "symlink" : "file");

/**
 * Children of a directory, from the mirror or from an on-demand listing.
 * `null` while a listing is in flight (the row shows it is loading).
 */
export function childrenOf(m: Mirror, dir: string): Entry[] | null {
  const parent = dir ? m.tree.entries.get(dir) : null;
  // On demand: the whole tree on the shell path; a lazy (ignored or past the
  // cap) directory; or anything below one, which the mirror never held.
  const onDemand = m.status === "onDemand" || (parent?.lazy ?? false) || (dir !== "" && !parent);
  if (!onDemand && (m.status === "live" || m.status === "down")) return m.tree.children(dir);
  if (m.status === "opening" && !dir) return null;
  const cached = m.lazy.get(dir);
  if (cached?.entries) return cached.entries;
  if (!cached) {
    // Called during render: record the fetch as started *without* notifying,
    // and start it after the render — a subscriber firing mid-render is a
    // React error, and the loading row is already what this returns.
    m.lazy.set(dir, { entries: null, loading: true, error: null });
    queueMicrotask(() => void fetchLazy(m, dir));
  }
  return null;
}

/** An entry from the mirror or from an on-demand listing. */
export function entryAt(m: Mirror, rel: string): Entry | undefined {
  const slash = rel.lastIndexOf("/");
  const parent = slash < 0 ? "" : rel.slice(0, slash);
  return m.tree.entries.get(rel) ?? m.lazy.get(parent)?.entries?.find((e) => e.path === rel);
}

export function lazyState(m: Mirror, dir: string): Lazy | undefined {
  return m.lazy.get(dir);
}

/**
 * List one directory on demand. A listing already on screen stays there while
 * the new one is fetched — a refresh is not a first load, and replacing a good
 * answer with a loading row is the pane flickering under the operator.
 */
export async function listLazy(m: Mirror, dir: string): Promise<void> {
  const prev = m.lazy.get(dir);
  if (prev?.loading) return;
  m.lazy.set(dir, { entries: prev?.entries ?? null, loading: true, error: null });
  bump(m);
  await fetchLazy(m, dir);
}

async function fetchLazy(m: Mirror, dir: string): Promise<void> {
  const prev = m.lazy.get(dir);
  try {
    const listed = await api.fsList(m.target, absPath(m, dir));
    // Anything inside an ignored directory is ignored too.
    const parentIgnored = dir ? (entryAt(m, dir)?.ignored ?? false) : false;
    const entries: Entry[] = listed.map((d) => ({
      path: dir ? `${dir}/${d.name}` : d.name,
      kind: kindOf(d.kind),
      size: 0,
      version: "",
      ignored: parentIgnored,
      lazy: kindOf(d.kind) === "dir",
    }));
    m.lazy.set(dir, { entries, loading: false, error: null });
  } catch (e) {
    // Reported on the row itself — a permission error on one folder must not
    // look like an empty folder, nor blank the whole tree.
    m.lazy.set(dir, { entries: prev?.entries ?? null, loading: false, error: e instanceof Error ? e.message : String(e) });
  }
  bump(m);
}

/** Forget on-demand listings so they are fetched again. The live tree is
 *  never thrown away — it is current by construction. */
export function refreshLazy(m: Mirror, dir?: string) {
  const dirs = dir === undefined ? [...m.lazy.keys()] : [dir];
  for (const d of dirs) {
    const cur = m.lazy.get(d);
    // Mark it settled so `listLazy` will run it again; keep what is shown.
    if (cur) m.lazy.set(d, { ...cur, loading: false });
    void listLazy(m, d);
  }
  if (!dirs.length) bump(m);
}

/** Re-try opening a mirror that fell back, e.g. after a reconnect. */
export function reopen(m: Mirror) {
  // The on-demand listings stay on screen while it tries — a refresh is not a
  // first load.
  refreshLazy(m);
  void open(m, 1);
}

/** A hook that runs `fn` whenever any file under any mirror changes on disk. */
export function useFileChanges(fn: (target: TargetRef, abs: string, version: string | null) => void) {
  useEffect(() => onFileChange(fn), [fn]);
}
