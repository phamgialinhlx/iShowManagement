import { useCallback, useEffect, useState } from "react";

import { api, type DirEntry, type TargetRef } from "../lib/api";
import { TreeMenu, type MenuTarget } from "./TreeMenu";

/**
 * A lazy directory tree.
 *
 * Children load on first expand and are cached in a module-scope map, so
 * collapsing and reopening a folder — or switching to another panel and back —
 * costs no round trip. That cache lives outside React on purpose: the panel
 * unmounts whenever the layout changes, and re-listing a deep remote tree every
 * time is exactly the latency this app exists to avoid.
 */

/** `target|path` → entries. Survives unmount; cleared when a target reconnects. */
const listingCache = new Map<string, DirEntry[]>();

const cacheKey = (target: TargetRef, path: string) => `${target.host ?? "local"}|${path}`;

export function invalidateListings(target?: TargetRef) {
  if (!target) {
    listingCache.clear();
    return;
  }
  const prefix = `${target.host ?? "local"}|`;
  for (const key of listingCache.keys()) {
    if (key.startsWith(prefix)) listingCache.delete(key);
  }
}

function Chevron({ open }: { open: boolean }) {
  // Inline SVG, Lucide-style, square caps — rule 3, no icon fonts or emoji.
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="square"
      style={{
        transform: open ? "rotate(90deg)" : "none",
        transition: "transform var(--dur) var(--ease)",
        flexShrink: 0,
      }}
      aria-hidden="true"
    >
      <path d="M9 18l6-6-6-6" />
    </svg>
  );
}

function Node({
  target,
  path,
  entry,
  depth,
  selected,
  onSelect,
  onContext,
  refreshToken,
}: {
  target: TargetRef;
  path: string;
  entry: DirEntry;
  depth: number;
  selected: string | null;
  onSelect: (path: string) => void;
  onContext: (menu: MenuTarget) => void;
  refreshToken: number;
}) {
  const [open, setOpen] = useState(false);
  const [children, setChildren] = useState<DirEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const isDir = entry.kind === "directory";
  const isSelected = selected === path;

  // Drop cached children when the tree is invalidated, so a folder reopened
  // after a create or delete shows what is actually there now.
  useEffect(() => {
    if (refreshToken > 0) setChildren(null);
  }, [refreshToken]);

  const toggle = async () => {
    if (!isDir) {
      onSelect(path);
      return;
    }

    if (open) {
      setOpen(false);
      return;
    }

    setOpen(true);
    if (children) return;

    const key = cacheKey(target, path);
    const cached = listingCache.get(key);
    if (cached) {
      setChildren(cached);
      return;
    }

    setLoading(true);
    setError(null);
    try {
      const entries = await api.fsList(target, path);
      listingCache.set(key, entries);
      setChildren(entries);
    } catch (e) {
      // Reported on the row itself — a permission error on one folder should
      // not look like an empty folder, nor blank the whole tree.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div>
      <button
        type="button"
        onClick={toggle}
        onContextMenu={(e) => {
          e.preventDefault();
          onContext({
            x: e.clientX,
            y: e.clientY,
            path,
            isDirectory: isDir,
            parent: path.slice(0, path.lastIndexOf("/")) || "/",
          });
        }}
        className="flex w-full items-center gap-1.5 px-2 py-[3px] text-left"
        style={{
          paddingLeft: 8 + depth * 12,
          background: isSelected ? "var(--hover)" : "transparent",
          color: entry.kind === "symlink" ? "var(--text-soft)" : "var(--text)",
        }}
      >
        {isDir ? <Chevron open={open} /> : <span style={{ width: 10, flexShrink: 0 }} />}
        <span className="data truncate text-[12px]">{entry.name}</span>
        {loading && <span className="micro ml-auto">…</span>}
      </button>

      {error && (
        <p
          className="data px-2 py-[2px] text-[10px]"
          style={{ paddingLeft: 20 + depth * 12, color: "rgb(var(--primary))" }}
        >
          {error}
        </p>
      )}

      {open &&
        children?.map((child) => (
          <LazyChild
            key={child.name}
            target={target}
            parent={path}
            entry={child}
            depth={depth + 1}
            selected={selected}
            onSelect={onSelect}
            onContext={onContext}
            refreshToken={refreshToken}
          />
        ))}
    </div>
  );
}

/** Resolves the child's full path through Rust, then renders it. */
function LazyChild({
  target,
  parent,
  entry,
  depth,
  selected,
  onSelect,
  onContext,
  refreshToken,
}: {
  target: TargetRef;
  parent: string;
  entry: DirEntry;
  depth: number;
  selected: string | null;
  onSelect: (path: string) => void;
  onContext: (menu: MenuTarget) => void;
  refreshToken: number;
}) {
  const [path, setPath] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.fsJoin(parent, entry.name).then((p) => {
      if (!cancelled) setPath(p);
    });
    return () => {
      cancelled = true;
    };
  }, [parent, entry.name]);

  if (!path) return null;

  return (
    <Node
      target={target}
      path={path}
      entry={entry}
      depth={depth}
      selected={selected}
      onSelect={onSelect}
      onContext={onContext}
      refreshToken={refreshToken}
    />
  );
}

export function FileTree({
  target,
  root: rootProp,
  selected,
  onSelect,
}: {
  target: TargetRef;
  /** The session's project folder. Falls back to the target's home. */
  root?: string;
  selected: string | null;
  onSelect: (path: string) => void;
}) {
  const [root, setRoot] = useState<string | null>(null);
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<MenuTarget | null>(null);
  // Bumped after a create/rename/delete so open folders re-list rather than
  // showing a stale directory.
  const [refreshToken, setRefreshToken] = useState(0);

  const load = useCallback(async () => {
    setError(null);
    try {
      // A session pins its own folder; only an unscoped tree falls back to home.
      const home = rootProp ?? (await api.fsHome(target));
      setRoot(home);

      const key = cacheKey(target, home);
      const cached = listingCache.get(key);
      if (cached) {
        setEntries(cached);
        return;
      }

      const listed = await api.fsList(target, home);
      listingCache.set(key, listed);
      setEntries(listed);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [target, rootProp]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between gap-2 px-2 pb-2">
        <span className="micro truncate" title={root ?? undefined}>
          {root ?? "…"}
        </span>
        <button
          type="button"
          className="micro"
          onClick={() => {
            invalidateListings(target);
            setEntries(null);
            setRefreshToken((n) => n + 1);
            void load();
          }}
          style={{ color: "var(--text-soft)" }}
        >
          refresh
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-auto">
        {error && (
          <p role="alert" className="data px-2 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}

        {root &&
          entries?.map((entry) => (
            <LazyChild
              key={entry.name}
              target={target}
              parent={root}
              entry={entry}
              depth={0}
              selected={selected}
              onSelect={onSelect}
              onContext={setMenu}
              refreshToken={refreshToken}
            />
          ))}
      </div>

      {menu && (
        <TreeMenu
          menu={menu}
          target={target}
          onClose={() => setMenu(null)}
          onChanged={() => {
            invalidateListings(target);
            setEntries(null);
            setRefreshToken((n) => n + 1);
            void load();
          }}
        />
      )}
    </div>
  );
}
