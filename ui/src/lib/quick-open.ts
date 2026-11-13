import { fuzzy, toRanges, type Range } from "./quick-switch.ts";

/**
 * Quick open (⌘P) — the **pure** half: path-aware fuzzy ranking over the
 * worktree mirror. `QuickOpen.tsx` draws it; `ui/quick-open-check.ts` pins it.
 *
 * ## Path-aware, the way Zed and VS Code are
 *
 * The **file name** is what people type. `main` should put `src/main.rs`
 * above `src/main/helpers/util.rs`, and `fbar` should find `FooBar.tsx` before
 * `foo/bar/index.ts`. So a query is matched against the name first and, only
 * if the name alone cannot hold it, against the whole path — and a whole-path
 * match still earns more for every character that lands inside the name.
 * A query containing `/` is plainly about directories and matches the path.
 *
 * Shorter paths win ties: the shallow file is usually the one meant, and it is
 * what keeps the order stable between keystrokes.
 *
 * Instant by construction — the mirror is local, so this never waits on the
 * network; a cheap subsequence test rejects most paths before scoring.
 */

export type FileHit = {
  path: string;
  score: number;
  /** Matched ranges within the name. */
  nameRanges: Range[];
  /** Matched ranges within the directory part (`path` minus the name). */
  dirRanges: Range[];
};

/** Rows drawn. The rest are a keystroke away, and the footer says how many. */
export const MAX_RESULTS = 60;

const NAME_WEIGHT = 3;
const NAME_BONUS = 40;
const IN_NAME_BONUS = 4;

function subsequence(pattern: string, text: string): boolean {
  let i = 0;
  for (let j = 0; j < text.length && i < pattern.length; j++) {
    if (text[j] === pattern[i]) i++;
  }
  return i === pattern.length;
}

export function scorePath(path: string, query: string): FileHit | null {
  const q = query.replace(/\s+/g, "");
  if (!q) return { path, score: 0, nameRanges: [], dirRanges: [] };
  const lower = path.toLowerCase();
  const lq = q.toLowerCase();
  if (!subsequence(lq, lower)) return null;

  const cut = path.lastIndexOf("/") + 1;
  const name = path.slice(cut);
  const lengthPenalty = path.length * 0.05;

  if (!q.includes("/")) {
    const onName = fuzzy(q, name);
    if (onName) {
      return {
        path,
        score: onName.score * NAME_WEIGHT + NAME_BONUS - lengthPenalty,
        nameRanges: toRanges(onName.indices),
        dirRanges: [],
      };
    }
  }

  const whole = fuzzy(q, path);
  if (!whole) return null;
  let score = whole.score - lengthPenalty;
  const nameIdx: number[] = [];
  const dirIdx: number[] = [];
  for (const i of whole.indices) {
    if (i >= cut) {
      score += IN_NAME_BONUS;
      nameIdx.push(i - cut);
    } else dirIdx.push(i);
  }
  return { path, score, nameRanges: toRanges(nameIdx), dirRanges: toRanges(dirIdx) };
}

/**
 * Rank `paths` for `query`. With an empty query, `recent` (most recent first)
 * leads, then the shallowest files — something useful before a key is pressed.
 */
export function rankFiles(
  paths: readonly string[],
  query: string,
  recent: readonly string[] = [],
  limit = MAX_RESULTS,
): { rows: FileHit[]; total: number } {
  if (!query.trim()) {
    const known = new Set(paths);
    const head = recent.filter((p) => known.has(p));
    const seen = new Set(head);
    const rest = paths
      .filter((p) => !seen.has(p))
      .sort((a, b) => depth(a) - depth(b) || a.length - b.length || (a < b ? -1 : 1));
    const all = [...head, ...rest];
    return {
      rows: all.slice(0, limit).map((path) => ({ path, score: 0, nameRanges: [], dirRanges: [] })),
      total: all.length,
    };
  }
  const recentRank = new Map(recent.map((p, i) => [p, recent.length - i]));
  const hits: FileHit[] = [];
  for (const path of paths) {
    const hit = scorePath(path, query);
    if (!hit) continue;
    // A file you just had open is a little more likely to be the one meant.
    hit.score += (recentRank.get(path) ?? 0) * 0.5;
    hits.push(hit);
  }
  hits.sort((a, b) => b.score - a.score || a.path.length - b.path.length || (a.path < b.path ? -1 : 1));
  return { rows: hits.slice(0, limit), total: hits.length };
}

function depth(path: string): number {
  let n = 0;
  for (const c of path) if (c === "/") n++;
  return n;
}

/** Split `text` by `ranges` for highlighting. */
export function pieces(text: string, ranges: readonly Range[]): { text: string; hit: boolean }[] {
  const out: { text: string; hit: boolean }[] = [];
  let at = 0;
  for (const [start, end] of ranges) {
    if (start > at) out.push({ text: text.slice(at, start), hit: false });
    out.push({ text: text.slice(start, end), hit: true });
    at = end;
  }
  if (at < text.length) out.push({ text: text.slice(at), hit: false });
  return out;
}

export const QUICK_OPEN_EVENT = "rmux:quick-open";

export function requestQuickOpen(): void {
  window.dispatchEvent(new CustomEvent(QUICK_OPEN_EVENT));
}
