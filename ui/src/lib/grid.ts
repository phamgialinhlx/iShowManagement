import type { PaneRef, SessionV3 } from "./workspace-model";

/**
 * Which session belongs in which cell, for an NxN grid.
 *
 * Pure, and separate from the store, because the rules are not obvious and
 * getting them wrong is silent — a session that quietly stops being reachable
 * looks identical to one that was never there.
 *
 *  1. **An assignment wins.** A cell the operator filled shows that session,
 *     wherever it sits in the rail.
 *  2. **Empty cells auto-fill**, in rail order, from sessions not already
 *     placed. So a fresh grid is useful with no arranging at all, and a new
 *     session appears rather than waiting to be slotted in.
 *  3. **No session appears twice.** Two cells holding one session would mount
 *     its view twice — two Claude panes attached to a single conversation, both
 *     writing to it. A duplicate assignment is dropped, keeping the first.
 *  4. **A stale assignment does not hold a cell hostage.** A closed session's
 *     slot becomes empty and auto-fill may use it.
 */
export function gridLayout<S extends { id: string }>(
  sessions: S[],
  slots: readonly (string | null)[],
  grid: number,
): (S | null)[] {
  const cells = Math.max(0, grid * grid);
  const byId = new Map(sessions.map((s) => [s.id, s]));

  const placed = new Set<string>();
  const out: (S | null)[] = Array.from({ length: cells }, (_, i) => {
    const id = slots[i];
    if (!id || placed.has(id)) return null;
    const session = byId.get(id);
    if (!session) return null;
    placed.add(id);
    return session;
  });

  const spare = sessions.filter((s) => !placed.has(s.id));
  for (let i = 0; i < cells && spare.length; i += 1) {
    if (!out[i]) out[i] = spare.shift()!;
  }

  return out;
}

/**
 * Which sessions stay mounted in focus mode — the **warm set**.
 *
 * Focus mode shows one session at a time. Keeping *only* that one mounted would
 * tear down and rebuild a pane every time you glance at another session and come
 * back; keeping *every* session ever opened mounted — which is what this used to
 * do — is why RAM tracked everything opened this run rather than what you are
 * watching. Each live session holds an xterm plus a WebGL context per pane, so a
 * day of opening sessions accumulated dozens of them, hidden behind `display:none`.
 *
 * The compromise is a small warm set: the active session plus the most recently
 * looked-at ones stay live; everything past `keep` is **suspended** — its view
 * unmounts, disposing the xterm and its WebGL context. Returning to it remounts
 * and reattaches to the same shells and Claude under `rmux-agent` (scrollback
 * replayed), so nothing is lost — the shell and the conversation live on the
 * target, not in the pane. The rail keeps reporting a suspended session's status
 * regardless, because `status-watch` polls every session, not only mounted ones.
 *
 * Pure and separate from the store, for the same reason as `gridLayout`: the
 * rules are subtle and getting them wrong is silent.
 *
 *  1. **The active session is always warm, and first** — even before it has been
 *     recorded in `recent`.
 *  2. **Most-recently-active next**, so the sessions you alternate between stay
 *     instant to return to.
 *  3. **Deduplicated, and closed sessions dropped** — a stale id whose session no
 *     longer exists must not hold a warm slot.
 *  4. **Capped at `keep`.** Beyond it, sessions are suspended.
 */
export function warmSessions<S extends { id: string }>(
  active: string | null,
  recent: readonly string[],
  sessions: readonly S[],
  keep: number,
): S[] {
  const byId = new Map(sessions.map((s) => [s.id, s]));
  const seen = new Set<string>();
  const out: S[] = [];
  for (const id of [active, ...recent]) {
    if (!id || seen.has(id)) continue;
    seen.add(id);
    const session = byId.get(id);
    if (!session) continue;
    out.push(session);
    if (out.length >= keep) break;
  }
  return out;
}

/**
 * Which **pane** belongs in which cell — the pane-manager generalisation of
 * `gridLayout` (a pane is a session, a host panel, or a project's files).
 *
 * The rules match `gridLayout`: explicit panes win, empty cells auto-fill from
 * sessions not already shown, **in stable rail order**, and no session shows
 * twice.
 *
 * The rail-order part is load-bearing and was once wrong. A grid must not
 * reshuffle when you merely click a pane to focus it — clicking calls
 * `activate()`, which changes `activeSession`, and an earlier version ordered
 * the auto-fill *active-first*. So every click yanked the clicked pane to the
 * top-left and displaced the one that was there, and a restart re-derived
 * positions from `activeSession` rather than from anything the operator saw.
 * Only **focus mode** (`grid === 1`) is active-first, because its single cell
 * must *be* the active session; a grid is stable regardless of what is active.
 */
export function layoutPanes(
  panes: readonly (PaneRef | null)[],
  sessions: readonly Pick<SessionV3, "id">[],
  activeId: string | null,
  grid: number,
): (PaneRef | null)[] {
  const cells = Math.max(0, grid * grid);
  const out: (PaneRef | null)[] = Array.from({ length: cells }, (_, i) => panes[i] ?? null);

  const shown = new Set(out.flatMap((p) => (p && p.kind === "session" ? [p.id] : [])));
  const order =
    grid === 1 && activeId
      ? [activeId, ...sessions.map((s) => s.id).filter((id) => id !== activeId)]
      : sessions.map((s) => s.id);
  const spare = order.filter((id) => !shown.has(id) && sessions.some((s) => s.id === id));

  for (let i = 0; i < cells && spare.length; i += 1) {
    if (!out[i]) out[i] = { kind: "session", id: spare.shift()! };
  }
  return out;
}
