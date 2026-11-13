import type { FileContent } from "./api";

/**
 * Open file buffers — the editor's in-memory model of a file.
 *
 * Moved out of the v2 `sessions.ts` (retired) so the editor and the dirty-count
 * can depend on the *type* without the old store. The buffer **store** (loading,
 * saving, per-project keying) lands in Phase 2 with the files pane; this file is
 * just the shared shape and its two pure helpers.
 *
 * Keyed by **project + path** in v3 (was session + path): a Project's file tree
 * is shared by all its sessions, and two projects with a file at the same path —
 * `src/main.rs` in two checkouts — must not alias, or an edit in one saves into
 * the other. That is data loss, not a cosmetic bug.
 */

export type BufferKey = string;

export type Buffer = {
  key: BufferKey;
  /** The Project this file belongs to. */
  projectId: string;
  path: string;
  saved: string;
  text: string;
  content: FileContent | null;
  loading: boolean;
  error: string | null;
  saving: boolean;
  saveError: string | null;
  /**
   * The version token the file was read (or last saved) at — its mtime as Rust
   * reports it. Quoted on save so a file changed on disk since is refused
   * rather than overwritten. `null`/absent on the shell path, which cannot
   * vouch for one.
   */
  version?: string | null;
  /** The disk moved under this buffer and the operator has to choose. */
  disk?: DiskState | null;
};

/**
 * Why the editor is showing *changed on disk — reload / keep mine*.
 *
 * Never resolved silently in either direction: reloading would discard the
 * operator's edits, saving would discard the agent's.
 */
export type DiskState =
  /** A newer copy is on disk and this buffer has unsaved edits. */
  | { kind: "changed"; version: string }
  /** A save was refused because the file changed since it was read. */
  | { kind: "conflict" }
  /** The file is gone. */
  | { kind: "deleted" };

/**
 * Buffers are keyed by project, not by path alone — see the note above.
 *
 * NUL is the separator for the same reason `rmux-fs` uses it for records: it is
 * the one byte a POSIX path cannot contain, so no real project id or path can
 * forge a key belonging to another pair.
 *
 * **Written as `\0`, never as a literal NUL byte.** It was one, and a NUL in the
 * source makes git classify the whole file as binary — no diff, ever, on a file
 * whose one job is deciding which buffer an edit saves into. The escape produces
 * the identical string.
 */
export const bufferKey = (projectId: string, path: string): BufferKey =>
  `${projectId}\0${path}`;

export const isDirty = (b: Buffer): boolean =>
  b.content?.kind === "text" && b.text !== b.saved;
