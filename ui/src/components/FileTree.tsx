import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { api, type TargetRef } from "../lib/api";
import { describe, filesFrom, uploadFiles } from "../lib/upload";
import {
  absPath,
  childrenOf,
  lazyState,
  refreshLazy,
  relPath,
  reopen,
  useMirror,
  type Mirror,
} from "../lib/worktree";
import { anchoredScrollTop, parentOf, visibleRows, type Entry, type Row } from "../lib/worktree-model";
import { PanelLoader } from "./PanelLoader";
import { TreeMenu, type MenuTarget } from "./TreeMenu";

/**
 * The file tree, drawn from the worktree mirror (`lib/worktree.ts`).
 *
 * On a host running `fs-serve` the whole tree is already here, so expanding a
 * folder costs nothing and a file an agent creates appears by itself. On the
 * shell path (Windows, an older agent) folders list on demand, as they always
 * did — the header says which, so the state is readable without clicking.
 *
 * ## Virtualised, and anchored
 *
 * Only the rows in view are rendered: a checkout can hold a quarter of a
 * million entries and an expanded `src/` alone can be thousands. And because
 * the tree now changes *by itself*, the viewport is anchored to the first
 * visible row — a file appearing above the fold must not push everything the
 * operator is looking at down a line. Nothing moves under their hands.
 */

const ROW = 22;
/** Rows rendered beyond the viewport on each side, so a fast scroll does not
 *  flash blank before the next render. */
const OVERSCAN = 12;

/** Expanded folders, per mirror. Module scope: the pane unmounts whenever the
 *  layout changes, and re-collapsing the operator's tree every time is the
 *  kind of state loss that reads as the app forgetting. */
const expandedByMirror = new Map<string, Set<string>>();

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

function statusLabel(m: Mirror): { text: string; title: string; alert: boolean } {
  switch (m.status) {
    case "opening":
      return { text: "reading…", title: "Reading the folder tree", alert: false };
    case "live":
      if (m.truncated)
        return {
          text: "live · partial",
          title: "This folder is very large: the deepest directories list when opened",
          alert: false,
        };
      if (m.watchLimited)
        return {
          text: "live · limited",
          title: "The host would not watch every folder (its inotify limit), so some changes may not appear on their own",
          alert: false,
        };
      return { text: "live", title: "Changes on disk appear here by themselves", alert: false };
    case "down":
      return { text: "reconnecting", title: "The connection dropped; the tree will catch up when it returns", alert: false };
    case "onDemand":
      return {
        text: "on demand",
        title: `Folders list when opened and do not update by themselves${m.reason ? ` — ${m.reason}` : ""}`,
        alert: false,
      };
    case "error":
      return { text: "error", title: m.error ?? "", alert: true };
  }
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
  const [home, setHome] = useState<string | null>(rootProp || null);
  const [homeError, setHomeError] = useState<string | null>(null);
  useEffect(() => {
    if (rootProp) {
      setHome(rootProp);
      return;
    }
    let cancelled = false;
    api
      .fsHome(target)
      .then((h) => !cancelled && setHome(h))
      .catch((e) => !cancelled && setHomeError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [target, rootProp]);

  const m = useMirror(target, home);
  const [expandedRev, setExpandedRev] = useState(0);
  const expanded = useMemo(() => {
    if (!m) return new Set<string>();
    let set = expandedByMirror.get(m.key);
    if (!set) expandedByMirror.set(m.key, (set = new Set()));
    return set;
  }, [m]);

  const rows = useMemo<Row[]>(
    () => (m ? visibleRows((dir) => childrenOf(m, dir), expanded) : []),
    // `m.rev` is the mirror's change counter; `expandedRev` the local one.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [m, m?.rev, expandedRev, expanded],
  );

  const toggle = (entry: Entry) => {
    if (expanded.has(entry.path)) expanded.delete(entry.path);
    else expanded.add(entry.path);
    setExpandedRev((n) => n + 1);
  };

  // ── virtualisation and the scroll anchor ────────────────────────────────
  const scroller = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(600);
  const anchor = useRef<{ path: string; offset: number; index: number } | null>(null);

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewport(el.clientHeight || 600));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const recordAnchor = (top: number) => {
    const index = Math.floor(top / ROW);
    const row = rows[index];
    anchor.current = row && top > 0 ? { path: row.entry.path, offset: index * ROW - top, index } : null;
  };

  // After the rows change, put the anchored row back where it was.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const next = anchoredScrollTop(rows, anchor.current, ROW);
    if (next !== null && Math.abs(next - el.scrollTop) >= 1) {
      el.scrollTop = next;
      setScrollTop(el.scrollTop);
    }
    recordAnchor(el.scrollTop);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows]);

  const first = Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN);
  const last = Math.min(rows.length, Math.ceil((scrollTop + viewport) / ROW) + OVERSCAN);

  // ── menu, drop target, uploads ──────────────────────────────────────────
  const [menu, setMenu] = useState<MenuTarget | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const [uploading, setUploading] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<{ text: string; failed: boolean } | null>(null);
  const root = m?.root ?? home;

  /** After a create/rename/delete/upload. Live: the watcher already has it —
   *  only an on-demand listing of that folder needs fetching again. */
  const changed = (parentAbs: string) => {
    if (!m) return;
    const rel = relPath(m, parentAbs);
    if (m.status === "onDemand" || (rel !== null && m.lazy.has(rel))) refreshLazy(m, rel ?? undefined);
  };

  const refresh = () => {
    if (!m) return;
    if (m.status === "onDemand") reopen(m);
    else refreshLazy(m);
  };

  const dirUnder = (node: EventTarget | null): string | null =>
    (node instanceof Element ? node.closest("[data-drop-dir]")?.getAttribute("data-drop-dir") : null) ?? root;

  const receive = async (dir: string, files: File[]) => {
    setOutcome(null);
    const result = await uploadFiles(target, dir, files, (done, total, name) =>
      setUploading(total > 1 ? `${done + 1}/${total} · ${name}` : name),
    );
    setUploading(null);
    // Refreshed whenever anything landed, even if part of the batch failed —
    // hiding the files that did arrive is worse than the failure itself.
    if (result.uploaded.length) changed(dir);
    setOutcome({ text: describe(result), failed: result.failed.length > 0 });
    // Successes fade; failures stay until the next attempt.
    if (!result.failed.length) setTimeout(() => setOutcome(null), 2500);
  };

  const status = m ? statusLabel(m) : null;
  const rootListing = m ? lazyState(m, "") : undefined;
  const loadingRoot = !m || m.status === "opening" || (rows.length === 0 && m.status === "live" && !m.scanned);
  const error = homeError ?? rootListing?.error ?? null;

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
        <span className="flex shrink-0 items-center gap-2">
          {status && (
            <span
              className="micro"
              title={status.title}
              style={{ color: status.alert ? "rgb(var(--primary))" : "var(--text-faint)" }}
            >
              {status.text}
            </span>
          )}
          <button type="button" className="chip" onClick={refresh} style={{ color: "var(--text-soft)" }}>
            refresh
          </button>
        </span>
      </header>

      <div
        ref={scroller}
        className="min-h-0 flex-1 overflow-auto"
        onScroll={(e) => {
          const top = e.currentTarget.scrollTop;
          setScrollTop(top);
          recordAnchor(top);
        }}
        style={{
          outline: dropTarget !== null && dropTarget === root ? "1px solid rgb(var(--primary))" : "none",
          outlineOffset: "-1px",
        }}
      >
        {error && (
          <p role="alert" className="data px-2 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}

        {loadingRoot && !error ? (
          // Never a blank pane: say what is being read and from where.
          <PanelLoader
            variant="rows"
            phase="READING THE TREE"
            detail={`${target.host ?? "this machine"} · ${root ?? "home"}`}
            rows={8}
          />
        ) : rows.length === 0 && !error ? (
          <p className="micro px-2 py-1">empty folder</p>
        ) : (
          <div style={{ height: rows.length * ROW, position: "relative" }}>
            {rows.slice(first, last).map((row, i) => (
              <TreeRow
                key={row.entry.path}
                m={m!}
                row={row}
                top={(first + i) * ROW}
                open={expanded.has(row.entry.path)}
                selected={selected}
                dropTarget={dropTarget}
                onToggle={toggle}
                onSelect={onSelect}
                onContext={setMenu}
              />
            ))}
          </div>
        )}
      </div>

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
          onChanged={changed}
        />
      )}
    </div>
  );
}

function TreeRow({
  m,
  row,
  top,
  open,
  selected,
  dropTarget,
  onToggle,
  onSelect,
  onContext,
}: {
  m: Mirror;
  row: Row;
  top: number;
  open: boolean;
  selected: string | null;
  dropTarget: string | null;
  onToggle: (e: Entry) => void;
  onSelect: (path: string) => void;
  onContext: (menu: MenuTarget) => void;
}) {
  const { entry, depth } = row;
  const path = absPath(m, entry.path);
  const parent = absPath(m, parentOf(entry.path));
  const isDir = entry.kind === "dir";
  const isSelected = selected === path;
  // A drop on a file means the folder it lives in — which is what anyone
  // aiming at a file in a list means.
  const dropsInto = isDir ? path : parent;
  const receiving = dropTarget !== null && dropTarget === dropsInto;
  const listing = isDir && open ? lazyState(m, entry.path) : undefined;
  const name = entry.path.slice(entry.path.lastIndexOf("/") + 1);

  return (
    <button
      type="button"
      onClick={() => (isDir ? onToggle(entry) : onSelect(path))}
      data-drop-dir={dropsInto}
      onContextMenu={(e) => {
        e.preventDefault();
        onContext({ x: e.clientX, y: e.clientY, path, isDirectory: isDir, parent });
      }}
      title={listing?.error ?? undefined}
      className="absolute left-0 right-0 flex items-center gap-1.5 px-2 text-left"
      style={{
        top,
        height: ROW,
        paddingLeft: 8 + depth * 12,
        background: receiving || isSelected ? "var(--hover)" : "transparent",
        // Ignored entries (node_modules, build output) recede rather than vanish:
        // they are real files, just not the project's own.
        color: entry.ignored ? "var(--text-faint)" : entry.kind === "symlink" ? "var(--text-soft)" : "var(--text)",
        boxShadow: receiving ? "inset 2px 0 0 0 rgb(var(--primary))" : "none",
      }}
    >
      {isDir ? <Chevron open={open} /> : <span style={{ width: 10, flexShrink: 0 }} />}
      <span className="data truncate text-[12px]">{name}</span>
      {listing?.loading && <span className="micro ml-auto">listing…</span>}
      {listing?.error && (
        <span className="micro ml-auto truncate" style={{ color: "rgb(var(--primary))" }}>
          {listing.error}
        </span>
      )}
    </button>
  );
}
