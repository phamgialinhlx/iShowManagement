/**
 * The worktree mirror's pure half — the tree, its patches, the child index and
 * the visible rows. `worktree.ts` wires it to Rust; `ui/worktree-check.ts` pins
 * it under Node, so nothing here may import the store or the bridge.
 *
 * ## Why the tree lives here at all
 *
 * Zed's remote model: the host sends the whole worktree once, then a stream of
 * changes. Holding it in the webview is what makes expanding a folder and
 * fuzzy-finding a file instant — neither ever crosses the network again.
 *
 * ## Paths
 *
 * Relative to the root, `/`-separated, exactly as Rust sends them. A path is an
 * opaque key here: it is split on `/` (which no filename can contain) and
 * concatenated, never re-encoded — so a name carrying an escaped non-UTF-8 byte
 * goes back to Rust exactly as it came.
 */

export type EntryKind = "file" | "dir" | "symlink";

export type Entry = {
  path: string;
  kind: EntryKind;
  size: number;
  version: string;
  ignored: boolean;
  lazy: boolean;
};

export type Patch = {
  upserts?: Entry[];
  removes?: string[];
  renames?: { from: string; to: string }[];
};

/** A file whose bytes on disk changed, for open editors. `version` null = gone. */
export type FileChange = { path: string; version: string | null };

export const parentOf = (rel: string): string => {
  const i = rel.lastIndexOf("/");
  return i < 0 ? "" : rel.slice(0, i);
};

export const nameOf = (rel: string): string => rel.slice(rel.lastIndexOf("/") + 1);

/**
 * Directories first, then names case-insensitively — the order the shell path
 * and `LocalFs` already use, so switching backends does not reshuffle a tree
 * the operator has learnt.
 */
export function compareEntries(a: Entry, b: Entry): number {
  const ad = a.kind === "dir" ? 0 : 1;
  const bd = b.kind === "dir" ? 0 : 1;
  if (ad !== bd) return ad - bd;
  const an = nameOf(a.path);
  const bn = nameOf(b.path);
  const al = an.toLowerCase();
  const bl = bn.toLowerCase();
  if (al !== bl) return al < bl ? -1 : 1;
  return an < bn ? -1 : an > bn ? 1 : 0;
}

export class Tree {
  readonly entries = new Map<string, Entry>();
  /** parent → its children's paths. Maintained incrementally. */
  private readonly kids = new Map<string, Set<string>>();
  /** parent → sorted children, rebuilt only for a parent that changed. */
  private readonly sorted = new Map<string, Entry[]>();

  get size(): number {
    return this.entries.size;
  }

  private link(rel: string) {
    const parent = parentOf(rel);
    let set = this.kids.get(parent);
    if (!set) this.kids.set(parent, (set = new Set()));
    if (!set.has(rel)) {
      set.add(rel);
      this.sorted.delete(parent);
    }
  }

  private unlinkSubtree(rel: string) {
    const stack = [rel];
    while (stack.length) {
      const cur = stack.pop()!;
      const children = this.kids.get(cur);
      if (children) for (const c of children) stack.push(c);
      this.kids.delete(cur);
      this.sorted.delete(cur);
      this.entries.delete(cur);
    }
    const parent = parentOf(rel);
    if (this.kids.get(parent)?.delete(rel)) this.sorted.delete(parent);
  }

  upsert(entry: Entry): Entry | undefined {
    const old = this.entries.get(entry.path);
    if (old && old.kind === "dir" && entry.kind !== "dir") {
      // A directory replaced by a file: its subtree is gone.
      for (const c of [...(this.kids.get(entry.path) ?? [])]) this.unlinkSubtree(c);
    }
    this.entries.set(entry.path, entry);
    this.link(entry.path);
    if (old && compareEntries(old, entry) !== 0) this.sorted.delete(parentOf(entry.path));
    return old;
  }

  remove(rel: string) {
    this.unlinkSubtree(rel);
  }

  rename(from: string, to: string) {
    const moved: Entry[] = [];
    const stack = [from];
    while (stack.length) {
      const cur = stack.pop()!;
      const e = this.entries.get(cur);
      if (e) moved.push(e);
      for (const c of this.kids.get(cur) ?? []) stack.push(c);
    }
    this.unlinkSubtree(from);
    for (const e of moved) {
      const path = to + e.path.slice(from.length);
      this.upsert({ ...e, path });
    }
  }

  /**
   * Apply one patch; report the files whose contents may have changed, so an
   * open editor can decide whether to reload.
   */
  apply(patch: Patch): FileChange[] {
    const changed: FileChange[] = [];
    for (const { from, to } of patch.renames ?? []) {
      const was = this.entries.get(from);
      if (was?.kind === "file") {
        changed.push({ path: from, version: null });
        // Reported for the destination too: write-to-temp-then-rename is how
        // most tools save, and an editor open on `to` must see that as a change
        // even though no upsert of `to` follows with a *different* version.
        changed.push({ path: to, version: was.version });
      }
      this.rename(from, to);
    }
    for (const rel of patch.removes ?? []) {
      // Every file under a removed directory is gone too.
      const stack = [rel];
      while (stack.length) {
        const cur = stack.pop()!;
        if (this.entries.get(cur)?.kind === "file") changed.push({ path: cur, version: null });
        for (const c of this.kids.get(cur) ?? []) stack.push(c);
      }
      this.remove(rel);
    }
    for (const entry of patch.upserts ?? []) {
      const old = this.upsert(entry);
      if (entry.kind === "file" && old && old.version !== entry.version) {
        changed.push({ path: entry.path, version: entry.version });
      }
    }
    return changed;
  }

  children(dir: string): Entry[] {
    let list = this.sorted.get(dir);
    if (!list) {
      list = [...(this.kids.get(dir) ?? [])].map((p) => this.entries.get(p)!).filter(Boolean);
      list.sort(compareEntries);
      this.sorted.set(dir, list);
    }
    return list;
  }

  /** Every file, for quick open. Ignored files and anything under a lazy
   *  directory are not in the tree to begin with, except ignored *files*. */
  files(includeIgnored = false): Entry[] {
    const out: Entry[] = [];
    for (const e of this.entries.values()) {
      if (e.kind === "dir") continue;
      if (e.ignored && !includeIgnored) continue;
      out.push(e);
    }
    return out;
  }
}

/** One row of the flattened, visible tree. */
export type Row = { entry: Entry; depth: number };

/**
 * The rows on screen: children of the root, and of every expanded directory,
 * depth-first. `childrenOf` answers for lazy directories too (from an
 * on-demand listing), returning `null` while one is still being fetched.
 */
export function visibleRows(
  childrenOf: (dir: string) => Entry[] | null,
  expanded: ReadonlySet<string>,
): Row[] {
  const rows: Row[] = [];
  const walk = (dir: string, depth: number) => {
    for (const entry of childrenOf(dir) ?? []) {
      rows.push({ entry, depth });
      if (entry.kind === "dir" && expanded.has(entry.path)) walk(entry.path, depth + 1);
    }
  };
  walk("", 0);
  return rows;
}

/**
 * Where to put `scrollTop` after the rows changed, so the row the operator was
 * looking at stays exactly where it was.
 *
 * An agent creating a file *above* the viewport would otherwise push every
 * visible row down by one — the list moving under the operator's hands. The
 * anchor is the first visible row and its offset into the viewport; if it was
 * removed, the nearest surviving row above it stands in.
 */
export function anchoredScrollTop(
  rows: readonly Row[],
  anchor: { path: string; offset: number; index: number } | null,
  rowHeight: number,
): number | null {
  if (!anchor) return null;
  let index = rows.findIndex((r) => r.entry.path === anchor.path);
  if (index < 0) index = Math.min(anchor.index, Math.max(0, rows.length - 1));
  return Math.max(0, index * rowHeight - anchor.offset);
}
