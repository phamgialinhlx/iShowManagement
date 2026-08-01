import { useEffect, useRef, useState } from "react";

import { api, type TargetRef } from "../lib/api";

export type MenuTarget = {
  x: number;
  y: number;
  path: string;
  isDirectory: boolean;
  /** Directory the new entry belongs in — the folder itself, or its parent. */
  parent: string;
};

/**
 * Right-click menu for the file tree.
 *
 * Destructive actions confirm first, and the confirmation names the file. A
 * delete that takes one click, on a tree where a mis-aimed right-click is easy,
 * is how people lose work over SSH — where there is no Trash to recover from.
 */
export function TreeMenu({
  menu,
  target,
  onClose,
  onChanged,
}: {
  menu: MenuTarget;
  target: TargetRef;
  onClose: () => void;
  onChanged: (parent: string) => void;
}) {
  const [prompt, setPrompt] = useState<null | "file" | "folder" | "rename">(null);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const name = menu.path.split("/").filter(Boolean).pop() ?? menu.path;

  useEffect(() => {
    if (prompt) inputRef.current?.focus();
  }, [prompt]);

  // Dismiss on outside click or Escape, like every context menu.
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest("[data-tree-menu]")) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const run = async (action: () => Promise<void>, refreshed: string) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      onChanged(refreshed);
      onClose();
    } catch (e) {
      // Stays visible until the next attempt — this is the only place the user
      // learns the operation failed.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const submitPrompt = async (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = value.trim();
    if (!trimmed) return;

    if (prompt === "rename") {
      const to = await api.fsJoin(menu.parent, trimmed);
      await run(() => api.fsRename(target, menu.path, to), menu.parent);
      return;
    }

    const dir = menu.isDirectory ? menu.path : menu.parent;
    const path = await api.fsJoin(dir, trimmed);
    await run(
      () => (prompt === "folder" ? api.fsCreateDir(target, path) : api.fsCreateFile(target, path)),
      dir,
    );
  };

  return (
    <div
      data-tree-menu
      className="menu fixed z-[80] min-w-[190px] p-1"
      style={{ left: menu.x, top: menu.y }}
    >
      {prompt ? (
        <form onSubmit={submitPrompt} className="flex flex-col gap-2 p-2">
          <span className="micro">
            {prompt === "rename" ? "Rename to" : prompt === "folder" ? "New folder" : "New file"}
          </span>
          <input
            ref={inputRef}
            className="field"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            spellCheck={false}
            autoComplete="off"
          />
          <div className="flex gap-2">
            <button className="btn btn-primary flex-1" type="submit" disabled={busy || !value.trim()}>
              {busy ? "Working…" : "OK"}
            </button>
            <button className="btn" type="button" onClick={onClose}>
              Cancel
            </button>
          </div>
          {error && (
            <p role="alert" className="data text-[10px]" style={{ color: "rgb(var(--primary))" }}>
              {error}
            </p>
          )}
        </form>
      ) : confirming ? (
        <div className="flex flex-col gap-2 p-2">
          {/* Naming the file is the point: it is what catches a mis-aimed click. */}
          <p className="data text-[11px]">
            Delete <span style={{ color: "rgb(var(--primary))" }}>{name}</span>
            {menu.isDirectory ? " and everything inside it" : ""}?
          </p>
          <p className="micro">this cannot be undone</p>
          <div className="flex gap-2">
            <button
              className="btn btn-primary flex-1"
              type="button"
              disabled={busy}
              onClick={() => void run(() => api.fsDelete(target, menu.path), menu.parent)}
            >
              {busy ? "Deleting…" : "Delete"}
            </button>
            <button className="btn" type="button" onClick={onClose}>
              Cancel
            </button>
          </div>
          {error && (
            <p role="alert" className="data text-[10px]" style={{ color: "rgb(var(--primary))" }}>
              {error}
            </p>
          )}
        </div>
      ) : (
        <div className="flex flex-col">
          <MenuItem
            label="New file"
            onClick={() => {
              setValue("");
              setPrompt("file");
            }}
          />
          <MenuItem
            label="New folder"
            onClick={() => {
              setValue("");
              setPrompt("folder");
            }}
          />
          <hr className="hairline my-1" />
          <MenuItem
            label="Rename"
            onClick={() => {
              setValue(name);
              setPrompt("rename");
            }}
          />
          <MenuItem label="Delete" destructive onClick={() => setConfirming(true)} />
        </div>
      )}
    </div>
  );
}

function MenuItem({
  label,
  onClick,
  destructive,
}: {
  label: string;
  onClick: () => void;
  destructive?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="data px-3 py-[5px] text-left text-[11px]"
      style={{ color: destructive ? "rgb(var(--primary))" : "var(--text)" }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {label}
    </button>
  );
}
