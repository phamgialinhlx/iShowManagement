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
 * A deck shape: how many columns by how many rows.
 *
 * **This replaced a single number, and the number was the bug waiting to
 * happen.** `grid: n` could only ever mean an n x n square, so every layout was
 * as tall as it was wide. On a 2560-wide desktop a 2x2 gives four panes each
 * about 1200x650 — but a terminal is a *tall* thing, and what a wide screen
 * actually wants is panes side by side at full height. That shape was
 * unreachable: there is no n for which n x n is 1 row of 2.
 *
 * Columns first, rows second, because that is the axis a wide screen adds to.
 * `1x2` reads as one row of two.
 */
export type Deck = { cols: number; rows: number };

/** Cells in a deck. The one place `cols * rows` is spelled out. */
export const deckCells = (deck: Deck): number => Math.max(0, deck.cols * deck.rows);

/** Focus mode is a deck of exactly one cell, not a magic `grid === 1`. */
export const isFocusDeck = (deck: Deck): boolean => deckCells(deck) === 1;

/**
 * The decks the operator can pick, in the order they appear in the status bar.
 *
 * Squares first because they are what most people reach for, then the wide
 * ones. `1x2` and `1x4` exist for widescreen and are deliberately *not* offered
 * as `2x1`/`4x1`: a column of short, full-width terminals is worse than one
 * pane, since a terminal loses rows and gains nothing from the extra width.
 */
export const DECKS: readonly { id: string; label: string; deck: Deck; title: string }[] = [
  { id: "1", label: "1", deck: { cols: 1, rows: 1 }, title: "One pane at a time" },
  { id: "2x2", label: "2\u00d72", deck: { cols: 2, rows: 2 }, title: "Four panes, two by two" },
  { id: "3x3", label: "3\u00d73", deck: { cols: 3, rows: 3 }, title: "Nine panes" },
  { id: "4x4", label: "4\u00d74", deck: { cols: 4, rows: 4 }, title: "Sixteen panes" },
  { id: "1x2", label: "1\u00d72", deck: { cols: 2, rows: 1 }, title: "Two panes side by side, full height" },
  { id: "1x4", label: "1\u00d74", deck: { cols: 4, rows: 1 }, title: "Four panes side by side, full height" },
];

/** The id a deck is stored and compared by. */
export const deckId = (deck: Deck): string =>
  deckCells(deck) === 1 ? "1" : `${deck.rows}x${deck.cols}`;

/**
 * Read a stored deck.
 *
 * **Accepts the old bare number**, because that is what every existing install
 * has in `rmux.grid`: `"3"` meant 3x3. Dropping that would silently reset
 * everyone to focus mode on upgrade, which reads as losing your layout.
 */
export function parseDeck(raw: string | null | undefined): Deck {
  if (!raw) return { cols: 1, rows: 1 };
  const known = DECKS.find((d) => d.id === raw);
  if (known) return known.deck;
  const n = Number(raw);
  if (Number.isFinite(n) && n >= 1 && n <= 4) return { cols: n, rows: n };
  return { cols: 1, rows: 1 };
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
 * Only **focus mode** (a one-cell deck) is active-first, because its single cell
 * must *be* the active session; a grid is stable regardless of what is active.
 */
export function layoutPanes(
  panes: readonly (PaneRef | null)[],
  sessions: readonly Pick<SessionV3, "id">[],
  activeId: string | null,
  deck: Deck,
): (PaneRef | null)[] {
  const cells = deckCells(deck);
  const out: (PaneRef | null)[] = Array.from({ length: cells }, (_, i) => panes[i] ?? null);

  const shown = new Set(out.flatMap((p) => (p && p.kind === "session" ? [p.id] : [])));
  const order =
    isFocusDeck(deck) && activeId
      ? [activeId, ...sessions.map((s) => s.id).filter((id) => id !== activeId)]
      : sessions.map((s) => s.id);
  const spare = order.filter((id) => !shown.has(id) && sessions.some((s) => s.id === id));

  for (let i = 0; i < cells && spare.length; i += 1) {
    if (!out[i]) out[i] = { kind: "session", id: spare.shift()! };
  }
  return out;
}
