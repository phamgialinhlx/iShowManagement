import { useCallback, useEffect, useState } from "react";

import { api, type DirEntry, type TargetRef } from "../lib/api";
import { describe, filesFrom, uploadFiles } from "../lib/upload";
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
  dropTarget,
}: {
  target: TargetRef;
  path: string;
  entry: DirEntry;
  depth: number;
  selected: string | null;
  onSelect: (path: string) => void;
  onContext: (menu: MenuTarget) => void;
  refreshToken: number;
  /** Folder a drag is currently over, so the row can show it will receive it. */
  dropTarget: string | null;
}) {
  const [open, setOpen] = useState(false);
  const [children, setChildren] = useState<DirEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const isDir = entry.kind === "directory";
  const isSelected = selected === path;
  // Where a file dropped on *this* row would land. A drop on a file means the
  // folder it lives in — which is what anyone aiming at a file in a list means,
  // and saying so is what stops the drop being a guess.
  const dropsInto = isDir ? path : path.slice(0, path.lastIndexOf("/")) || "/";
  const receiving = dropTarget !== null && dropTarget === dropsInto;

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
        // Read by the tree's drop handler to work out where the pointer is.
        // Kept on the element rather than in React state because `dragover`
        // fires continuously and hit-testing from the event target costs
        // nothing, while a state write per event would re-render the whole tree.
        data-drop-dir={dropsInto}
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
          background: receiving || isSelected ? "var(--hover)" : "transparent",
          color: entry.kind === "symlink" ? "var(--text-soft)" : "var(--text)",
          // The destination is marked, not merely tinted: at a glance a hover
          // tint and a selection tint are the same thing, and dropping eight
          // files into the wrong folder is not a mistake you notice quickly.
          boxShadow: receiving ? "inset 2px 0 0 0 rgb(var(--primary))" : "none",
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
            dropTarget={dropTarget}
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
  dropTarget,
}: {
  target: TargetRef;
  parent: string;
  entry: DirEntry;
  depth: number;
  selected: string | null;
  onSelect: (path: string) => void;
  onContext: (menu: MenuTarget) => void;
  refreshToken: number;
  dropTarget: string | null;
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
      dropTarget={dropTarget}
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

  // Dropping files in. The folder under the pointer is tracked so the tree can
  // *show* where a release would land — a drag with no visible destination is a
  // guess, and this one writes to a machine somewhere else.
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const [uploading, setUploading] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<{ text: string; failed: boolean } | null>(null);

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

  const refresh = useCallback(() => {
    invalidateListings(target);
    setEntries(null);
    setRefreshToken((n) => n + 1);
    void load();
  }, [target, load]);

  /**
   * Which folder the pointer is over.
   *
   * Hit-tested from the event target rather than tracked in state per row: a
   * `dragover` fires many times a second, and a listener on every row of a deep
   * tree is a listener per file. Falling back to the root means the empty space
   * below the last entry is a valid destination too, which is where people aim
   * when they mean "just put it in the project".
   */
  const dirUnder = (node: EventTarget | null): string | null =>
    (node instanceof Element ? node.closest("[data-drop-dir]")?.getAttribute("data-drop-dir") : null) ??
    root;

  const receive = async (dir: string, files: File[]) => {
    setOutcome(null);
    const result = await uploadFiles(target, dir, files, (done, total, name) =>
      setUploading(total > 1 ? `${done + 1}/${total} · ${name}` : name),
    );
    setUploading(null);

    // Refreshed whenever anything landed, even if part of the batch failed —
    // hiding the files that did arrive is worse than the failure itself.
    if (result.uploaded.length) refresh();

    setOutcome({ text: describe(result), failed: result.failed.length > 0 });
    // Successes fade; failures stay until the next attempt.
    if (!result.failed.length) setTimeout(() => setOutcome(null), 2500);
  };

  return (
    <div
      className="flex h-full flex-col"
      onDragOver={(e) => {
        // Only files. A drag of selected text across the tree must not offer to
        // write it somewhere.
        if (!e.dataTransfer.types.includes("Files")) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "copy";
        const dir = dirUnder(e.target);
        // Compared before setting: this fires continuously, and an unconditional
        // write would re-render the whole tree on every pointer move.
        setDropTarget((current) => (current === dir ? current : dir));
      }}
      onDragLeave={(e) => {
        // `dragleave` also fires when crossing into a child element, which would
        // make the highlight flicker off and on across every row.
        if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
        setDropTarget(null);
      }}
      onDrop={(e) => {
        const dir = dirUnder(e.target);
        setDropTarget(null);
        if (!dir) return;

        const files = filesFrom(e.dataTransfer);
        e.preventDefault();
        if (!files.length) {
          // A drag from a web page carries a URL or some HTML, not a file, and
          // looks identical while dragging. Saying so beats a drop that appears
          // to have been accepted and did nothing.
          setOutcome({ text: "That drag carried no files", failed: true });
          return;
        }
        void receive(dir, files);
      }}
    >
      <header className="flex items-center justify-between gap-2 px-2 pb-2">
        <span className="micro truncate" title={root ?? undefined}>
          {root ?? "…"}
        </span>
        <button type="button" className="micro" onClick={refresh} style={{ color: "var(--text-soft)" }}>
          refresh
        </button>
      </header>

      <div
        className="min-h-0 flex-1 overflow-auto"
        style={{
          // The root is a destination too — the space under the last row is
          // where a drop aimed at "the project" lands, so it has to look like
          // it will take one.
          outline: dropTarget !== null && dropTarget === root ? "1px solid rgb(var(--primary))" : "none",
          outlineOffset: "-1px",
        }}
      >
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
              dropTarget={dropTarget}
            />
          ))}
      </div>

      {/* One line, at the bottom of the tree, saying what a drop is doing or
          did. It occupies no space when there is nothing to say — a permanently
          reserved status row is a permanently empty one. */}
      {(dropTarget !== null || uploading || outcome) && (
        <div
          className="shrink-0 border-t px-2 py-[4px]"
          style={{ borderColor: "var(--border)" }}
          role={outcome?.failed ? "alert" : "status"}
        >
          {uploading ? (
            <div className="flex items-center gap-2">
              <div className="flex h-[10px] items-end gap-[2px]" aria-hidden="true">
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
              </div>
              <span className="micro truncate">uploading {uploading}</span>
            </div>
          ) : outcome ? (
            <span
              className="micro truncate"
              style={{ color: outcome.failed ? "rgb(var(--primary))" : "var(--text-soft)" }}
            >
              {outcome.text}
            </span>
          ) : (
            // Named while the drag is still in the air. This is the sentence
            // that makes a drop deliberate rather than hopeful.
            <span className="micro truncate">drop into {dropTarget}</span>
          )}
        </div>
      )}

      {menu && (
        <TreeMenu
          menu={menu}
          target={target}
          root={root ?? ""}
          onClose={() => setMenu(null)}
          onChanged={refresh}
        />
      )}
    </div>
  );
}
