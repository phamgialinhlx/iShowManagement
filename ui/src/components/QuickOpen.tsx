import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { pieces, QUICK_OPEN_EVENT, rankFiles, type FileHit } from "../lib/quick-open";
import { isMac } from "../lib/shortcuts";
import { useWorkspace } from "../lib/workspace";
import { absPath, relPath, useMirror } from "../lib/worktree";
import { PanelLoader } from "./PanelLoader";

/**
 * Quick open — ⌘P (Ctrl+Shift+P off macOS): fuzzy-find a file in the current
 * project and open it.
 *
 * Instant because it never asks the network: it ranks the worktree mirror the
 * file tree already holds (`lib/worktree.ts`). On a host on the shell path
 * there is no mirror to rank, and the dialog says so rather than offering a
 * search that cannot work.
 *
 * - **Works from inside the editor.** `match` lets this one chord through a
 *   Monaco focus (Monaco binds nothing to ⌘P), so jumping file to file does not
 *   mean clicking out of the code first.
 * - **Enter during an IME composition is the IME's**, as in the switcher.
 * - **The list has a ceiling** (`MAX_RESULTS`) and says how many it left out.
 */
export function QuickOpen() {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const toggle = () => setOpen((o) => !o);
    window.addEventListener(QUICK_OPEN_EVENT, toggle);
    return () => window.removeEventListener(QUICK_OPEN_EVENT, toggle);
  }, []);
  if (!open) return null;
  return <QuickOpenDialog onClose={() => setOpen(false)} />;
}

/** Stable, so the selector above never returns a fresh object. */
const NO_TARGET: import("../lib/api").TargetRef = {};

function currentProject(): string | null {
  const s = useWorkspace.getState();
  const fromSession = s.activeSession ? s.projectOf(s.activeSession)?.id : undefined;
  return fromSession ?? s.layout.activeProject ?? s.projects[0]?.id ?? null;
}

function QuickOpenDialog({ onClose }: { onClose: () => void }) {
  const [projectId] = useState(currentProject);
  const project = useWorkspace((s) => s.projects.find((p) => p.id === projectId));
  const target = useWorkspace((s) => (projectId ? s.targetOfProject(projectId) : NO_TARGET));
  const openOrder = useWorkspace((s) => (projectId ? s.openOrder[projectId] : undefined));
  const buffers = useWorkspace((s) => s.buffers);
  const m = useMirror(target, project?.folder || null);

  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const returnFocus = useRef<Element | null>(document.activeElement);

  const paths = useMemo(
    () => (m ? m.tree.files().map((e) => e.path) : []),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [m, m?.rev],
  );
  const recent = useMemo(() => {
    if (!m) return [];
    return [...(openOrder ?? [])]
      .reverse()
      .map((k) => buffers[k]?.path)
      .map((p) => (p ? relPath(m, p) : null))
      .filter((p): p is string => !!p);
  }, [m, openOrder, buffers]);
  const { rows, total } = useMemo(() => rankFiles(paths, query, recent), [paths, query, recent]);
  const sel = Math.min(selected, Math.max(0, rows.length - 1));

  useEffect(() => input.current?.focus(), []);
  useEffect(() => setSelected(0), [query]);
  useLayoutEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-index="${sel}"]`)?.scrollIntoView({ block: "nearest" });
  }, [sel, rows]);

  const cancel = () => {
    onClose();
    const back = returnFocus.current;
    if (back instanceof HTMLElement && back.isConnected) requestAnimationFrame(() => back.focus());
  };

  const choose = (hit: FileHit | undefined) => {
    if (!hit || !m || !projectId) return;
    onClose();
    const s = useWorkspace.getState();
    s.openFiles(projectId);
    void s.openFile(projectId, absPath(m, hit.path));
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    const down = e.key === "ArrowDown" || (e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "n");
    const up = e.key === "ArrowUp" || (e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "p");
    if (down || up) {
      e.preventDefault();
      if (rows.length) setSelected((sel + (down ? 1 : -1) + rows.length) % rows.length);
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      choose(rows[sel]);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      if (query) setQuery("");
      else cancel();
      return;
    }
    // The same chord closes it again.
    const mod = isMac() ? e.metaKey : e.ctrlKey;
    if (mod && e.key.toLowerCase() === "p") {
      e.preventDefault();
      cancel();
    }
  };

  let body: React.ReactNode = null;
  if (!project) {
    body = <li className="micro px-3 py-3">open a project first</li>;
  } else if (!m || m.status === "opening" || (m.status === "live" && !m.scanned && !paths.length)) {
    body = (
      <li className="px-3 py-3">
        <PanelLoader variant="inline" phase="READING THE TREE" detail={`${target.host ?? "this machine"} · ${project.folder}`} />
      </li>
    );
  } else if (m.status === "onDemand") {
    body = (
      <li className="micro px-3 py-3" style={{ color: "var(--overlay1)" }}>
        this server lists folders on demand, so there is no file list to search
        {m.reason ? ` — ${m.reason}` : ""}. Project search ({isMac() ? "⌘⇧F" : "Ctrl+Shift+F"} in the files pane) still works.
      </li>
    );
  } else if (!rows.length) {
    body = (
      <li className="micro px-3 py-3" style={{ color: "var(--overlay1)" }}>
        {paths.length ? `no file matches “${query.trim()}”` : "no files in this project"}
      </li>
    );
  }

  return (
    <div
      className="fixed inset-0 z-[98] grid place-items-start justify-center pt-[12vh]"
      style={{ background: "color-mix(in srgb, var(--app-bg) 55%, transparent)" }}
      onMouseDown={cancel}
    >
      <div
        role="dialog"
        aria-label="Open a file"
        className="window flex max-h-[76vh] w-[min(640px,94vw)] flex-col overflow-y-auto"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 px-3 py-[10px]" style={{ borderBottom: "1px solid var(--surface1)" }}>
          <input
            ref={input}
            aria-label="Open a file in this project"
            aria-controls="quick-open-list"
            aria-activedescendant={rows[sel] ? `qo-${sel}` : undefined}
            className="data min-w-0 flex-1 bg-transparent text-[14px] outline-none"
            style={{ color: "var(--text)" }}
            placeholder={project ? `open a file in ${project.label}…` : "open a file…"}
            spellCheck={false}
            autoComplete="off"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
          />
        </div>

        {/* The ceiling is on the element that scrolls, per CLAUDE.md. */}
        <ul ref={list} id="quick-open-list" role="listbox" className="max-h-[56vh] overflow-y-auto py-1">
          {body ??
            rows.map((hit, i) => {
              const cut = hit.path.lastIndexOf("/") + 1;
              const name = hit.path.slice(cut);
              const dir = hit.path.slice(0, Math.max(0, cut - 1));
              return (
                <li
                  key={hit.path}
                  id={`qo-${i}`}
                  data-index={i}
                  role="option"
                  aria-selected={i === sel}
                  className="mx-1 flex cursor-default items-baseline gap-[10px] px-2 py-[5px]"
                  style={{ background: i === sel ? "var(--selection-bg)" : "transparent", borderRadius: "var(--radius)" }}
                  onMouseMove={(e) => {
                    if (i === sel || (e.movementX === 0 && e.movementY === 0)) return;
                    setSelected(i);
                  }}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => choose(hit)}
                >
                  <span className="data shrink-0 text-[13px]" style={{ color: "var(--text)" }}>
                    {pieces(name, hit.nameRanges).map((p, k) =>
                      p.hit ? (
                        <span key={k} style={{ color: "var(--accent)", fontWeight: 700 }}>
                          {p.text}
                        </span>
                      ) : (
                        <span key={k}>{p.text}</span>
                      ),
                    )}
                  </span>
                  <span className="data min-w-0 truncate text-[11px]" style={{ color: "var(--subtext0)" }}>
                    {pieces(dir, hit.dirRanges).map((p, k) =>
                      p.hit ? (
                        <span key={k} style={{ color: "var(--text)" }}>
                          {p.text}
                        </span>
                      ) : (
                        <span key={k}>{p.text}</span>
                      ),
                    )}
                  </span>
                </li>
              );
            })}
        </ul>

        <div
          className="micro flex items-center justify-between gap-3 px-3 py-[6px]"
          style={{ borderTop: "1px solid var(--surface1)", color: "var(--overlay1)" }}
        >
          <span>↑↓ move · ↵ open · esc close</span>
          {total > rows.length && (
            <span>
              {rows.length} of {total.toLocaleString()} — type to narrow
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
