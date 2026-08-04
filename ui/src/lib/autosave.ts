/**
 * Saving an edit without being asked to.
 *
 * ⌘S is a habit from editors that open files on a local disk. rmux's files are
 * usually on someone else's machine, reached over SSH, and the buffer holding
 * your changes lives in a webview that a crash, a reload or a closed window
 * takes with it. "I typed it and it was gone" is the worst outcome this app has,
 * so the default is that typing is enough.
 *
 * ## Why a timer and not every keystroke
 *
 * A save is a **whole-file write over the network**. Firing one per character
 * would put a round trip on the keyboard, and on a slow link the writes would
 * queue behind each other faster than they drain. The timer collapses a burst of
 * typing into one write once you pause — which is also when a save is most
 * likely to represent a coherent state rather than half an identifier.
 *
 * {@link AUTOSAVE_DELAY} is deliberately longer than a local editor's. The cost
 * of one extra second of delay is that a crash loses one more second of typing;
 * the cost of being too eager is a write amplification the operator pays for on
 * every keystroke of a remote session.
 *
 * ## Why the timers live here and not in the editor component
 *
 * `FileBody` only mounts `CodeEditor` for the **active** buffer, so switching
 * tabs unmounts it. A timer owned by the component would be cleaned up by that
 * unmount and the pending edits would never be written — the operator would
 * have lost work by doing nothing more than looking at another file. Module
 * scope outlives every component that can be unmounted underneath it.
 *
 * The store's `edit` action drives this, which means every path that changes a
 * buffer's text schedules a save, including any added later.
 */

const KEY = "rmux.editor.autosave";

/**
 * How long typing has to stop before the file is written.
 *
 * Not configurable, on purpose. It is a latency/durability trade with one
 * defensible answer, and a number nobody can predict the effect of is a setting
 * that gets changed once and then blamed for something unrelated.
 */
export const AUTOSAVE_DELAY = 1200;

/**
 * How soon to look again when a write was already in flight.
 *
 * Short, because the edits it is coming back for are already made and the file
 * on disk is already stale — but not zero, or a slow remote write would be
 * chased by a tight loop of checks for as long as it takes.
 */
export const RETRY_DELAY = 300;

/**
 * **Absent means on.** A first run must autosave — that is the default the
 * operator asked for — and reading "no preference" as "off" would quietly opt
 * out everyone who has never opened Settings.
 */
export function autosaveEnabled(): boolean {
  return localStorage.getItem(KEY) !== "0";
}

export function setAutosaveEnabled(on: boolean): void {
  try {
    localStorage.setItem(KEY, on ? "1" : "0");
  } catch {
    // A full localStorage must not stop the editor working. Losing the
    // preference falls back to on, which is the safe direction.
  }
}

const timers = new Map<string, ReturnType<typeof setTimeout>>();

/**
 * Write `key` once typing stops.
 *
 * Each call replaces the previous timer for that buffer, so a burst of edits
 * costs one save rather than one per edit. Keyed per buffer: two files being
 * edited in different sessions must not cancel each other's save.
 */
export function scheduleAutosave(key: string, run: () => void, delay = AUTOSAVE_DELAY): void {
  cancelAutosave(key);
  timers.set(
    key,
    setTimeout(() => {
      timers.delete(key);
      run();
    }, delay),
  );
}

export function cancelAutosave(key: string): void {
  const timer = timers.get(key);
  if (timer !== undefined) {
    clearTimeout(timer);
    timers.delete(key);
  }
}

/** Whether a write is still owed for this buffer. Exposed for the checks. */
export function autosavePending(key: string): boolean {
  return timers.has(key);
}

/** Buffer state in which writing back would destroy the file. */
type Writable = {
  loading: boolean;
  error: string | null;
  content: { kind: string } | null;
  saving: boolean;
  text: string;
  saved: string;
};

/**
 * Whether this buffer may be written back right now.
 *
 * **Every branch here is a way to lose a file**, which is why the decision is
 * one pure function with its own tests rather than a chain of `&&` at a call
 * site:
 *
 * - `loading` — the read has not finished. The buffer's text is empty or
 *   partial, and writing it would truncate a file whose only crime was being
 *   opened.
 * - `error` — the read *failed*. Same emptiness, same truncation, and this time
 *   the operator has an error on screen telling them the file could not be
 *   read, while it gets overwritten behind it.
 * - not text — an image or a binary preview has no editable text to write.
 * - not dirty — nothing to say. Rewriting an unchanged file would still bump
 *   its mtime, which is enough to set off a file watcher or a build.
 *
 * `saving` is *not* a refusal, it is a **retry**: returning "no" outright would
 * drop the edits made during the in-flight write, which is exactly the silent
 * loss this whole module exists to prevent.
 */
export function autosaveDecision(b: Writable | undefined): "write" | "retry" | "skip" {
  if (!b) return "skip";
  if (b.loading || b.error || b.content?.kind !== "text") return "skip";
  if (b.text === b.saved) return "skip";
  return b.saving ? "retry" : "write";
}
