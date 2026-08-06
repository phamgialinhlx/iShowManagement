import type { PaneRef, SessionV3 } from "./workspace-model";

/**
 * The pane-grid layout presets and the pure cell-assignment rules.
 *
 * Pure, and separate from the store, because the rules are not obvious and
 * getting them wrong is silent — a session that quietly stops being reachable
 * looks identical to one that was never there.
 */

/** A layout preset, rows × columns. The non-square shapes exist for ultrawide
 *  screens: "1x2"/"1x3" are one row split into columns, not columns of rows. */
export type GridLayoutId = "1x1" | "1x2" | "1x3" | "2x2" | "3x3" | "4x4";
export type GridDims = { rows: number; cols: number };

export const GRID_LAYOUTS: Record<GridLayoutId, GridDims> = {
  "1x1": { rows: 1, cols: 1 },
  "1x2": { rows: 1, cols: 2 },
  "1x3": { rows: 1, cols: 3 },
  "2x2": { rows: 2, cols: 2 },
  "3x3": { rows: 3, cols: 3 },
  "4x4": { rows: 4, cols: 4 },
};

/** Preset order — the order the VIEW control renders. */
export const GRID_LAYOUT_IDS = Object.keys(GRID_LAYOUTS) as GridLayoutId[];

export const gridDims = (layout: GridLayoutId): GridDims => GRID_LAYOUTS[layout];

export const gridCells = (layout: GridLayoutId): number =>
  GRID_LAYOUTS[layout].rows * GRID_LAYOUTS[layout].cols;

/** Focus mode — the single-pane layout with the warm-set machinery behind it.
 *  The one predicate: the old numeric model kept four independent copies of
 *  `grid === 1`, which is four chances to drift. */
export const isFocus = (layout: GridLayoutId): boolean => layout === "1x1";

/**
 * Read a persisted layout. Old builds stored a bare digit meaning N×N
 * ("2" → 2×2), so those map to their squares; anything unrecognised falls back
 * to focus mode rather than guessing a shape.
 */
export function parseGridLayout(raw: string | null): GridLayoutId {
  if (raw && raw in GRID_LAYOUTS) return raw as GridLayoutId;
  const n = Number(raw);
  if (n === 2 || n === 3 || n === 4) return `${n}x${n}` as GridLayoutId;
  return "1x1";
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
 * Pure and separate from the store, for the same reason as `layoutPanes`: the
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
 * Which **pane** belongs in which cell (a pane is a session, a host panel, a
 * project's files — or a tile cleared on purpose).
 *
 *  1. **An assignment wins.** A cell the operator filled shows that pane,
 *     wherever its session sits in the rail.
 *  2. **Empty cells auto-fill**, from sessions not already placed, **in stable
 *     rail order** — so a fresh grid is useful with no arranging at all. An
 *     `empty`-kind pane blocks auto-fill: it is a tile the operator cleared,
 *     and refilling it would make the clear control appear to do nothing.
 *  3. **No session appears twice.** Two cells holding one session would mount
 *     its view twice — two Claude panes attached to a single conversation, both
 *     writing to it.
 *
 * (A closed session does not hold a cell hostage either, but that is the
 * store's job: `removeSession` nulls any pane pointing at it.)
 *
 * The rail-order part is load-bearing and was once wrong. A grid must not
 * reshuffle when you merely click a pane to focus it — clicking calls
 * `activate()`, which changes `activeSession`, and an earlier version ordered
 * the auto-fill *active-first*. So every click yanked the clicked pane to the
 * top-left and displaced the one that was there, and a restart re-derived
 * positions from `activeSession` rather than from anything the operator saw.
 * Only **focus mode** is active-first, because its single cell must *be* the
 * active session; a grid is stable regardless of what is active.
 */
export function layoutPanes(
  panes: readonly (PaneRef | null)[],
  sessions: readonly Pick<SessionV3, "id">[],
  activeId: string | null,
  layout: GridLayoutId,
): (PaneRef | null)[] {
  const cells = gridCells(layout);
  const out: (PaneRef | null)[] = Array.from({ length: cells }, (_, i) => panes[i] ?? null);

  const shown = new Set(out.flatMap((p) => (p && p.kind === "session" ? [p.id] : [])));
  const order =
    isFocus(layout) && activeId
      ? [activeId, ...sessions.map((s) => s.id).filter((id) => id !== activeId)]
      : sessions.map((s) => s.id);
  const spare = order.filter((id) => !shown.has(id) && sessions.some((s) => s.id === id));

  for (let i = 0; i < cells && spare.length; i += 1) {
    if (!out[i]) out[i] = { kind: "session", id: spare.shift()! };
  }
  return out;
}

/**
 * The cell an "open into the deck" verb fills. The focused cell wins; with no
 * cell focused the first *empty* tile takes it — opening from the rail used to
 * land on cell 0 every time, silently displacing whatever was there — and only
 * a completely full deck falls back to cell 0.
 */
export function openTarget(
  cells: readonly (PaneRef | null)[],
  focusedCell: number | null,
): number {
  if (focusedCell !== null && focusedCell >= 0 && focusedCell < cells.length) return focusedCell;
  const empty = cells.findIndex((p) => !p || p.kind === "empty");
  return empty >= 0 ? empty : 0;
}

/**
 * The sessions actually on screen — derived from `layoutPanes`, never from the
 * raw `panes` array. The raw array both misses auto-filled cells (the common
 * case: `panes` starts empty) and keeps orphan entries beyond the visible cell
 * count, so a rail marker reading it drifts from what the deck shows.
 */
export function onScreenSessions(
  panes: readonly (PaneRef | null)[],
  sessions: readonly Pick<SessionV3, "id">[],
  activeId: string | null,
  layout: GridLayoutId,
): Set<string> {
  return new Set(
    layoutPanes(panes, sessions, activeId, layout).flatMap((p) =>
      p && p.kind === "session" ? [p.id] : [],
    ),
  );
}
