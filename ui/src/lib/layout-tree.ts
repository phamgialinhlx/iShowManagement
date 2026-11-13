import type {
  PaneRef,
  PersistedV3,
  Project,
  ProjectId,
  Server,
  SessionV3,
} from "./workspace-model.ts";

/**
 * The workbench layout, v4 — herdr's model: **Project → Tabs → a binary split
 * tree of panes.**
 *
 * This replaces the equal-size grid ("deck": 1, 2x2, … 8x8). A grid could only
 * say "N equal tiles, auto-filled from the rail", which meant the arrangement was
 * whatever the rail order happened to be, a pane could not be wider than its
 * neighbour, and a session could be on screen in one layout and unreachable in
 * the next without anything saying so. Here the arrangement is the stored
 * structure:
 *
 * - one **project** is shown at a time (`activeProject`);
 * - a project has an ordered list of **tabs**, one of them active;
 * - a tab holds a **split tree**: a leaf is a pane (`PaneRef`), a split divides
 *   its rectangle between two children at `ratio`.
 *
 * ## The invariant this module exists to keep
 *
 * **Every session lives in exactly one leaf of exactly one tab of its own
 * project.** Two halves, both silent when broken:
 *
 * - *at least one* — a session in no tab is unreachable from the pane area, and
 *   a session that quietly stops being reachable looks identical to one that was
 *   never there;
 * - *at most one* — two leaves on one session mount its view twice, which for
 *   Claude is two panes attached to one conversation, both writing to it.
 *
 * `normalizeLayout` repairs both on every load and every pool change, so the
 * store never has to trust that each code path remembered.
 *
 * **Pure, with type-only imports** (erased), like `workspace-model.ts`: the
 * store and `ui/layout-tree-check.ts` both depend on it, and a runtime import
 * would boot the Zustand store as a side effect of the test. Ids are never
 * minted here — a clock or counter is not pure — so every function that creates
 * a node takes a `Mint` or the ids themselves.
 */

// ── shapes ───────────────────────────────────────────────────────────────────

/**
 * The split shape is generic over what a leaf holds, because there are two
 * trees of it: a tab's tree of **panes** (`Leaf`), and the window's tree of
 * **projects** (`ProjectLeaf`, the project grid). One set of primitives —
 * split, remove, ratio, rectangles, geometric neighbour — serves both, so a
 * divider clamp or a neighbour rule cannot mean one thing for panes and another
 * for tiles.
 */
export type BaseLeaf = { kind: "leaf"; id: string };

/**
 * `row` lays `a` and `b` side by side (`a` left) — what "split right" makes.
 * `col` stacks them (`a` on top) — what "split down" makes. The same words as
 * CSS `flex-direction`, so the renderer and the model cannot disagree.
 */
export type SplitOf<L extends BaseLeaf> = {
  kind: "split";
  id: string;
  dir: "row" | "col";
  /** The share of the rectangle `a` takes, clamped to `MIN_RATIO..MAX_RATIO`. */
  ratio: number;
  a: NodeOf<L>;
  b: NodeOf<L>;
};

export type NodeOf<L extends BaseLeaf> = L | SplitOf<L>;

export type Leaf = { kind: "leaf"; id: string; pane: PaneRef };
export type Split = SplitOf<Leaf>;
export type SplitNode = NodeOf<Leaf>;

/** A tile of the project grid: one project's tab bar and active tab. */
export type ProjectLeaf = { kind: "leaf"; id: string; project: ProjectId };
export type ProjectTree = NodeOf<ProjectLeaf>;

export type Tab = {
  id: string;
  /** Stored name. Shown only once `renamed`; otherwise the tab is labelled by
   *  its focused session, which tracks renames and Claude's own title. */
  name: string;
  renamed?: boolean;
  root: SplitNode;
  /** The focused leaf's id. Always a leaf of `root` (normalize repairs it). */
  focus: string;
  /** The focused pane fills the tab. Cleared when there is nothing to zoom. */
  zoomed?: boolean;
};

export type ProjectTabs = { tabs: Tab[]; activeTab: string | null };

export type Layout = {
  /** The project being worked in — with a project grid, **the focused tile's**. */
  activeProject: ProjectId | null;
  projects: Record<ProjectId, ProjectTabs>;
  /**
   * The project grid: several projects on screen at once, as a split tree of
   * tiles. **Absent means one tile showing `activeProject`** — exactly the
   * single-project behaviour, and what every layout saved before the grid
   * existed parses to. Never stored with fewer than two tiles (`normalize`
   * collapses it), and a project is in at most one tile.
   */
  view?: ProjectTree;
};

export type Direction = "left" | "right" | "up" | "down";

export type Mint = (prefix: "tab" | "leaf" | "split" | "tile") => string;

export const EMPTY_LAYOUT: Layout = { activeProject: null, projects: {} };
const EMPTY_TABS: ProjectTabs = { tabs: [], activeTab: null };

/**
 * A dragged divider is clamped, not free. Past 0.1 a pane is a sliver no
 * terminal can fit a prompt into, and a pane dragged to zero is a session that
 * is still running and can no longer be seen — the same unreachable state the
 * invariant above forbids, arrived at by mouse.
 */
export const MIN_RATIO = 0.1;
export const MAX_RATIO = 0.9;

export function clampRatio(r: number): number {
  if (!Number.isFinite(r)) return 0.5;
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, r));
}

// ── tree primitives ──────────────────────────────────────────────────────────

export function leaves<L extends BaseLeaf>(node: NodeOf<L>): L[] {
  return node.kind === "leaf" ? [node] : [...leaves(node.a), ...leaves(node.b)];
}

export function findLeaf<L extends BaseLeaf>(node: NodeOf<L>, leafId: string): L | null {
  return leaves(node).find((l) => l.id === leafId) ?? null;
}

/** A stable identity for what a pane shows — React keys and dedup both use it. */
export function paneKey(ref: PaneRef): string {
  if (ref.kind === "session") return `session:${ref.id}`;
  if (ref.kind === "host") return `host:${ref.serverId}`;
  return `files:${ref.projectId}`;
}

/** Replace `leafId` with a split of itself and `fresh` (`fresh` right/below). */
export function splitLeaf<L extends BaseLeaf>(
  node: NodeOf<L>,
  leafId: string,
  dir: "row" | "col",
  fresh: L,
  splitId: string,
  ratio = 0.5,
): NodeOf<L> {
  if (node.kind === "leaf") {
    if (node.id !== leafId) return node;
    return { kind: "split", id: splitId, dir, ratio: clampRatio(ratio), a: node, b: fresh };
  }
  const a = splitLeaf(node.a, leafId, dir, fresh, splitId, ratio);
  const b = a === node.a ? splitLeaf(node.b, leafId, dir, fresh, splitId, ratio) : node.b;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/**
 * Remove a leaf. Its parent split **collapses** into the sibling — a split with
 * one child is not a state, it is a pane with a divider leading nowhere.
 * Returns `null` when the leaf was the whole tree.
 */
export function removeLeaf<L extends BaseLeaf>(node: NodeOf<L>, leafId: string): NodeOf<L> | null {
  if (node.kind === "leaf") return node.id === leafId ? null : node;
  const a = removeLeaf(node.a, leafId);
  const b = removeLeaf(node.b, leafId);
  if (!a) return b;
  if (!b) return a;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/** Keep only leaves `keep` accepts, collapsing as `removeLeaf` does. */
export function filterLeaves<L extends BaseLeaf>(node: NodeOf<L>, keep: (l: L) => boolean): NodeOf<L> | null {
  if (node.kind === "leaf") return keep(node) ? node : null;
  const a = filterLeaves(node.a, keep);
  const b = filterLeaves(node.b, keep);
  if (!a) return b;
  if (!b) return a;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

export function setRatio<L extends BaseLeaf>(node: NodeOf<L>, splitId: string, ratio: number): NodeOf<L> {
  if (node.kind === "leaf") return node;
  if (node.id === splitId) return { ...node, ratio: clampRatio(ratio) };
  const a = setRatio(node.a, splitId, ratio);
  const b = setRatio(node.b, splitId, ratio);
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/**
 * Swap two leaves' positions. The *leaf* moves — id and pane together — so the
 * focus, which names a leaf id, travels with the pane being moved, as herdr's
 * swap does.
 */
export function swapLeaves(node: SplitNode, idA: string, idB: string): SplitNode {
  const la = findLeaf(node, idA);
  const lb = findLeaf(node, idB);
  if (!la || !lb || idA === idB) return node;
  const walk = (n: SplitNode): SplitNode => {
    if (n.kind === "leaf") return n.id === idA ? lb : n.id === idB ? la : n;
    return { ...n, a: walk(n.a), b: walk(n.b) };
  };
  return walk(node);
}

// ── geometry ─────────────────────────────────────────────────────────────────

/** A rectangle in the tab's unit square (0..1 on both axes). */
export type Rect = { x: number; y: number; w: number; h: number };
export const UNIT: Rect = { x: 0, y: 0, w: 1, h: 1 };

/** A divider ratio being dragged, applied without committing it to the tree. */
export type RatioOverride = { splitId: string; ratio: number } | null;

function childRects<L extends BaseLeaf>(s: SplitOf<L>, r: Rect, override: RatioOverride): [Rect, Rect] {
  const ratio = clampRatio(override && override.splitId === s.id ? override.ratio : s.ratio);
  if (s.dir === "row") {
    const wa = r.w * ratio;
    return [
      { x: r.x, y: r.y, w: wa, h: r.h },
      { x: r.x + wa, y: r.y, w: r.w - wa, h: r.h },
    ];
  }
  const ha = r.h * ratio;
  return [
    { x: r.x, y: r.y, w: r.w, h: ha },
    { x: r.x, y: r.y + ha, w: r.w, h: r.h - ha },
  ];
}

/** Every leaf's rectangle, keyed by leaf id. */
export function leafRects<L extends BaseLeaf>(
  node: NodeOf<L>,
  rect: Rect = UNIT,
  override: RatioOverride = null,
): Record<string, Rect> {
  const out: Record<string, Rect> = {};
  const walk = (n: NodeOf<L>, r: Rect) => {
    if (n.kind === "leaf") {
      out[n.id] = r;
      return;
    }
    const [ra, rb] = childRects(n, r, override);
    walk(n.a, ra);
    walk(n.b, rb);
  };
  walk(node, rect);
  return out;
}

/** Every split with the rectangle it divides — what the dividers are drawn from. */
export function splitRects<L extends BaseLeaf>(
  node: NodeOf<L>,
  rect: Rect = UNIT,
  override: RatioOverride = null,
): { split: SplitOf<L>; rect: Rect }[] {
  const out: { split: SplitOf<L>; rect: Rect }[] = [];
  const walk = (n: NodeOf<L>, r: Rect) => {
    if (n.kind === "leaf") return;
    out.push({ split: n, rect: r });
    const [ra, rb] = childRects(n, r, override);
    walk(n.a, ra);
    walk(n.b, rb);
  };
  walk(node, rect);
  return out;
}

const EPS = 1e-6;
const overlap = (a0: number, a1: number, b0: number, b1: number) =>
  Math.max(0, Math.min(a1, b1) - Math.max(a0, b0));

/**
 * The leaf next to `leafId` in a direction, **geometrically** — from the
 * rectangles, never from tree position. "The sibling" is wrong as soon as the
 * tree is nested: in `[A | [B / C]]`, right of A is B *or* C depending on
 * height, and up from C is B even though they share no parent with A.
 *
 * Candidates lie wholly on that side and overlap on the other axis. Nearest edge
 * wins; then the one level with the focused pane's centre, so moving right and
 * back left returns to where you started; then the largest overlap.
 * `null` at an edge — the caller treats that as a no-op, like the grid's clamp.
 */
export function neighbour<L extends BaseLeaf>(node: NodeOf<L>, leafId: string, dir: Direction): string | null {
  return neighbourIn(leafRects(node), leafId, dir);
}

/** `neighbour` over any set of rectangles — the project grid runs it across
 *  the panes of every tile at once. */
export function neighbourIn(rects: Record<string, Rect>, leafId: string, dir: Direction): string | null {
  const r = rects[leafId];
  if (!r) return null;
  const cx = r.x + r.w / 2;
  const cy = r.y + r.h / 2;

  let best: string | null = null;
  let bestScore: [number, number, number] | null = null;
  for (const [id, c] of Object.entries(rects)) {
    if (id === leafId) continue;
    let gap: number;
    let ov: number;
    let level: number;
    if (dir === "right" || dir === "left") {
      gap = dir === "right" ? c.x - (r.x + r.w) : r.x - (c.x + c.w);
      ov = overlap(r.y, r.y + r.h, c.y, c.y + c.h);
      level = cy >= c.y - EPS && cy <= c.y + c.h + EPS ? 0 : 1;
    } else {
      gap = dir === "down" ? c.y - (r.y + r.h) : r.y - (c.y + c.h);
      ov = overlap(r.x, r.x + r.w, c.x, c.x + c.w);
      level = cx >= c.x - EPS && cx <= c.x + c.w + EPS ? 0 : 1;
    }
    if (gap < -EPS || ov <= EPS) continue;
    const score: [number, number, number] = [gap, level, -ov];
    if (
      !bestScore ||
      score[0] < bestScore[0] - EPS ||
      (Math.abs(score[0] - bestScore[0]) <= EPS &&
        (score[1] < bestScore[1] || (score[1] === bestScore[1] && score[2] < bestScore[2] - EPS)))
    ) {
      best = id;
      bestScore = score;
    }
  }
  return best;
}

/**
 * A balanced-looking tree from a flat list, laid out `cols` across — used to
 * carry an old grid saved view into the tree model. Each row is a chain of
 * `row` splits, rows are chained by `col` splits, and each ratio is `1/n` of
 * what remains, so every cell gets an equal share exactly as the grid showed it.
 */
export function treeFromPanes(panes: PaneRef[], cols: number, mint: Mint): SplitNode | null {
  if (!panes.length) return null;
  const across = Math.max(1, Math.round(cols) || 1);
  const chain = (nodes: SplitNode[], dir: "row" | "col"): SplitNode => {
    let node = nodes[nodes.length - 1]!;
    for (let i = nodes.length - 2; i >= 0; i -= 1) {
      const n = nodes.length - i;
      node = { kind: "split", id: mint("split"), dir, ratio: clampRatio(1 / n), a: nodes[i]!, b: node };
    }
    return node;
  };
  const rows: SplitNode[] = [];
  for (let i = 0; i < panes.length; i += across) {
    const row = panes
      .slice(i, i + across)
      .map((pane): SplitNode => ({ kind: "leaf", id: mint("leaf"), pane }));
    rows.push(chain(row, "row"));
  }
  return chain(rows, "col");
}

/** A deep copy with fresh ids, keeping only the leaves `keep` accepts. */
export function cloneTree(
  node: SplitNode,
  keep: (pane: PaneRef) => boolean,
  mint: Mint,
): SplitNode | null {
  if (node.kind === "leaf") {
    return keep(node.pane) ? { kind: "leaf", id: mint("leaf"), pane: { ...node.pane } } : null;
  }
  const a = cloneTree(node.a, keep, mint);
  const b = cloneTree(node.b, keep, mint);
  if (!a) return b;
  if (!b) return a;
  return { kind: "split", id: mint("split"), dir: node.dir, ratio: clampRatio(node.ratio), a, b };
}

// ── layout-level reads ───────────────────────────────────────────────────────

export function tabsOf(layout: Layout, pid: ProjectId | null): ProjectTabs {
  return (pid && layout.projects[pid]) || EMPTY_TABS;
}

export function activeTab(layout: Layout): { projectId: ProjectId; tab: Tab } | null {
  const pid = layout.activeProject;
  if (!pid) return null;
  const pt = tabsOf(layout, pid);
  const tab = pt.tabs.find((t) => t.id === pt.activeTab);
  return tab ? { projectId: pid, tab } : null;
}

/** A tab's leaves as drawn: all of them, or only the focused one while zoomed. */
export function shownLeaves(tab: Tab): Leaf[] {
  if (tab.zoomed) {
    const f = findLeaf(tab.root, tab.focus);
    if (f) return [f];
  }
  return leaves(tab.root);
}

/** A project's active tab, whether or not it is the focused project. */
export function activeTabOf(layout: Layout, pid: ProjectId): Tab | null {
  const pt = tabsOf(layout, pid);
  return pt.tabs.find((t) => t.id === pt.activeTab) ?? null;
}

/**
 * Leaves the operator can see right now: **every tile's** active tab (only the
 * focused pane of a zoomed one). This is "on screen" for the rail's marks,
 * hands-free, notification quieting and the warm set — one derivation, because
 * with a project grid "the active tab" is no longer everything on screen, and a
 * consumer still reading only that would call a pane in plain view unwatched.
 */
export function visibleLeaves(layout: Layout): Leaf[] {
  return shownProjects(layout).flatMap((pid) => {
    const tab = activeTabOf(layout, pid);
    return tab ? shownLeaves(tab) : [];
  });
}

export function focusedLeaf(layout: Layout): Leaf | null {
  const at = activeTab(layout);
  return at ? findLeaf(at.tab.root, at.tab.focus) : null;
}

export type Location = { projectId: ProjectId; tabId: string; leafId: string };

export function locatePane(layout: Layout, test: (p: PaneRef) => boolean): Location | null {
  for (const [projectId, pt] of Object.entries(layout.projects)) {
    for (const tab of pt.tabs) {
      const leaf = leaves(tab.root).find((l) => test(l.pane));
      if (leaf) return { projectId, tabId: tab.id, leafId: leaf.id };
    }
  }
  return null;
}

export function locateSession(layout: Layout, sessionId: string): Location | null {
  return locatePane(layout, (p) => p.kind === "session" && p.id === sessionId);
}

/** Every session id under a tab — what closing it would end. */
export function sessionsInTab(tab: Tab): string[] {
  return leaves(tab.root).flatMap((l) => (l.pane.kind === "session" ? [l.pane.id] : []));
}

/**
 * What a tab is called. A renamed tab keeps its name; otherwise it is named by
 * its focused pane, so a tab tracks its session's rename and Claude's adopted
 * title rather than freezing whatever the session was called when the tab was
 * made. The stored name and then the position are fallbacks, never blanks.
 */
export function tabLabel(
  tab: Tab,
  index: number,
  sessionName: (id: string) => string | undefined,
): string {
  if (tab.renamed && tab.name.trim()) return tab.name;
  const f = findLeaf(tab.root, tab.focus) ?? leaves(tab.root)[0];
  if (f?.pane.kind === "session") {
    const n = sessionName(f.pane.id);
    if (n) return n;
  }
  if (f?.pane.kind === "host") return "host";
  if (f?.pane.kind === "files") return "files";
  return tab.name || String(index + 1);
}

// ── layout-level transitions (all immutable) ─────────────────────────────────

function withTabs(layout: Layout, pid: ProjectId, fn: (pt: ProjectTabs) => ProjectTabs): Layout {
  const next = fn(tabsOf(layout, pid));
  return { ...layout, projects: { ...layout.projects, [pid]: next } };
}

function withTab(layout: Layout, pid: ProjectId, tabId: string, fn: (t: Tab) => Tab): Layout {
  return withTabs(layout, pid, (pt) => ({
    ...pt,
    tabs: pt.tabs.map((t) => (t.id === tabId ? fn(t) : t)),
  }));
}

/**
 * Make `pid` the focused project. With a project grid: a project already in a
 * tile focuses that tile; one that is not **replaces the focused tile's
 * project** — what a single tile has always done, so nothing about clicking a
 * project changes until a second tile exists. Every transition that moves
 * `activeProject` goes through here, so the grid can never be left showing a
 * focus that is in no tile.
 */
export function selectProject(layout: Layout, pid: ProjectId): Layout {
  if (layout.activeProject === pid) return layout;
  return { ...showInFocusedTile(layout, pid), activeProject: pid };
}

export function selectTab(layout: Layout, pid: ProjectId, tabId: string): Layout {
  const pt = tabsOf(layout, pid);
  if (!pt.tabs.some((t) => t.id === tabId)) return layout;
  return withTabs(selectProject(layout, pid), pid, (p) => ({ ...p, activeTab: tabId }));
}

/** Select the active project's tab at `index` (0-based). Out of range: no-op. */
export function selectTabIndex(layout: Layout, index: number): Layout {
  const pid = layout.activeProject;
  const tab = pid ? tabsOf(layout, pid).tabs[index] : undefined;
  return pid && tab ? selectTab(layout, pid, tab.id) : layout;
}

/** Next/previous tab in the active project, wrapping — a tab strip is a ring,
 *  unlike a grid, whose edges are places. */
export function cycleTab(layout: Layout, delta: number): Layout {
  const pid = layout.activeProject;
  if (!pid) return layout;
  const pt = tabsOf(layout, pid);
  if (pt.tabs.length < 2) return layout;
  const at = Math.max(0, pt.tabs.findIndex((t) => t.id === pt.activeTab));
  const n = pt.tabs.length;
  return selectTab(layout, pid, pt.tabs[(((at + delta) % n) + n) % n]!.id);
}

/** Append a tab holding one pane, and make it (and its project) active. */
export function addTab(
  layout: Layout,
  pid: ProjectId,
  pane: PaneRef,
  name: string,
  mint: Mint,
  opts: { activate?: boolean; renamed?: boolean } = {},
): { layout: Layout; tabId: string; leafId: string } {
  const leafId = mint("leaf");
  const tabId = mint("tab");
  const tab: Tab = {
    id: tabId,
    name,
    renamed: opts.renamed || undefined,
    root: { kind: "leaf", id: leafId, pane },
    focus: leafId,
  };
  const activate = opts.activate ?? true;
  let next = withTabs(layout, pid, (pt) => ({
    tabs: [...pt.tabs, tab],
    activeTab: activate || !pt.activeTab ? tabId : pt.activeTab,
  }));
  if (activate) next = selectProject(next, pid);
  return { layout: next, tabId, leafId };
}

/** Insert a whole tree as a new active tab. */
export function addTreeTab(
  layout: Layout,
  pid: ProjectId,
  root: SplitNode,
  name: string,
  mint: Mint,
): { layout: Layout; tabId: string } {
  const tabId = mint("tab");
  const tab: Tab = { id: tabId, name, renamed: true, root, focus: leaves(root)[0]!.id };
  const next = withTabs(selectProject(layout, pid), pid, (pt) => ({
    tabs: [...pt.tabs, tab],
    activeTab: tabId,
  }));
  return { layout: next, tabId };
}

/** Focus a leaf, and bring its tab and project forward. */
export function focusLeaf(layout: Layout, pid: ProjectId, tabId: string, leafId: string): Layout {
  const tab = tabsOf(layout, pid).tabs.find((t) => t.id === tabId);
  if (!tab || !findLeaf(tab.root, leafId)) return layout;
  const sel = selectTab(layout, pid, tabId);
  // Focusing another pane while zoomed un-zooms: a hidden pane holding the
  // keyboard is the "screen says one thing, keys do another" failure.
  return withTab(sel, pid, tabId, (t) =>
    t.focus === leafId ? t : { ...t, focus: leafId, zoomed: undefined },
  );
}

/**
 * Move the focus one pane in a direction. Inside the focused tile first; at its
 * edge, **across tiles**, geometrically — the panes of every tile are laid out
 * in one coordinate space and the same `neighbourIn` rule picks the nearest, so
 * `l` at the right edge of one project lands in the pane level with it in the
 * project beside. A tile with no tab is a target too (it focuses that project).
 * Returns the input layout unchanged when there is nothing there; `leaf` is the
 * pane now focused, `null` when the move landed on an empty tile or nowhere.
 */
export function focusInDirection(layout: Layout, dir: Direction): { layout: Layout; leaf: Leaf | null } {
  const at = activeTab(layout);
  if (at) {
    const to = neighbour(at.tab.root, at.tab.focus, dir);
    if (to) return { layout: focusLeaf(layout, at.projectId, at.tab.id, to), leaf: findLeaf(at.tab.root, to) };
  }
  const tree = layout.view;
  if (!tree || leaves(tree).length < 2) return { layout, leaf: null };
  const tiles = leafRects(tree);
  const rects: Record<string, Rect> = {};
  const where = new Map<string, { pid: ProjectId; tabId: string | null; leafId: string | null }>();
  let origin: string | null = null;
  for (const tile of leaves(tree)) {
    const T = tiles[tile.id]!;
    const tab = activeTabOf(layout, tile.project);
    if (!tab) {
      const key = `tile:${tile.id}`;
      rects[key] = T;
      where.set(key, { pid: tile.project, tabId: null, leafId: null });
      if (tile.project === layout.activeProject) origin = key;
      continue;
    }
    const inner = leafRects(tab.root);
    for (const leaf of shownLeaves(tab)) {
      const r = tab.zoomed ? UNIT : inner[leaf.id]!;
      const key = `${tile.id}:${leaf.id}`;
      rects[key] = { x: T.x + r.x * T.w, y: T.y + r.y * T.h, w: r.w * T.w, h: r.h * T.h };
      where.set(key, { pid: tile.project, tabId: tab.id, leafId: leaf.id });
      if (tile.project === layout.activeProject && leaf.id === tab.focus) origin = key;
    }
  }
  if (!origin) return { layout, leaf: null };
  const to = neighbourIn(rects, origin, dir);
  const hit = to ? where.get(to) : undefined;
  if (!hit) return { layout, leaf: null };
  if (!hit.tabId || !hit.leafId) return { layout: selectProject(layout, hit.pid), leaf: null };
  const tab = activeTabOf(layout, hit.pid)!;
  return { layout: focusLeaf(layout, hit.pid, hit.tabId, hit.leafId), leaf: findLeaf(tab.root, hit.leafId) };
}

/** Split the focused pane; the new pane takes the right/bottom half and the focus. */
export function splitFocused(
  layout: Layout,
  dir: "right" | "down",
  pane: PaneRef,
  mint: Mint,
): { layout: Layout; leafId: string } | null {
  const at = activeTab(layout);
  if (!at) return null;
  const leafId = mint("leaf");
  const root = splitLeaf(
    at.tab.root,
    at.tab.focus,
    dir === "right" ? "row" : "col",
    { kind: "leaf", id: leafId, pane },
    mint("split"),
  );
  if (root === at.tab.root) return null;
  const next = withTab(layout, at.projectId, at.tab.id, (t) => ({
    ...t,
    root,
    focus: leafId,
    zoomed: undefined,
  }));
  return { layout: next, leafId };
}

/**
 * Place a pane beside a session **without moving anything the operator is
 * using** — how a spawned child appears (herdr's orchestration).
 *
 * When `besideSessionId` is in a tab of `pid`, its leaf is split: side by side
 * when that pane is wider than tall on screen (`aspect` is the pane area's
 * width / height in pixels, because the tree's rectangles are in a unit square
 * and a 0.5 × 1 leaf is tall in a square but wide in a 16:9 window), else
 * stacked. The tab's focus, zoom, the active tab and the active project are all
 * left exactly as they were — a child arriving must never take the keyboard.
 *
 * Otherwise the pane gets a new tab in `pid`, not activated.
 */
export function placeBeside(
  layout: Layout,
  pid: ProjectId,
  besideSessionId: string | null,
  pane: PaneRef,
  mint: Mint,
  aspect = 16 / 9,
  name = "",
): { layout: Layout; tabId: string; leafId: string; placed: "split" | "tab"; dir?: "row" | "col" } {
  const loc = besideSessionId ? locateSession(layout, besideSessionId) : null;
  if (loc && loc.projectId === pid) {
    const tab = tabsOf(layout, pid).tabs.find((t) => t.id === loc.tabId)!;
    const r = leafRects(tab.root)[loc.leafId] ?? UNIT;
    const a = Number.isFinite(aspect) && aspect > 0 ? aspect : 16 / 9;
    const dir: "row" | "col" = r.w * a > r.h ? "row" : "col";
    const leafId = mint("leaf");
    const root = splitLeaf(tab.root, loc.leafId, dir, { kind: "leaf", id: leafId, pane }, mint("split"));
    // Only the tree changes: `focus` and `zoomed` are carried as they were.
    const next = withTab(layout, pid, tab.id, (t) => ({ ...t, root }));
    return { layout: next, tabId: tab.id, leafId, placed: "split", dir };
  }
  const added = addTab(layout, pid, pane, name, mint, { activate: false });
  return { ...added, placed: "tab" };
}

/** The first leaf of the subtree that takes a removed leaf's place. */
function heir<L extends BaseLeaf>(node: NodeOf<L>, leafId: string): string | null {
  if (node.kind === "leaf") return null;
  if (node.a.kind === "leaf" && node.a.id === leafId) return leaves(node.b)[0]!.id;
  if (node.b.kind === "leaf" && node.b.id === leafId) return leaves(node.a)[leaves(node.a).length - 1]!.id;
  return heir(node.a, leafId) ?? heir(node.b, leafId);
}

/** Remove a tab. The active tab passes to the one now at its index, else the last. */
export function removeTab(layout: Layout, pid: ProjectId, tabId: string): Layout {
  return withTabs(layout, pid, (pt) => {
    const at = pt.tabs.findIndex((t) => t.id === tabId);
    if (at < 0) return pt;
    const tabs = pt.tabs.filter((t) => t.id !== tabId);
    const activeTab =
      pt.activeTab === tabId ? (tabs[Math.min(at, tabs.length - 1)]?.id ?? null) : pt.activeTab;
    return { tabs, activeTab };
  });
}

/**
 * Close one pane. The parent split collapses; the focus passes to the pane that
 * took its place; a tab left with nothing is removed. Purely structural — ending
 * the session behind it is the store's job (and its confirmation's).
 */
export function closeLeaf(layout: Layout, pid: ProjectId, tabId: string, leafId: string): Layout {
  const tab = tabsOf(layout, pid).tabs.find((t) => t.id === tabId);
  if (!tab || !findLeaf(tab.root, leafId)) return layout;
  const root = removeLeaf(tab.root, leafId);
  if (!root) return removeTab(layout, pid, tabId);
  const focus = tab.focus === leafId ? (heir(tab.root, leafId) ?? leaves(root)[0]!.id) : tab.focus;
  const zoomed = tab.zoomed && leaves(root).length > 1 && focus === tab.focus ? true : undefined;
  return withTab(layout, pid, tabId, (t) => ({ ...t, root, focus, zoomed }));
}

/** Drop every leaf showing a session, wherever it is. */
export function removeSessionLeaves(layout: Layout, sessionId: string): Layout {
  let next = layout;
  for (;;) {
    const loc = locateSession(next, sessionId);
    if (!loc) return next;
    next = closeLeaf(next, loc.projectId, loc.tabId, loc.leafId);
  }
}

/** Zoom only means something with more than one pane; with one it is a no-op. */
export function toggleZoom(layout: Layout): Layout {
  const at = activeTab(layout);
  if (!at) return layout;
  if (!at.tab.zoomed && leaves(at.tab.root).length < 2) return layout;
  return withTab(layout, at.projectId, at.tab.id, (t) => ({ ...t, zoomed: t.zoomed ? undefined : true }));
}

/** Swap the focused pane with its neighbour in a direction; focus moves with it. */
export function swapInDirection(layout: Layout, dir: Direction): Layout {
  const at = activeTab(layout);
  if (!at) return layout;
  const other = neighbour(at.tab.root, at.tab.focus, dir);
  if (!other) return layout;
  return withTab(layout, at.projectId, at.tab.id, (t) => ({
    ...t,
    root: swapLeaves(t.root, t.focus, other),
  }));
}

export function setTabRatio(
  layout: Layout,
  pid: ProjectId,
  tabId: string,
  splitId: string,
  ratio: number,
): Layout {
  return withTab(layout, pid, tabId, (t) => ({ ...t, root: setRatio(t.root, splitId, ratio) }));
}

/** Rename a tab. An empty name hands the label back to its focused session. */
export function renameTab(layout: Layout, pid: ProjectId, tabId: string, name: string): Layout {
  const clean = name.trim();
  return withTab(layout, pid, tabId, (t) =>
    clean ? { ...t, name: clean, renamed: true } : { ...t, renamed: undefined },
  );
}

/** Reorder: move the tab at `from` to index `to` within its project. */
export function moveTab(layout: Layout, pid: ProjectId, from: number, to: number): Layout {
  return withTabs(layout, pid, (pt) => {
    if (from < 0 || from >= pt.tabs.length || from === to) return pt;
    const tabs = [...pt.tabs];
    const [moved] = tabs.splice(from, 1);
    tabs.splice(Math.max(0, Math.min(tabs.length, to)), 0, moved!);
    return { ...pt, tabs };
  });
}

// ── drag a session (or a whole tab) beside a pane ────────────────────────────

/**
 * What is being dragged: one session (a sidebar row, or a single-pane tab), or
 * a whole tab — which, with more than one pane, moves as its subtree.
 */
export type MoveSource =
  | { kind: "session"; sessionId: string }
  | { kind: "tab"; projectId: ProjectId; tabId: string };

/** The pane it is dropped on. */
export type MoveTarget = { projectId: ProjectId; tabId: string; leafId: string };

/**
 * The subtree a source names, and where it lives now. A single-pane tab is just
 * its leaf, so dragging it and dragging that session from the sidebar are the
 * same move. `null` when the source no longer exists.
 */
export function resolveMoveSource(
  layout: Layout,
  source: MoveSource,
): { projectId: ProjectId; tabId: string; node: SplitNode; wholeTab: boolean } | null {
  if (source.kind === "session") {
    const loc = locateSession(layout, source.sessionId);
    if (!loc) return null;
    const tab = tabsOf(layout, loc.projectId).tabs.find((t) => t.id === loc.tabId)!;
    const node = findLeaf(tab.root, loc.leafId)!;
    return { projectId: loc.projectId, tabId: loc.tabId, node, wholeTab: tab.root.kind === "leaf" };
  }
  const tab = tabsOf(layout, source.projectId).tabs.find((t) => t.id === source.tabId);
  if (!tab) return null;
  return { projectId: source.projectId, tabId: tab.id, node: tab.root, wholeTab: true };
}

/**
 * Would dropping `source` on `target` do anything? False for a drop onto the
 * dragged pane itself, onto any pane inside the subtree being dragged, or onto
 * a pane that is not there — the cases that show no drop zone at all.
 */
export function canMoveBeside(layout: Layout, source: MoveSource, target: MoveTarget): boolean {
  const src = resolveMoveSource(layout, source);
  if (!src) return false;
  const tab = tabsOf(layout, target.projectId).tabs.find((t) => t.id === target.tabId);
  if (!tab || !findLeaf(tab.root, target.leafId)) return false;
  return !findLeaf(src.node, target.leafId);
}

/**
 * Drop a session (or a whole tab) on one side of a pane: the target pane is
 * split in that direction and the dragged subtree takes that half.
 *
 * - The dragged subtree is **taken out** of where it was first — its parent
 *   split collapses, and a tab it leaves empty is removed — then inserted beside
 *   the target. **Every leaf id is preserved**: panes are keyed by leaf id, so a
 *   fresh one would remount the terminal behind it (a reattach flash for a move
 *   that changed nothing about the session).
 * - Dropping on itself or inside its own subtree returns `null` (no-op).
 * - The target tab takes the dragged pane's focus (the session the operator
 *   just placed, or the dragged tab's own focused pane), is un-zoomed so the new
 *   half is actually on screen, and becomes the active tab of its project.
 * - **Purely structural.** Nothing ends. `sessions` is every session id that
 *   moved, and `fromProject` where it came from — when that differs from
 *   `target.projectId`, the store must re-home those sessions in the same write,
 *   or `normalizeLayout` would pull them straight back out.
 */
export function moveBeside(
  layout: Layout,
  source: MoveSource,
  target: MoveTarget,
  side: Direction,
  mint: Mint,
): { layout: Layout; focus: string; sessions: string[]; fromProject: ProjectId } | null {
  if (!canMoveBeside(layout, source, target)) return null;
  const src = resolveMoveSource(layout, source)!;
  const srcTab = tabsOf(layout, src.projectId).tabs.find((t) => t.id === src.tabId)!;
  const focus =
    src.node.kind === "leaf" ? src.node.id : findLeaf(src.node, srcTab.focus) ? srcTab.focus : leaves(src.node)[0]!.id;
  const sessions = leaves(src.node).flatMap((l) => (l.pane.kind === "session" ? [l.pane.id] : []));

  // Take it out. A whole tab goes as a tab (its strip slot with it); a leaf
  // inside a bigger tab goes as `closeLeaf` does, focus passing to its heir.
  let next =
    src.wholeTab
      ? removeTab(layout, src.projectId, src.tabId)
      : closeLeaf(layout, src.projectId, src.tabId, src.node.id);

  const tab = tabsOf(next, target.projectId).tabs.find((t) => t.id === target.tabId);
  if (!tab || !findLeaf(tab.root, target.leafId)) return null; // cannot happen past canMoveBeside
  const root = insertBeside<Leaf>(tab.root, target.leafId, side, src.node, mint("split"));
  next = withTab(next, target.projectId, target.tabId, (t) => ({ ...t, root, focus, zoomed: undefined }));
  next = selectTab(next, target.projectId, target.tabId);
  return { layout: next, focus, sessions, fromProject: src.projectId };
}

// ── the project grid ─────────────────────────────────────────────────────────

/**
 * A big screen can show several projects at once: the pane area is itself a
 * split tree whose leaves are **projects** (`Layout.view`). Each tile draws that
 * project's own tab bar and its own active tab — its own focus, zoom and tabs —
 * and the focused tile is simply the one showing `activeProject`, so every
 * existing operation on "the active tab" keeps meaning the tab the operator is
 * in.
 *
 * Two rules the grid must keep, both silent when broken:
 *
 * - **A project is in at most one tile.** Two tiles on one project would draw
 *   its active tab twice — every session in it mounted twice. Opening a
 *   project that is already shown focuses its tile instead.
 * - **Closing a tile removes the tile, never work.** Its sessions keep running
 *   and its tabs stay exactly as they were; the project is one click away in
 *   the sidebar. Ending a session is what closing a *pane* does, with its
 *   confirmation.
 */

/** The id the implicit single tile is drawn under. Splitting it keeps it as the
 *  first half, so the tile on screen is not remounted when a second appears. */
export const SOLO_TILE = "tile-solo";

/** The grid as drawn: `view`, or the one implicit tile showing `activeProject`. */
export function viewTree(layout: Layout): ProjectTree | null {
  if (layout.view) return layout.view;
  return layout.activeProject ? { kind: "leaf", id: SOLO_TILE, project: layout.activeProject } : null;
}

/** Every project on screen, in tile order. */
export function shownProjects(layout: Layout): ProjectId[] {
  const tree = viewTree(layout);
  return tree ? leaves(tree).map((t) => t.project) : [];
}

export function tileOf(layout: Layout, pid: ProjectId): ProjectLeaf | null {
  const tree = viewTree(layout);
  return tree ? (leaves(tree).find((t) => t.project === pid) ?? null) : null;
}

function mapTiles(node: ProjectTree, fn: (t: ProjectLeaf) => ProjectLeaf): ProjectTree {
  if (node.kind === "leaf") return fn(node);
  return { ...node, a: mapTiles(node.a, fn), b: mapTiles(node.b, fn) };
}

/** The view after `pid` takes the focused tile (no-op when already shown or
 *  when there is no grid). `activeProject` is left for the caller to set. */
function showInFocusedTile(layout: Layout, pid: ProjectId): Layout {
  const tree = layout.view;
  if (!tree) return layout;
  const all = leaves(tree);
  if (all.some((t) => t.project === pid)) return layout;
  const focused = all.find((t) => t.project === layout.activeProject) ?? all[0]!;
  return { ...layout, view: mapTiles(tree, (t) => (t.id === focused.id ? { ...t, project: pid } : t)) };
}

/** Insert `fresh` (a leaf, or a whole subtree) on one side of `leafId` —
 *  left/up puts it first. */
export function insertBeside<L extends BaseLeaf>(
  node: NodeOf<L>,
  leafId: string,
  side: Direction,
  fresh: NodeOf<L>,
  splitId: string,
  ratio = 0.5,
): NodeOf<L> {
  const dir: "row" | "col" = side === "left" || side === "right" ? "row" : "col";
  const after = side === "right" || side === "down";
  const walk = (n: NodeOf<L>): NodeOf<L> => {
    if (n.kind === "leaf") {
      if (n.id !== leafId) return n;
      const r = clampRatio(ratio);
      return after
        ? { kind: "split", id: splitId, dir, ratio: r, a: n, b: fresh }
        : { kind: "split", id: splitId, dir, ratio: r, a: fresh, b: n };
    }
    const a = walk(n.a);
    const b = a === n.a ? walk(n.b) : n.b;
    return a === n.a && b === n.b ? n : { ...n, a, b };
  };
  return walk(node);
}

/**
 * Put `pid` in a new tile on `side` of tile `tileId`, and focus it. A project
 * already shown elsewhere **moves** there (it is taken out of its old tile
 * first), because a project in two tiles is its sessions mounted twice.
 * Dropping a project on its own tile changes nothing but the focus.
 */
export function placeTile(
  layout: Layout,
  pid: ProjectId,
  tileId: string,
  side: Direction,
  mint: Mint,
): Layout {
  let tree = viewTree(layout);
  if (!tree || !findLeaf(tree, tileId)) return layout;
  const existing = leaves(tree).find((t) => t.project === pid);
  if (existing) {
    if (existing.id === tileId) return selectProject(layout, pid);
    tree = removeLeaf(tree, existing.id);
    if (!tree || !findLeaf(tree, tileId)) return layout;
  }
  const view = insertBeside(tree, tileId, side, { kind: "leaf", id: mint("tile"), project: pid }, mint("split"));
  return { ...layout, view, activeProject: pid };
}

/**
 * Open `pid` in a new tile beside the focused one — to the right when the
 * focused tile is wider than tall on screen, else below. `aspect` is the pane
 * area's width / height in pixels, for the same reason `placeBeside` takes it:
 * the tree's rectangles are in a unit square. Already shown: focus its tile.
 */
export function openBeside(layout: Layout, pid: ProjectId, mint: Mint, aspect = 16 / 9): Layout {
  if (tileOf(layout, pid)) return selectProject(layout, pid);
  const tree = viewTree(layout);
  if (!tree) return { ...layout, activeProject: pid };
  const focused = leaves(tree).find((t) => t.project === layout.activeProject) ?? leaves(tree)[0]!;
  const r = leafRects(tree)[focused.id] ?? UNIT;
  const a = Number.isFinite(aspect) && aspect > 0 ? aspect : 16 / 9;
  return placeTile(layout, pid, focused.id, r.w * a > r.h ? "right" : "down", mint);
}

/**
 * Remove a tile. Structural only — **no session ends**, no tab closes. The
 * focus passes to the tile that takes its place; one tile left is the plain
 * single-project view again. The last tile cannot be closed (there is no "no
 * project" view to fall back to), so this is a no-op then.
 */
export function closeTile(layout: Layout, tileId: string): Layout {
  const tree = layout.view;
  if (!tree || leaves(tree).length < 2) return layout;
  const tile = findLeaf(tree, tileId);
  if (!tile) return layout;
  const rest = removeLeaf(tree, tileId)!;
  let activeProject = layout.activeProject;
  if (tile.project === activeProject) {
    const h = heir(tree, tileId);
    activeProject = (h ? findLeaf(tree, h)?.project : undefined) ?? leaves(rest)[0]!.project;
  }
  const out: Layout = { ...layout, activeProject };
  if (leaves(rest).length > 1) out.view = rest;
  else delete out.view;
  return out;
}

/** The project of the next sidebar space that is not on screen, after the
 *  focused one and wrapping — what "open the next project beside" opens. */
export function nextUnshown(layout: Layout, order: readonly ProjectId[]): ProjectId | null {
  const shown = new Set(shownProjects(layout));
  const start = Math.max(0, order.indexOf(layout.activeProject ?? ""));
  for (let i = 1; i <= order.length; i += 1) {
    const pid = order[(start + i) % order.length]!;
    if (!shown.has(pid)) return pid;
  }
  return null;
}

export function setViewRatio(layout: Layout, splitId: string, ratio: number): Layout {
  if (!layout.view) return layout;
  const view = setRatio(layout.view, splitId, ratio);
  return view === layout.view ? layout : { ...layout, view };
}

// ── normalize: the invariant, repaired ───────────────────────────────────────

export type Pool = {
  servers: Pick<Server, "id">[];
  projects: Pick<Project, "id">[];
  sessions: Pick<SessionV3, "id" | "projectId" | "name">[];
};

/**
 * Bring a layout back in line with the pool — run on load and on every pool
 * change, so no code path has to be trusted to have placed or removed things.
 *
 * 1. **Projects that no longer exist are dropped**, tabs and all.
 * 2. **A leaf whose target is gone is dropped** (a closed session, a removed
 *    server or project) — collapsing its split, removing an emptied tab.
 * 3. **A session leaf outside its own project's tabs is dropped**, then
 *    re-placed by rule 5 where it belongs.
 * 4. **A session in two leaves keeps the first** — two panes on one Claude
 *    conversation both write to it. "First" is project order, then tab order,
 *    then tree order, so the outcome is deterministic.
 * 5. **A session in no leaf gets a tab of its own** in its project, appended in
 *    rail order. A session that quietly stops being reachable looks identical
 *    to one that was never there; this is what makes that impossible.
 * 6. Focus, active tab and active project are repaired to something that exists.
 *
 * Returns the input unchanged (same reference) when nothing needed repair, so
 * the store can skip a write.
 */
export function normalizeLayout(layout: Layout, pool: Pool, mint: Mint): Layout {
  const sessionProject = new Map(pool.sessions.map((s) => [s.id, s.projectId]));
  const projectIds = new Set(pool.projects.map((p) => p.id));
  const serverIds = new Set(pool.servers.map((s) => s.id));
  const placed = new Set<string>();
  let changed = false;

  const projects: Record<ProjectId, ProjectTabs> = {};
  for (const p of pool.projects) {
    const pt = layout.projects[p.id];
    if (!pt) continue;
    const tabs: Tab[] = [];
    for (const tab of pt.tabs) {
      const root = filterLeaves(tab.root, (l) => {
        const pane = l.pane;
        if (pane.kind === "session") {
          if (sessionProject.get(pane.id) !== p.id) return false; // gone, or not ours
          if (placed.has(pane.id)) return false; // a duplicate — first wins
          placed.add(pane.id);
          return true;
        }
        if (pane.kind === "host") return serverIds.has(pane.serverId);
        return projectIds.has(pane.projectId);
      });
      if (!root) {
        changed = true;
        continue;
      }
      const focus = findLeaf(root, tab.focus) ? tab.focus : leaves(root)[0]!.id;
      const zoomed = tab.zoomed && leaves(root).length > 1 ? true : undefined;
      if (root !== tab.root || focus !== tab.focus || zoomed !== tab.zoomed) changed = true;
      tabs.push(root === tab.root && focus === tab.focus && zoomed === tab.zoomed ? tab : { ...tab, root, focus, zoomed });
    }
    const activeTab = tabs.some((t) => t.id === pt.activeTab) ? pt.activeTab : (tabs[0]?.id ?? null);
    if (activeTab !== pt.activeTab || tabs.length !== pt.tabs.length) changed = true;
    projects[p.id] = tabs.length === pt.tabs.length && !changed && activeTab === pt.activeTab ? pt : { tabs, activeTab };
  }
  if (Object.keys(layout.projects).some((id) => !projectIds.has(id))) changed = true;

  let next: Layout = { activeProject: layout.activeProject, projects, view: layout.view };
  for (const s of pool.sessions) {
    if (placed.has(s.id) || !projectIds.has(s.projectId)) continue;
    changed = true;
    placed.add(s.id);
    next = addTab(next, s.projectId, { kind: "session", id: s.id }, s.name, mint, { activate: false }).layout;
  }

  // 7. The project grid: tiles whose project is gone are dropped, a project
  //    in two tiles keeps the first, and the focus must be one of the tiles.
  const inView = new Set<ProjectId>();
  let view: ProjectTree | null | undefined = layout.view
    ? filterLeaves(layout.view, (t) => {
        if (!projectIds.has(t.project) || inView.has(t.project)) return false;
        inView.add(t.project);
        return true;
      })
    : undefined;

  let activeProject = next.activeProject;
  if (!activeProject || !projectIds.has(activeProject)) {
    // The focused tile's project went: focus passes to a tile still on
    // screen, never to a project the grid is not showing.
    activeProject =
      (view ? leaves(view)[0]?.project : undefined) ??
      pool.projects.find((p) => tabsOf(next, p.id).tabs.length > 0)?.id ??
      pool.projects[0]?.id ??
      null;
  }
  if (view && activeProject && !inView.has(activeProject)) {
    // Something moved the focus without going through `selectProject`. The
    // focus is the fact; the first tile takes it rather than the grid showing
    // a focus that is nowhere on screen.
    const first = leaves(view)[0]!;
    view = mapTiles(view, (t) => (t.id === first.id ? { ...t, project: activeProject! } : t));
  }
  // One tile is not a grid — it is the plain single-project view.
  if (view && leaves(view).length < 2) view = undefined;
  if ((view ?? undefined) !== layout.view) changed = true;
  if (activeProject !== layout.activeProject) changed = true;
  if (!changed) return layout;
  const out: Layout = { ...next, activeProject };
  if (view) out.view = view;
  else delete out.view;
  return out;
}

/**
 * Which tabs stay mounted while hidden — the **warm set**, the tab-model twin of
 * the grid's `warmSessions`. Every live pane holds an xterm and a WebGL context,
 * so keeping every tab ever opened mounted is how RAM tracks the day rather than
 * the screen; keeping only the visible one rebuilds a pane on every glance away
 * and back. The active tab is always first; then most-recent; deduplicated;
 * tabs that no longer exist are dropped; capped at `keep`.
 */
export function warmTabs(
  active: string | null,
  recent: readonly string[],
  exists: (tabId: string) => boolean,
  keep: number,
): string[] {
  const out: string[] = [];
  for (const id of [active, ...recent]) {
    if (!id || out.includes(id) || !exists(id)) continue;
    out.push(id);
    if (out.length >= keep) break;
  }
  return out;
}

// ── persistence and migration ────────────────────────────────────────────────

/**
 * The persisted workspace, v4: the v3 pool unchanged, with the grid's `panes`
 * replaced by a `layout`. `activeSession` stays — it is what the instruments,
 * notifications and the control socket read as "the session being looked at".
 */
export type PersistedV4 = Omit<PersistedV3, "version" | "panes"> & {
  version: 4;
  layout: Layout;
};

/** Ids for migration: deterministic, so a migration is reproducible in a test
 *  and never collides with the clock-minted ids the store uses afterwards. */
export function migrationMint(): Mint {
  let n = 0;
  return (prefix) => `${prefix}-v4m-${++n}`;
}

/**
 * The v3 → v4 layout: **every session becomes its own tab**, in rail order,
 * named after the session; the active session's tab and project are selected.
 *
 * The v3 `panes` grid assignment is dropped on purpose — a grid cell has no
 * meaning in a tree, and guessing a split arrangement from it would invent one
 * the operator never made. Host and files cells go with it; both are one click
 * from the rail. **No session is dropped and no id changes**: `reattachName`
 * derives the daemon's session name from the id, so a changed id is a running
 * shell or Claude conversation that nothing can reach again.
 */
export function migrateLayoutV3(
  v3: Pick<PersistedV3, "servers" | "projects" | "sessions" | "activeSession">,
  mint: Mint = migrationMint(),
): Layout {
  let layout = normalizeLayout(EMPTY_LAYOUT, v3, mint);
  const loc = v3.activeSession ? locateSession(layout, v3.activeSession) : null;
  if (loc) layout = focusLeaf(layout, loc.projectId, loc.tabId, loc.leafId);
  return layout;
}

export function migrateV3toV4(v3: PersistedV3, mint: Mint = migrationMint()): PersistedV4 {
  const { panes: _panes, version: _v, ...rest } = v3;
  return { ...rest, version: 4, layout: migrateLayoutV3(v3, mint) };
}

/** Shape-check a stored layout; anything malformed degrades to empty rather
 *  than crashing the workbench on boot (normalize then re-places everything). */
export function parseLayout(raw: unknown): Layout {
  if (!raw || typeof raw !== "object") return EMPTY_LAYOUT;
  const l = raw as Partial<Layout>;
  const projects: Record<ProjectId, ProjectTabs> = {};
  const isNode = (n: unknown): n is SplitNode => {
    if (!n || typeof n !== "object") return false;
    const x = n as Partial<SplitNode>;
    if (typeof x.id !== "string") return false;
    if (x.kind === "leaf") return !!(x as Leaf).pane && typeof (x as Leaf).pane.kind === "string";
    if (x.kind === "split") {
      const s = x as Split;
      return (s.dir === "row" || s.dir === "col") && isNode(s.a) && isNode(s.b);
    }
    return false;
  };
  for (const [pid, pt] of Object.entries(l.projects ?? {})) {
    if (!pt || !Array.isArray(pt.tabs)) continue;
    const tabs = pt.tabs.filter(
      (t): t is Tab => !!t && typeof t.id === "string" && typeof t.focus === "string" && isNode(t.root),
    );
    projects[pid] = { tabs, activeTab: typeof pt.activeTab === "string" ? pt.activeTab : null };
  }
  const out: Layout = { activeProject: typeof l.activeProject === "string" ? l.activeProject : null, projects };
  // The project grid is optional: a layout saved before it existed has no
  // `view` and is one tile, exactly as it was; a malformed one is dropped
  // rather than costing the tabs beside it.
  const isTile = (n: unknown): n is ProjectTree => {
    if (!n || typeof n !== "object") return false;
    const x = n as Partial<ProjectTree>;
    if (typeof x.id !== "string") return false;
    if (x.kind === "leaf") return typeof (x as ProjectLeaf).project === "string";
    if (x.kind === "split") {
      const s = x as SplitOf<ProjectLeaf>;
      return (s.dir === "row" || s.dir === "col") && isTile(s.a) && isTile(s.b);
    }
    return false;
  };
  if (isTile(l.view) && l.view.kind === "split") out.view = l.view;
  return out;
}

// ── saved views ──────────────────────────────────────────────────────────────

/**
 * A saved view is one tab's split tree. Recalled, it opens as a new tab; panes
 * whose session has since ended are skipped, so a view never resurrects a dead
 * session or errors on a missing one.
 */
export type SavedView = { id: string; name: string; root: SplitNode };

/**
 * Read stored views, carrying the old grid shape (`{ deck, panes }`) forward as
 * an equal-share tree rather than dropping views someone saved.
 */
export function parseSavedViews(raw: unknown, mint: Mint): SavedView[] {
  if (!Array.isArray(raw)) return [];
  const out: SavedView[] = [];
  for (const v of raw) {
    if (!v || typeof v.id !== "string" || typeof v.name !== "string") continue;
    if (v.root && typeof v.root === "object") {
      const layout = parseLayout({ projects: { x: { tabs: [{ id: "t", focus: "f", root: v.root }] } } });
      if (layout.projects.x?.tabs.length) out.push({ id: v.id, name: v.name, root: v.root as SplitNode });
      continue;
    }
    if (v.deck && typeof v.deck.cols === "number" && Array.isArray(v.panes)) {
      const panes = (v.panes as (PaneRef | null)[]).filter(
        (p): p is PaneRef => !!p && typeof p.kind === "string",
      );
      const root = treeFromPanes(panes, v.deck.cols, mint);
      if (root) out.push({ id: v.id, name: v.name, root });
    }
  }
  return out;
}
