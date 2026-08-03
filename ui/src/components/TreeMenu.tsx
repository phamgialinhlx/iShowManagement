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
  root,
  onClose,
  onChanged,
}: {
  menu: MenuTarget;
  target: TargetRef;
  /** The project root, so "copy relative path" has something to be relative to. */
  root: string;
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

  // Which path was just copied, so the menu can say so where it happened.
  const [copied, setCopied] = useState<"full" | "relative" | null>(null);

  const copy = async (value: string, which: "full" | "relative") => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(which);
      // Long enough to read, short enough that the menu is not left standing
      // open on top of the tree.
      setTimeout(onClose, 900);
    } catch (e) {
      setError(e instanceof Error ? e.message : "could not write to the clipboard");
    }
  };

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

          {/* Copying a path is the most-used thing in a file tree and the one
              nobody can do without a menu — you cannot select the text. Both
              forms are offered because both are asked for constantly: the full
              path to hand to a shell on that host, the relative one to paste
              into a message, an import, or a prompt.

              The label reports the outcome in place for a moment rather than
              closing instantly. A menu that vanishes on click leaves you
              wondering whether it copied, and the only way to check is to paste
              somewhere and look. */}
          <MenuItem
            label={copied === "full" ? "Copied full path" : "Copy full path"}
            onClick={() => void copy(menu.path, "full")}
          />
          <MenuItem
            label={copied === "relative" ? "Copied relative path" : "Copy relative path"}
            onClick={() => void copy(relative(menu.path, root), "relative")}
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


/**
 * The path as it reads from the project root.
 *
 * Falls back to the absolute path when the file is somehow outside the root —
 * an honest full path beats a mangled relative one, and `../../..` chains are
 * not what anyone means by "relative path".
 */
function relative(path: string, root: string): string {
  const base = root.endsWith("/") ? root : `${root}/`;
  return path.startsWith(base) ? path.slice(base.length) : path;
}
