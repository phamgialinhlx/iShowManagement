import { useEffect, useRef, useState } from "react";

import { api, type TargetRef } from "../lib/api";
import { MenuItem, MenuSurface } from "./Menu";
import { describe, uploadFiles } from "../lib/upload";

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


  // Which path was just copied, so the menu can say so where it happened.
  const [copied, setCopied] = useState<"full" | "relative" | null>(null);

  // Where an upload lands: into the folder that was clicked, or alongside the
  // file that was. Right-clicking a file and getting its *parent* is what
  // everyone means, and it is what "Upload here" has to be able to say.
  const dir = menu.isDirectory ? menu.path : menu.parent;

  const fileInput = useRef<HTMLInputElement>(null);
  const [uploading, setUploading] = useState<string | null>(null);
  const [uploaded, setUploaded] = useState<string | null>(null);
  const [downloading, setDownloading] = useState(false);
  /** The folder it landed in, once it has — reported in the label. */
  const [downloaded, setDownloaded] = useState<string | null>(null);

  const upload = async (files: File[]) => {
    if (!files.length) return;

    setError(null);
    setUploaded(null);
    const outcome = await uploadFiles(target, dir, files, (done, total, name) =>
      // Named and counted while it runs. A remote upload of a few megabytes is
      // seconds of nothing, and a menu that sits there is a menu that has hung.
      setUploading(total > 1 ? `${done + 1}/${total} · ${name}` : name),
    );
    setUploading(null);

    // The tree must show what is now there even when part of the batch failed —
    // refusing to refresh would hide the files that did arrive.
    if (outcome.uploaded.length) onChanged(dir);

    if (outcome.failed.length) {
      setError(describe(outcome));
      return;
    }
    setUploaded(describe(outcome));
    setTimeout(onClose, 1400);
  };

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

  /**
   * Take a copy of this file onto this machine.
   *
   * Nothing on the host changes, so unlike the mutating items this does not
   * refresh the tree or close the menu — it reports where the file went and
   * leaves that on screen to be read. Closing instantly would throw away the
   * one piece of information the operator needs.
   */
  const download = async () => {
    setDownloading(true);
    setError(null);
    try {
      const result = await api.fsDownload(target, menu.path);
      // The folder, not the whole path: the menu is 240px wide and the leaf name
      // is already the thing they right-clicked.
      const folder = result.path.slice(0, Math.max(0, result.path.lastIndexOf("/")));
      setDownloaded(folder.split("/").pop() || folder || "downloads");
    } catch (e) {
      // Persists until the next attempt. A download that failed silently is
      // indistinguishable from one that worked until the file is looked for.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setDownloading(false);
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
    <MenuSurface at={menu} onClose={onClose}>
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

          {/* Upload sits with the other two "put something here" actions,
              because that is what it is — the same operation with the bytes
              coming from this machine instead of from nothing. The label names
              the destination folder, since a right-click on a *file* uploads
              beside it, and guessing which is the kind of thing that makes
              someone drop eight files into the wrong directory. */}
          <MenuItem
            label={uploading ? `Uploading ${uploading}…` : uploaded ? uploaded : "Upload files…"}
            disabled={uploading !== null}
            hint={uploading || uploaded ? undefined : shortName(dir)}
            onClick={() => fileInput.current?.click()}
          />
          <input
            ref={fileInput}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              const picked = Array.from(e.target.files ?? []);
              // Cleared so picking the same file twice in a row still fires.
              e.target.value = "";
              void upload(picked);
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

          {/* The reverse of Upload, and it sits with the copy actions rather
              than the destructive ones because that is what it is: taking a
              copy of something, changing nothing on the host.

              **It reports the landing place in the label.** There is no save
              dialog — rmux has no dialog plugin, and Tauri rejects an ungranted
              plugin command *silently*, which is a failure this app has been
              bitten by before. So the file goes where the log export goes, to a
              folder a person can find, and the menu says where. "Check your
              downloads" is not an answer when the folder holds two hundred
              things. */}
          {!menu.isDirectory && (
            <MenuItem
              label={
                downloading
                  ? "Downloading…"
                  : downloaded
                    ? `Saved to ${downloaded}`
                    : "Download a copy"
              }
              hint={downloading || downloaded ? undefined : "to this machine"}
              disabled={downloading}
              onClick={() => void download()}
            />
          )}

          <hr className="hairline my-1" />
          <MenuItem
            label="Rename"
            onClick={() => {
              setValue(name);
              setPrompt("rename");
            }}
          />
          <MenuItem label="Delete" destructive onClick={() => setConfirming(true)} />

          {/* Errors from an upload have nowhere else to go — this branch has no
              form to hang them off. They persist until the next attempt. */}
          {error && (
            <p
              role="alert"
              className="data px-3 py-1 text-[10px]"
              style={{ color: "rgb(var(--primary))" }}
            >
              {error}
            </p>
          )}
        </div>
      )}
    </MenuSurface>
  );
}

/** The last path segment — enough to say where something lands without wrapping. */
function shortName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
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
