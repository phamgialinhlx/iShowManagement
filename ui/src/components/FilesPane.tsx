import { useEffect, useRef, useState } from "react";

import type { TargetRef } from "../lib/api";
import { CodeEditor, disposeBufferModel, findInOpenFile, revealLine } from "./CodeEditor";
import { FileTree } from "./FileTree";
import { FileSearch } from "./FileSearch";
import { BinaryPreview, MarkdownPreview, previewKind } from "./FilePreview";
import { useWorkspace } from "../lib/workspace";
import { isDirty, type Buffer } from "../lib/buffers";
import { measureScale, widthFromPointer, TREE_DEFAULT, TREE_MIN, TREE_MAX } from "../lib/pane-resize";

/**
 * A Project's files pane — tree on the left, editor on the right.
 *
 * The v3 successor to the old `FilesView`, which lived inside `SessionView` and
 * was keyed by session. Files are **per project** now (ADR-001), so this is
 * keyed by `projectId`; the target and root come from the Project's Server.
 */

const TREE_KEY = "rmux.treeWidth";

const readSize = (key: string, fallback: number) => {
  const raw = Number(localStorage.getItem(key));
  return Number.isFinite(raw) && raw > 0 ? raw : fallback;
};

function EditorTabs({ projectId }: { projectId: string }) {
  const openOrder = useWorkspace((s) => s.openOrder[projectId]);
  const buffers = useWorkspace((s) => s.buffers);
  const active = useWorkspace((s) => s.activeBuffer[projectId]);
  const activate = useWorkspace((s) => s.activateBuffer);
  const close = useWorkspace((s) => s.closeBuffer);

  if (!openOrder?.length) return null;

  return (
    <div className="flex shrink-0 overflow-x-auto border-b" style={{ borderColor: "var(--border)" }}>
      {openOrder.map((key) => {
        const buffer = buffers[key];
        if (!buffer) return null;
        const dirty = isDirty(buffer);
        const isActive = key === active;
        const label = buffer.path.split("/").filter(Boolean).pop() ?? buffer.path;

        return (
          <div
            key={key}
            className="flex shrink-0 items-center gap-2 border-r px-3 py-[6px]"
            style={{
              borderColor: "var(--border)",
              background: isActive ? "var(--hover)" : "transparent",
              boxShadow: isActive ? "inset 0 -1px 0 var(--text)" : "none",
            }}
          >
            <button
              type="button"
              onClick={() => activate(projectId, key)}
              title={buffer.path}
              className="data text-[11px]"
              style={{ color: isActive ? "var(--text)" : "var(--text-soft)" }}
            >
              {label}
            </button>
            <button
              type="button"
              aria-label={`Close ${label}`}
              className="grid h-[12px] w-[12px] place-items-center"
              style={{ color: dirty ? "rgb(var(--busy))" : "var(--text-faint)" }}
              onClick={() => {
                close(key);
                disposeBufferModel(key);
              }}
            >
              {dirty ? (
                <svg width="7" height="7" viewBox="0 0 8 8" aria-hidden="true">
                  <circle cx="4" cy="4" r="4" fill="currentColor" />
                </svg>
              ) : (
                <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="square" aria-hidden="true">
                  <path d="M18 6L6 18M6 6l12 12" />
                </svg>
              )}
            </button>
          </div>
        );
      })}
    </div>
  );
}

/** Show a file the way it wants to be shown; source is one click away. */
function FileBody({ target, buffer }: { target: TargetRef; buffer: Buffer }) {
  const save = useWorkspace((s) => s.save);
  const edit = useWorkspace((s) => s.edit);
  const [showSource, setShowSource] = useState(false);

  const kind = previewKind(buffer.path);
  const canPreview =
    kind !== "none" &&
    !buffer.error &&
    (kind !== "markdown" || (!buffer.loading && buffer.content?.kind === "text"));

  const body =
    canPreview && !showSource ? (
      kind === "markdown" ? (
        <MarkdownPreview text={buffer.text} />
      ) : (
        <BinaryPreview target={target} path={buffer.path} />
      )
    ) : (
      <CodeEditor buffer={buffer} onSave={() => void save(buffer.key)} onEdit={edit} />
    );

  return (
    <div className="flex h-full flex-col">
      {canPreview && (
        <div className="flex shrink-0 items-center gap-1 border-b px-2 py-1" style={{ borderColor: "var(--border)" }}>
          <div className="seg">
            {(["preview", "source"] as const).map((mode) => {
              const isSource = mode === "source";
              if (isSource && kind !== "markdown") return null;
              return (
                <button key={mode} type="button" aria-pressed={showSource === isSource} onClick={() => setShowSource(isSource)}>
                  {mode}
                </button>
              );
            })}
          </div>
          <span className="micro ml-auto">{kind}</span>
        </div>
      )}
      <div className="min-h-0 flex-1">{body}</div>
    </div>
  );
}

export function FilesPane({ projectId }: { projectId: string }) {
  const project = useWorkspace((s) => s.projects.find((p) => p.id === projectId));
  const target = useWorkspace((s) => s.targetOfProject(projectId));
  const buffers = useWorkspace((s) => s.buffers);
  const activeKey = useWorkspace((s) => s.activeBuffer[projectId]);
  const openFile = useWorkspace((s) => s.openFile);
  const restoreFiles = useWorkspace((s) => s.restoreFiles);

  useEffect(() => restoreFiles(projectId), [projectId, restoreFiles]);

  const [treeWidth, setTreeWidth] = useState(() => readSize(TREE_KEY, TREE_DEFAULT));
  const [dragging, setDragging] = useState(false);
  const [searching, setSearching] = useState(false);
  const pane = useRef<HTMLDivElement>(null);

  const active = activeKey ? buffers[activeKey] : null;
  const root = project?.folder ?? "";

  useEffect(() => {
    if (!dragging) return;
    const el = pane.current;
    if (!el) return;
    const left = el.getBoundingClientRect().left;
    const scale = measureScale(el);
    const onMove = (e: MouseEvent) => setTreeWidth(widthFromPointer(e.clientX, left, scale));
    const onUp = () => setDragging(false);
    const previous = document.body.style.cursor;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      document.body.style.cursor = previous;
      document.body.style.userSelect = "";
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [dragging]);

  useEffect(() => localStorage.setItem(TREE_KEY, String(treeWidth)), [treeWidth]);

  // ⌘⇧F opens project search; ⌘F asks Monaco to find within the open file (only
  // the fallback for when the caret is not already in the editor).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "f") return;
      if (e.shiftKey) {
        e.preventDefault();
        setSearching(true);
        return;
      }
      if (document.activeElement?.closest(".monaco-editor")) return;
      e.preventDefault();
      findInOpenFile();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div ref={pane} className="flex h-full min-h-0">
      <div className="flex shrink-0 flex-col overflow-hidden py-2" style={{ width: treeWidth }}>
        {searching ? (
          <FileSearch
            target={target}
            root={root}
            onClose={() => setSearching(false)}
            onOpen={(path, line) => {
              void openFile(projectId, path).then(() => {
                requestAnimationFrame(() => requestAnimationFrame(() => revealLine(line)));
              });
            }}
          />
        ) : (
          <FileTree
            target={target}
            root={root}
            selected={active?.path ?? null}
            onSelect={(path) => void openFile(projectId, path)}
          />
        )}
      </div>

      <div
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the file tree"
        aria-valuenow={treeWidth}
        aria-valuemin={TREE_MIN}
        aria-valuemax={TREE_MAX}
        tabIndex={0}
        className="group flex w-[9px] shrink-0 cursor-col-resize items-stretch justify-center"
        onMouseDown={() => setDragging(true)}
        onDoubleClick={() => setTreeWidth(TREE_DEFAULT)}
        onKeyDown={(e) => {
          const step = e.shiftKey ? 40 : 8;
          if (e.key === "ArrowLeft") {
            e.preventDefault();
            setTreeWidth((w) => Math.max(w - step, TREE_MIN));
          } else if (e.key === "ArrowRight") {
            e.preventDefault();
            setTreeWidth((w) => Math.min(w + step, TREE_MAX));
          }
        }}
      >
        <div
          className="w-[3px] transition-colors group-hover:!bg-[var(--text-faint)]"
          style={{ background: dragging ? "rgb(var(--primary))" : "var(--border)" }}
        />
      </div>

      <div className="flex min-w-0 flex-1 flex-col">
        <EditorTabs projectId={projectId} />
        <div className="min-h-0 flex-1">
          {active ? (
            <FileBody target={target} buffer={active} />
          ) : (
            <div className="grid h-full place-items-center">
              <span className="micro">select a file</span>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
