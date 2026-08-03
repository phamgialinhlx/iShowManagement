import { useCallback, useEffect, useState } from "react";

import { api, type DirEntry, type TargetRef } from "../lib/api";

/**
 * Browse a target's directories and pick one.
 *
 * Only folders are listed. You are choosing a project root, and showing files
 * would be offering hundreds of things that cannot be picked — noise that makes
 * the folders harder to find.
 *
 * Navigation is click-to-enter with a breadcrumb to climb back out, and the
 * currently open directory is always the one that gets opened. That ordering
 * matters: it means you can always see what you are about to choose, which is the
 * whole reason this replaced a text field you had to guess into.
 */

function FolderIcon() {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="square"
      aria-hidden="true"
      style={{ flexShrink: 0 }}
    >
      <path d="M3 6h6l2 2h10v11H3z" />
    </svg>
  );
}

function LinkIcon() {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="square"
      aria-hidden="true"
      style={{ flexShrink: 0 }}
    >
      <path d="M10 14a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-1 1" />
      <path d="M14 10a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l1-1" />
    </svg>
  );
}

/** Path split into clickable segments, each with the full path to jump to. */
function crumbs(path: string): { label: string; path: string }[] {
  const parts = path.split("/").filter(Boolean);
  const out = [{ label: "/", path: "/" }];
  let accumulated = "";
  for (const part of parts) {
    accumulated += `/${part}`;
    out.push({ label: part, path: accumulated });
  }
  return out;
}

export function FolderBrowser({
  target,
  initialPath,
  recents,
  onChoose,
  busy,
}: {
  target: TargetRef;
  initialPath: string;
  /** Folders previously opened on this target — the fastest way back to work. */
  recents: string[];
  onChoose: (path: string) => void;
  busy: boolean;
}) {
  const [path, setPath] = useState(initialPath);
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [showHidden, setShowHidden] = useState(false);

  const load = useCallback(
    async (next: string) => {
      setLoading(true);
      setError(null);
      try {
        const listed = await api.fsList(target, next);
        setEntries(listed);
        setPath(next);
      } catch (e) {
        // Stay where we are on failure — a permission error should not strand
        // the user in a directory they cannot see.
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    },
    [target],
  );

  useEffect(() => {
    void load(initialPath);
  }, [load, initialPath]);

  const folders = (entries ?? [])
    .filter((e) => e.kind === "directory" || e.kind === "symlink")
    .filter((e) => showHidden || !e.name.startsWith("."));

  const enter = async (name: string) => {
    const child = await api.fsJoin(path, name);
    void load(child);
  };

  const goUp = async () => {
    const parent = await api.fsParent(path);
    if (parent) void load(parent);
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      {/* Breadcrumb: shows exactly what will be opened, and climbs in one click. */}
      <div className="flex flex-wrap items-center gap-1">
        {crumbs(path).map((crumb, i, all) => (
          <span key={crumb.path} className="flex items-center gap-1">
            <button
              type="button"
              className="data text-[11px]"
              style={{ color: i === all.length - 1 ? "var(--text)" : "var(--text-soft)" }}
              onClick={() => void load(crumb.path)}
              disabled={loading}
            >
              {crumb.label}
            </button>
            {i < all.length - 1 && (
              <span className="micro" style={{ opacity: 0.5 }}>
                /
              </span>
            )}
          </span>
        ))}
        {loading && <span className="micro ml-1">…</span>}
      </div>

      <div className="flex items-center gap-3">
        <button
          type="button"
          className="chip"
          onClick={() => void goUp()}
          disabled={loading}
        >
          ↑ up
        </button>
        {/* Bordered and labelled, because two bare words side by side read as
            one control — "up hidden" rather than two separate actions. */}
        <button
          type="button"
          className="chip"
          aria-pressed={showHidden}
          onClick={() => setShowHidden((v) => !v)}
        >
          {showHidden ? "hiding nothing" : "show hidden"}
        </button>
        <span className="micro ml-auto">
          {folders.length} folder{folders.length === 1 ? "" : "s"}
        </span>
      </div>

      <div
        className="inset min-h-0 flex-1 overflow-y-auto"
        style={{ border: "1px solid var(--border)" }}
      >
        {error && (
          <p role="alert" className="data p-2 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}

        {folders.map((entry) => (
          <button
            key={entry.name}
            type="button"
            onClick={() => void enter(entry.name)}
            disabled={loading}
            className="flex w-full items-center gap-2 px-2 py-[5px] text-left"
            style={{ color: entry.kind === "symlink" ? "var(--text-soft)" : "var(--text)" }}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            {entry.kind === "symlink" ? <LinkIcon /> : <FolderIcon />}
            <span className="data truncate text-[12px]">{entry.name}</span>
          </button>
        ))}

        {!error && entries && folders.length === 0 && (
          <p className="micro p-2 leading-relaxed">
            no sub-folders here — open this one, or go up
          </p>
        )}
      </div>

      {recents.length > 0 && (
        <div className="flex flex-col gap-1">
          <span className="micro">Recent on this host</span>
          <div className="flex flex-wrap gap-1">
            {recents.slice(0, 5).map((recent) => (
              <button
                key={recent}
                type="button"
                className="data px-2 py-[3px] text-[11px]"
                style={{ background: "var(--hover)", color: "var(--text-soft)" }}
                onClick={() => void load(recent)}
                title={recent}
              >
                {recent.split("/").filter(Boolean).pop() ?? recent}
              </button>
            ))}
          </div>
        </div>
      )}

      {/* The current directory is what opens — always visible above the button,
          so there is never a question about what you are about to get. */}
      <button
        type="button"
        className="btn btn-primary w-full"
        onClick={() => onChoose(path)}
        disabled={busy || loading}
      >
        {busy ? "Opening…" : `Open ${path.split("/").filter(Boolean).pop() ?? path}`}
      </button>
    </div>
  );
}
