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
