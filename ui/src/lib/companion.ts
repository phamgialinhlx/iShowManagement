/**
 * A shell that belongs to one Claude session.
 *
 * Claude proposes and you verify — so the terminal you check its work in wants
 * to be *the same folder, on the same machine*, sitting beside the conversation
 * rather than a tab away. Switching to another Claude session should bring that
 * session's own shell with it, not leave you in the last one's directory.
 *
 * ## Why it is not a session in the rail
 *
 * A companion has no independent existence: it is created for a conversation,
 * lives in that conversation's folder, and dies with it. Listing it beside real
 * sessions would double the rail for no decision — every Claude row would have
 * a shell row under it that you never choose between. It is a *view* of a
 * session, like the transcript, and it is reached the same way.
 *
 * ## But it is a real shell on the host, and that has a consequence
 *
 * It runs under `rmux-agent` like every other terminal, so it survives closing
 * the app and reattaches by name. Which means **removing the Claude session has
 * to kill it** — the same rule that makes `removeSession` send `terminal_close`
 * per tab. A companion nobody can reach is exactly the leak `rmux-agent list`
 * exists to make findable.
 */

/**
 * The agent session name for a Claude session's companion.
 *
 * Derived rather than minted, so it is the same name after a restart and the
 * shell is reattached instead of duplicated. The `companion-` prefix cannot
 * collide with a terminal session's own id (a ULID) or with `claude-<id>`.
 */
export const companionName = (sessionId: string): string => `companion-${sessionId}`;

/** Key into the workspace's `live` map, which is otherwise keyed by session id. */
export const companionKey = (sessionId: string): string => `companion:${sessionId}`;

const OPEN_KEY = (sessionId: string) => `rmux.companion.open.${sessionId}`;
const SPLIT_KEY = (sessionId: string) => `rmux.companion.split.${sessionId}`;

/** Fraction of the tile the *conversation* gets. The shell takes the rest. */
const DEFAULT_SPLIT = 0.62;

/**
 * Clamped so neither half can be dragged to nothing.
 *
 * A pane dragged to zero looks like it closed, and the handle that would bring
 * it back is then a 1px target — so the operator's recovery is to close and
 * reopen something they did not mean to close in the first place.
 */
export const clampSplit = (v: number): number => Math.min(0.85, Math.max(0.2, v));

export function readOpen(sessionId: string): boolean {
  try {
    return localStorage.getItem(OPEN_KEY(sessionId)) === "1";
  } catch {
    return false;
  }
}

export function writeOpen(sessionId: string, open: boolean): void {
  try {
    if (open) localStorage.setItem(OPEN_KEY(sessionId), "1");
    else localStorage.removeItem(OPEN_KEY(sessionId));
  } catch {
    /* a full localStorage must not stop a pane opening */
  }
}

export function readSplit(sessionId: string): number {
  try {
    const raw = Number(localStorage.getItem(SPLIT_KEY(sessionId)));
    return raw > 0 ? clampSplit(raw) : DEFAULT_SPLIT;
  } catch {
    return DEFAULT_SPLIT;
  }
}

export function writeSplit(sessionId: string, split: number): void {
  try {
    localStorage.setItem(SPLIT_KEY(sessionId), String(clampSplit(split)));
  } catch {
    /* ignore */
  }
}

/** Forget a session's companion preferences when the session goes. */
export function forget(sessionId: string): void {
  try {
    localStorage.removeItem(OPEN_KEY(sessionId));
    localStorage.removeItem(SPLIT_KEY(sessionId));
  } catch {
    /* ignore */
  }
}

/**
 * The split as a fraction, from a pointer position within the tile.
 *
 * Pure so the arithmetic can be tested without a DOM: the failure mode of a
 * drag handle is an off-by-one that only shows up at the edges, and edges are
 * exactly what a manual test does not reach.
 */
export function splitFromPointer(clientY: number, top: number, height: number): number {
  if (height <= 0) return DEFAULT_SPLIT;
  return clampSplit((clientY - top) / height);
}
