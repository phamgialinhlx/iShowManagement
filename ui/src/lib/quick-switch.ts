import { STATE_PRIORITY, needsYou, type AgentState } from "./agent-state.ts";

/**
 * The quick switcher (⌘K, prefix `g`) — the **pure** half: query parsing,
 * fuzzy scoring, ranking and the empty-query order. `QuickSwitch.tsx` builds
 * the rows from the store and draws them; everything that decides *which row
 * is first* lives here, where `ui/quick-switch-check.ts` pins it under Node.
 *
 * ## How a row is ranked
 *
 * Each whitespace-separated term of the query must match **some** field of a
 * row as a case-insensitive subsequence, or the row is out. A term scores its
 * best field, multiplied by that field's weight — the session name counts most,
 * then the project, then machine and tab, then branch, then folder — so `api`
 * puts a session *called* api above one that merely lives in a folder called
 * api. Within a field, a matched character earns more when it continues the
 * previous match (contiguous) or starts a word (after a separator, at a
 * camelCase hump, or at index 0), and loses a little for every character
 * skipped. Ties fall to the more urgent state, then recency, then the sidebar
 * order, so the list never shuffles between keystrokes.
 *
 * ## The empty query
 *
 * Most-recently-viewed sessions first — **the one already on screen last
 * among them**, because a switcher opened on the session you are looking at is
 * opened to go somewhere else (⌘Tab's order). Then whatever needs you (blocked,
 * then done, newest first), then everything else in sidebar order, then
 * projects.
 *
 * No imports but the agent-state table, so the check never boots Zustand.
 */

// ── rows ─────────────────────────────────────────────────────────────────────

export type SwitchItem = {
  kind: "session" | "project";
  /** Session id or project id — what Enter acts on. */
  id: string;
  /** Session name, or project label. Highlighted. */
  name: string;
  /** `claude` · `pi` · `terminal`, for a session. */
  sessionKind?: string;
  /** From `agent-state.ts`; `null` for a terminal or a project. */
  state: AgentState | null;
  machine: string;
  /** Project label (for a session row). */
  project?: string;
  tab?: string;
  branch?: string;
  folder?: string;
  lastStatusAt?: number;
};

export type Range = [start: number, end: number];

export type Ranked = {
  item: SwitchItem;
  score: number;
  /** Matched character ranges within `item.name`, half-open, merged. */
  ranges: Range[];
};

// ── the query ────────────────────────────────────────────────────────────────

export const STATES: readonly AgentState[] = ["blocked", "working", "done", "idle", "unknown"];

export type Query = {
  /** Free-text terms, lower-cased. */
  terms: string[];
  /** `is:` filters — a row must be in one of these. Empty means any. */
  states: AgentState[];
  /** `@host` filters — a row's machine must contain one of these. */
  hosts: string[];
};

/** `is:blocked`, `is:b`, `b:` (alone) → `blocked`. `null` when it is not a state token. */
function stateToken(token: string): AgentState | null | undefined {
  const t = token.toLowerCase();
  let word: string | null = null;
  if (t.startsWith("is:")) word = t.slice(3);
  else if (/^[bwdiu]:$/.test(t)) word = t.slice(0, 1);
  if (word === null) return undefined; // not a filter at all
  if (!word) return null; // `is:` still being typed — a filter, but no state yet
  return STATES.find((s) => s.startsWith(word!)) ?? null;
}

export function parseQuery(raw: string): Query {
  const q: Query = { terms: [], states: [], hosts: [] };
  for (const token of raw.trim().split(/\s+/).filter(Boolean)) {
    const st = stateToken(token);
    if (st !== undefined) {
      if (st && !q.states.includes(st)) q.states.push(st);
      continue;
    }
    if (token.startsWith("@")) {
      const h = token.slice(1).toLowerCase();
      if (h && !q.hosts.includes(h)) q.hosts.push(h);
      continue;
    }
    q.terms.push(token.toLowerCase());
  }
  return q;
}

// ── fuzzy ────────────────────────────────────────────────────────────────────

const SCORE_MATCH = 1;
const BONUS_CONTIGUOUS = 7;
const BONUS_WORD_START = 6;
const BONUS_FIRST = 4;
const PENALTY_GAP = 0.25;
const MAX_GAP_PENALTY = 3;

function isWordStart(text: string, i: number): boolean {
  if (i === 0) return true;
  const prev = text[i - 1]!;
  const cur = text[i]!;
  if (!/[a-z0-9]/i.test(prev)) return true;
  // camelCase hump and letter→digit boundaries.
  if (/[a-z]/.test(prev) && /[A-Z]/.test(cur)) return true;
  if (/[a-z]/i.test(prev) && /[0-9]/.test(cur)) return true;
  return false;
}

/**
 * Score `pattern` as a subsequence of `text`, case-insensitive, or `null`.
 *
 * Tries every start position of the first character and takes the best greedy
 * run from each, so `ap` in `rmux-api-gateway` lands on `api` (a word start,
 * contiguous) rather than the first `a`. Each later character prefers, in
 * order: directly after the previous match, then the next word start before
 * the next plain occurrence. Deterministic: on equal scores the earliest start
 * wins.
 */
export function fuzzy(pattern: string, text: string): { score: number; indices: number[] } | null {
  if (!pattern) return { score: 0, indices: [] };
  const p = pattern.toLowerCase();
  const t = text.toLowerCase();
  if (p.length > t.length) return null;
  let best: { score: number; indices: number[] } | null = null;
  for (let start = t.indexOf(p[0]!); start !== -1; start = t.indexOf(p[0]!, start + 1)) {
    const indices = [start];
    let ok = true;
    for (let k = 1; k < p.length; k++) {
      const prev = indices[k - 1]!;
      const c = p[k]!;
      if (t[prev + 1] === c) {
        indices.push(prev + 1);
        continue;
      }
      const plain = t.indexOf(c, prev + 1);
      if (plain === -1) {
        ok = false;
        break;
      }
      let pick = plain;
      for (let j = prev + 1; j < t.length; j++) {
        if (t[j] === c && isWordStart(text, j)) {
          pick = j;
          break;
        }
      }
      // A word start far away is still better than a mid-word letter only when
      // nothing contiguous was available — which is the case here.
      indices.push(pick);
    }
    if (!ok) continue;
    let score = 0;
    for (let k = 0; k < indices.length; k++) {
      const i = indices[k]!;
      score += SCORE_MATCH;
      if (k > 0 && i === indices[k - 1]! + 1) score += BONUS_CONTIGUOUS;
      if (isWordStart(text, i)) score += BONUS_WORD_START;
      if (k > 0) score -= Math.min(MAX_GAP_PENALTY, (i - indices[k - 1]! - 1) * PENALTY_GAP);
    }
    if (indices[0] === 0) score += BONUS_FIRST;
    if (!best || score > best.score) best = { score, indices };
  }
  return best;
}

/** Merge sorted indices into half-open ranges: `[0,1,2,5]` → `[[0,3],[5,6]]`. */
export function toRanges(indices: readonly number[]): Range[] {
  const sorted = [...new Set(indices)].sort((a, b) => a - b);
  const out: Range[] = [];
  for (const i of sorted) {
    const last = out[out.length - 1];
    if (last && last[1] === i) last[1] = i + 1;
    else out.push([i, i + 1]);
  }
  return out;
}

// ── ranking ──────────────────────────────────────────────────────────────────

/** The most rows the list draws. A host can hold more sessions than fit; the
 *  rest are reachable by typing, and the footer says how many were left out. */
export const MAX_ROWS = 50;

type Field = "name" | "project" | "machine" | "tab" | "branch" | "folder";
export const WEIGHTS: Record<Field, number> = {
  name: 3,
  project: 2,
  machine: 1.5,
  tab: 1.5,
  branch: 1.2,
  folder: 1,
};
const FIELDS = Object.keys(WEIGHTS) as Field[];

function passesFilters(item: SwitchItem, q: Query): boolean {
  if (q.states.length) {
    // A project has no state of its own here — `is:` is a question about agents.
    if (item.kind !== "session" || !item.state || !q.states.includes(item.state)) return false;
  }
  if (q.hosts.length) {
    const m = item.machine.toLowerCase();
    if (!q.hosts.some((h) => m.includes(h))) return false;
  }
  return true;
}

/** Score one row against the free-text terms, or `null` if a term matches nothing. */
export function scoreItem(item: SwitchItem, terms: readonly string[]): { score: number; ranges: Range[] } | null {
  let total = 0;
  const nameIdx: number[] = [];
  for (const term of terms) {
    let bestScore = -Infinity;
    let bestName: number[] | null = null;
    for (const field of FIELDS) {
      const value = item[field];
      if (!value) continue;
      const m = fuzzy(term, value);
      if (!m) continue;
      const s = m.score * WEIGHTS[field];
      if (s > bestScore) {
        bestScore = s;
        bestName = field === "name" ? m.indices : null;
      }
    }
    if (bestScore === -Infinity) return null;
    total += bestScore;
    if (bestName) nameIdx.push(...bestName);
  }
  return { score: total, ranges: toRanges(nameIdx) };
}

export type RankResult = { rows: Ranked[]; total: number };

/**
 * The rows to show, in order, at most `limit`, and how many matched in all.
 *
 * `items` arrive in sidebar order (servers, then projects, then sessions), which
 * is the final tie-break. `mru` is session ids, most recent first; `current` is
 * the session on screen.
 */
export function rank(
  items: readonly SwitchItem[],
  raw: string,
  mru: readonly string[],
  current: string | null,
  limit = MAX_ROWS,
): RankResult {
  const q = parseQuery(raw);
  const recency = new Map<string, number>();
  mru.forEach((id, i) => {
    if (!recency.has(id)) recency.set(id, i);
  });
  const recent = (it: SwitchItem) =>
    it.kind === "session" ? (recency.get(it.id) ?? Number.POSITIVE_INFINITY) : Number.POSITIVE_INFINITY;
  const prio = (it: SwitchItem) => (it.state ? STATE_PRIORITY[it.state] : -1);

  const pool = items.map((item, order) => ({ item, order })).filter(({ item }) => passesFilters(item, q));

  if (!q.terms.length) {
    // Empty (or filter-only) query: recency, then attention, then sidebar order.
    const bucket = (it: SwitchItem): number => {
      if (it.kind === "project") return 4;
      if (it.id === current) return recency.has(it.id) ? 1 : 3;
      if (recency.has(it.id)) return 0;
      if (needsYou(it.state)) return 2;
      return 3;
    };
    const sorted = pool
      .map((x) => ({ ...x, b: bucket(x.item) }))
      .sort(
        (a, c) =>
          a.b - c.b ||
          (a.b === 0 ? recent(a.item) - recent(c.item) : 0) ||
          (a.b === 2 ? prio(c.item) - prio(a.item) || (c.item.lastStatusAt ?? 0) - (a.item.lastStatusAt ?? 0) : 0) ||
          a.order - c.order,
      );
    return {
      total: sorted.length,
      rows: sorted.slice(0, limit).map(({ item }) => ({ item, score: 0, ranges: [] })),
    };
  }

  const scored: { item: SwitchItem; order: number; score: number; ranges: Range[] }[] = [];
  for (const { item, order } of pool) {
    const s = scoreItem(item, q.terms);
    if (s) scored.push({ item, order, ...s });
  }
  scored.sort(
    (a, c) =>
      c.score - a.score ||
      prio(c.item) - prio(a.item) ||
      recent(a.item) - recent(c.item) ||
      a.order - c.order,
  );
  return {
    total: scored.length,
    rows: scored.slice(0, limit).map(({ item, score, ranges }) => ({ item, score, ranges })),
  };
}

/** Split `text` into plain and highlighted runs for drawing. */
export function segments(text: string, ranges: readonly Range[]): { text: string; hit: boolean }[] {
  const out: { text: string; hit: boolean }[] = [];
  let at = 0;
  for (const [s, e] of ranges) {
    if (s > at) out.push({ text: text.slice(at, s), hit: false });
    if (e > s) out.push({ text: text.slice(s, e), hit: true });
    at = Math.max(at, e);
  }
  if (at < text.length) out.push({ text: text.slice(at), hit: false });
  return out;
}

// ── recency ──────────────────────────────────────────────────────────────────

export const MRU_CAP = 30;

/** `id` moved to the front; pruned to known ids and to the cap. */
export function touchMru(list: readonly string[], id: string, known?: (id: string) => boolean): string[] {
  return [id, ...list.filter((x) => x !== id && (!known || known(x)))].slice(0, MRU_CAP);
}

// ── opening ──────────────────────────────────────────────────────────────────

/** Opens (or closes) the switcher. An event, like `FOCUS_EVENT`, so the prefix
 *  table and the shortcut handler need not import the component. */
export const QUICK_SWITCH_EVENT = "rmux:quick-switch";

export function requestQuickSwitch(): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent(QUICK_SWITCH_EVENT));
}
