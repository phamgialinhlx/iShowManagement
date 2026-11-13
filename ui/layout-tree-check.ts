/**
 * The split-tree layout (`src/lib/layout-tree.ts`): tree operations, geometric
 * focus, zoom, swap, the ratio clamp, the v3 → v4 migration and `normalize`.
 *
 * Run it either way:
 *
 *     node --experimental-strip-types ui/layout-tree-check.ts
 *     open http://localhost:5273/layout-tree-check.html
 *
 * Every failure this guards is silent in the app. A session that stops being in
 * any tab is unreachable and looks exactly like one that never existed; a
 * session in two leaves mounts its view twice, which for Claude is two panes
 * writing to one conversation; and a migration that changes an id orphans a
 * running shell on a host, because `reattachName` derives the daemon's name
 * from it.
 */
import {
  EMPTY_LAYOUT,
  activeTab,
  addTab,
  clampRatio,
  closeLeaf,
  cycleTab,
  findLeaf,
  focusInDirection,
  focusLeaf,
  leafRects,
  leaves,
  locateSession,
  migrateLayoutV3,
  migrateV3toV4,
  moveTab,
  neighbour,
  normalizeLayout,
  parseLayout,
  parseSavedViews,
  removeSessionLeaves,
  renameTab,
  selectTabIndex,
  setRatio,
  splitFocused,
  splitLeaf,
  swapInDirection,
  tabLabel,
  tabsOf,
  toggleZoom,
  treeFromPanes,
  visibleLeaves,
  warmTabs,
  closeTile,
  nextUnshown,
  openBeside,
  placeTile,
  selectProject,
  setViewRatio,
  shownProjects,
  tileOf,
  viewTree,
  SOLO_TILE,
  canMoveBeside,
  moveBeside,
  type Direction,
  type Layout,
  type Leaf,
  type Mint,
  type Pool,
  type SplitNode,
} from "./src/lib/layout-tree.ts";
import { reattachName, type PersistedV3, type SessionV3 } from "./src/lib/workspace-model.ts";

const counterMint = (): Mint => {
  let n = 0;
  return (p) => `${p}-t${++n}`;
};
const leaf = (id: string, sid = id): Leaf => ({ kind: "leaf", id, pane: { kind: "session", id: sid } });
const row = (id: string, a: SplitNode, b: SplitNode, ratio = 0.5): SplitNode => ({ kind: "split", id, dir: "row", ratio, a, b });
const col = (id: string, a: SplitNode, b: SplitNode, ratio = 0.5): SplitNode => ({ kind: "split", id, dir: "col", ratio, a, b });
const ids = (n: SplitNode) => leaves(n).map((l) => l.id).join(",");

/** All session ids placed anywhere in a layout, with multiplicity. */
function placed(layout: Layout): { sid: string; pid: string }[] {
  const out: { sid: string; pid: string }[] = [];
  for (const [pid, pt] of Object.entries(layout.projects))
    for (const t of pt.tabs)
      for (const l of leaves(t.root)) if (l.pane.kind === "session") out.push({ sid: l.pane.id, pid });
  return out;
}

/** The invariant: every session in exactly one leaf, in its own project. */
function holdsInvariant(layout: Layout, pool: Pool): string | null {
  const where = placed(layout);
  for (const s of pool.sessions) {
    const hits = where.filter((w) => w.sid === s.id);
    if (hits.length !== 1) return `${s.id} placed ${hits.length} times`;
    if (hits[0]!.pid !== s.projectId) return `${s.id} placed in ${hits[0]!.pid}, belongs to ${s.projectId}`;
  }
  for (const w of where) if (!pool.sessions.some((s) => s.id === w.sid)) return `dead session ${w.sid} still placed`;
  return null;
}

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail && !ok ? ` — ${detail}` : ""}`);
    if (!ok) failures++;
  };

  // ── split / close / collapse ───────────────────────────────────────────────
  {
    const mint = counterMint();
    let L = addTab(EMPTY_LAYOUT, "P", { kind: "session", id: "A" }, "A", mint).layout;
    const t0 = activeTab(L)!.tab;
    check("a new tab is active, with its project", L.activeProject === "P" && tabsOf(L, "P").activeTab === t0.id);

    const r1 = splitFocused(L, "right", { kind: "session", id: "B" }, mint)!;
    L = r1.layout;
    let tab = activeTab(L)!.tab;
    check("split right makes a row split, old pane left", tab.root.kind === "split" && tab.root.dir === "row" && ids(tab.root) === `${t0.focus},${r1.leafId}`);
    check("the new half takes the focus", tab.focus === r1.leafId);

    const r2 = splitFocused(L, "down", { kind: "session", id: "C" }, mint)!;
    L = r2.layout;
    tab = activeTab(L)!.tab;
    const inner = tab.root.kind === "split" ? tab.root.b : null;
    check("split down nests a col split inside the focused half", inner?.kind === "split" && inner.dir === "col");

    // Close the focused pane (C): its parent collapses back to B alone.
    L = closeLeaf(L, "P", tab.id, r2.leafId);
    tab = activeTab(L)!.tab;
    check("closing collapses the parent split", tab.root.kind === "split" && tab.root.b.kind === "leaf" && tab.root.b.id === r1.leafId);
    check("focus passes to the pane that took its place", tab.focus === r1.leafId);

    L = closeLeaf(L, "P", tab.id, r1.leafId);
    tab = activeTab(L)!.tab;
    check("closing down to one pane leaves a bare leaf", tab.root.kind === "leaf" && tab.root.id === t0.focus);

    const t2 = addTab(L, "P", { kind: "session", id: "D" }, "D", mint);
    L = closeLeaf(t2.layout, "P", t2.tabId, t2.leafId);
    check("closing a tab's last pane removes the tab", tabsOf(L, "P").tabs.length === 1);
    check("and the active tab passes to a survivor", tabsOf(L, "P").activeTab === t0.id);

    check("removeLeaf on the whole tree yields null via closeLeaf→removeTab",
      tabsOf(closeLeaf(L, "P", t0.id, t0.focus), "P").tabs.length === 0);
    check("splitting an unknown leaf changes nothing",
      splitLeaf(leaf("x"), "nope", "row", leaf("y"), "s") .kind === "leaf");
  }

  // ── geometric focus ───────────────────────────────────────────────────────
  {
    // [A | [B / C]], B taller (0.6) so A's centre is level with B.
    const t = row("s1", leaf("A"), col("s2", leaf("B"), leaf("C"), 0.6));
    check("right of A is the pane level with its centre (B)", neighbour(t, "A", "right") === "B");
    check("up from C is B", neighbour(t, "C", "up") === "B");
    check("left from C is A, across tree levels", neighbour(t, "C", "left") === "A");
    check("left from A is nothing — an edge is a no-op", neighbour(t, "A", "left") === null);
    check("down from B is C", neighbour(t, "B", "down") === "C");
    check("down from C is nothing", neighbour(t, "C", "down") === null);

    // [[A / B] | [C / D]] — a 2x2 made of two columns.
    const g = row("r", col("c1", leaf("A"), leaf("B")), col("c2", leaf("C"), leaf("D")));
    check("2x2: right from B is D, not C", neighbour(g, "B", "right") === "D");
    check("2x2: right from A is C", neighbour(g, "A", "right") === "C");
    check("2x2: left from D is B", neighbour(g, "D", "left") === "B");

    // Round trip: right then left returns to the start.
    const back = neighbour(t, neighbour(t, "A", "right")!, "left");
    check("right then left comes back", back === "A");

    // Through the layout API: focus moves and the leaf is reported.
    const mint = counterMint();
    let L = addTab(EMPTY_LAYOUT, "P", { kind: "session", id: "A" }, "A", mint).layout;
    L = splitFocused(L, "right", { kind: "session", id: "B" }, mint)!.layout;
    const moved = focusInDirection(L, "left");
    check("focusInDirection reports the landed leaf", moved.leaf?.pane.kind === "session" && moved.leaf.pane.id === "A");
    check("and an edge reports none", focusInDirection(moved.layout, "left").leaf === null);
  }

  // ── zoom ──────────────────────────────────────────────────────────────────
  {
    const mint = counterMint();
    let L = addTab(EMPTY_LAYOUT, "P", { kind: "session", id: "A" }, "A", mint).layout;
    check("zoom with one pane is a no-op", toggleZoom(L) === L);
    L = splitFocused(L, "right", { kind: "session", id: "B" }, mint)!.layout;
    L = toggleZoom(L);
    check("zoom sets the flag", activeTab(L)!.tab.zoomed === true);
    check("while zoomed only the focused pane is on screen",
      visibleLeaves(L).length === 1 && visibleLeaves(L)[0]!.id === activeTab(L)!.tab.focus);
    check("the tab label shows Z only via the flag (label itself unchanged)",
      tabLabel(activeTab(L)!.tab, 0, (id) => `name-${id}`) === "name-B");
    const other = leaves(activeTab(L)!.tab.root).find((l) => l.id !== activeTab(L)!.tab.focus)!;
    const refocused = focusLeaf(L, "P", activeTab(L)!.tab.id, other.id);
    check("focusing another pane un-zooms", !activeTab(refocused)!.tab.zoomed);
    check("toggling again un-zooms", !activeTab(toggleZoom(L))!.tab.zoomed);
    check("splitting while zoomed un-zooms",
      !activeTab(splitFocused(L, "down", { kind: "session", id: "C" }, mint)!.layout)!.tab.zoomed);
  }

  // ── swap ──────────────────────────────────────────────────────────────────
  {
    const L: Layout = {
      activeProject: "P",
      projects: { P: { activeTab: "t", tabs: [{ id: "t", name: "t", focus: "A", root: row("s", leaf("A", "sa"), leaf("B", "sb")) }] } },
    };
    const S = swapInDirection(L, "right");
    const tab = activeTab(S)!.tab;
    check("swap moves the focused pane to the neighbour's place", ids(tab.root) === "B,A");
    check("the focus travels with the pane", tab.focus === "A" && findLeaf(tab.root, "A")!.pane.kind === "session");
    check("swap at an edge is a no-op", swapInDirection(L, "left") === L);
  }

  // ── ratio clamp ───────────────────────────────────────────────────────────
  {
    check("a ratio below 0.1 clamps to 0.1", clampRatio(0.01) === 0.1);
    check("a ratio above 0.9 clamps to 0.9", clampRatio(5) === 0.9);
    check("NaN becomes an even split", clampRatio(Number.NaN) === 0.5);
    const t = setRatio(row("s", leaf("A"), leaf("B")), "s", 0.02);
    check("setRatio clamps what it stores", t.kind === "split" && t.ratio === 0.1);
    const r = leafRects(row("s", leaf("A"), leaf("B"), 0.3));
    check("rects follow the ratio", Math.abs(r.A!.w - 0.3) < 1e-9 && Math.abs(r.B!.x - 0.3) < 1e-9 && Math.abs(r.B!.w - 0.7) < 1e-9);
    const o = leafRects(row("s", leaf("A"), leaf("B"), 0.3), undefined, { splitId: "s", ratio: 0 });
    check("a drag override is clamped too", Math.abs(o.A!.w - 0.1) < 1e-9);
  }

  // ── tabs: index, cycle, move, rename ──────────────────────────────────────
  {
    const mint = counterMint();
    let L = EMPTY_LAYOUT;
    for (const id of ["a", "b", "c"]) L = addTab(L, "P", { kind: "session", id }, id, mint).layout;
    const tabIds = tabsOf(L, "P").tabs.map((t) => t.id);
    check("selectTabIndex picks by position", tabsOf(selectTabIndex(L, 0), "P").activeTab === tabIds[0]);
    check("an out-of-range index is a no-op", selectTabIndex(L, 9) === L);
    check("next wraps from the last to the first", tabsOf(cycleTab(L, 1), "P").activeTab === tabIds[0]);
    check("previous goes back one", tabsOf(cycleTab(L, -1), "P").activeTab === tabIds[1]);
    const M = moveTab(L, "P", 0, 2);
    check("moveTab reorders", tabsOf(M, "P").tabs.map((t) => t.id).join() === [tabIds[1], tabIds[2], tabIds[0]].join());
    const R = renameTab(L, "P", tabIds[0]!, "  build  ");
    const rt = tabsOf(R, "P").tabs[0]!;
    check("rename trims and latches", rt.name === "build" && rt.renamed === true && tabLabel(rt, 0, () => "x") === "build");
    const U = renameTab(R, "P", tabIds[0]!, "");
    check("an empty rename hands the label back to the session",
      !tabsOf(U, "P").tabs[0]!.renamed && tabLabel(tabsOf(U, "P").tabs[0]!, 0, (id) => `s:${id}`) === "s:a");
  }

  // ── removing a session drops its leaf everywhere ──────────────────────────
  {
    const mint = counterMint();
    let L = addTab(EMPTY_LAYOUT, "P", { kind: "session", id: "A" }, "A", mint).layout;
    L = splitFocused(L, "right", { kind: "session", id: "B" }, mint)!.layout;
    L = removeSessionLeaves(L, "B");
    check("the leaf goes and the split collapses", activeTab(L)!.tab.root.kind === "leaf");
    check("and the session is nowhere", locateSession(L, "B") === null);
  }

  // ── v3 → v4 migration ─────────────────────────────────────────────────────
  {
    const sessions: SessionV3[] = [
      { id: "session-1", projectId: "p1", kind: "claude", name: "api work" },
      { id: "term-1", projectId: "p1", kind: "terminal", name: "shell" },
      { id: "pisession-1", projectId: "p2", kind: "pi", name: "pi" },
      { id: "term-x", projectId: "p2", kind: "terminal", name: "adopted", hostName: "term-from-another-mac" },
      { id: "session-2", projectId: "p1", kind: "claude", name: "docs" },
    ];
    const v3: PersistedV3 = {
      version: 3,
      servers: [{ id: "srv", target: { host: "prod" } }],
      projects: [
        { id: "p1", serverId: "srv", folder: "/a", label: "a" },
        { id: "p2", serverId: "srv", folder: "/b", label: "b" },
        { id: "p3", serverId: "srv", folder: "/c", label: "c" },
      ],
      sessions,
      panes: [{ kind: "session", id: "term-1" }, { kind: "host", serverId: "srv" }, null, { kind: "session", id: "term-1" }],
      activeSession: "pisession-1",
      openPaths: { p1: ["/a/x.ts"] },
      activePath: { p1: "/a/x.ts" },
    };
    const before = sessions.map((s) => reattachName(s));
    const v4 = migrateV3toV4(v3);
    const where = placed(v4.layout);

    check("the version is 4", v4.version === 4);
    check("every session is placed exactly once, in its own project", holdsInvariant(v4.layout, v4) === null, holdsInvariant(v4.layout, v4) ?? "");
    check("no session is dropped", v4.sessions.length === sessions.length && where.length === sessions.length);
    check("every id is preserved verbatim",
      sessions.every((s, i) => v4.sessions[i]!.id === s.id && where.some((w) => w.sid === s.id)));
    check("so every reattach name is unchanged",
      v4.sessions.map((s) => reattachName(s)).join() === before.join());
    check("an adopted session keeps its host name", v4.sessions[3]!.hostName === "term-from-another-mac");
    check("each session is its own tab, named after it, in rail order",
      tabsOf(v4.layout, "p1").tabs.map((t) => t.name).join("|") === "api work|shell|docs" &&
        tabsOf(v4.layout, "p2").tabs.map((t) => t.name).join("|") === "pi|adopted");
    check("each tab is a single leaf (the grid arrangement is dropped)",
      Object.values(v4.layout.projects).every((pt) => pt.tabs.every((t) => t.root.kind === "leaf")));
    check("host panes from the old grid are dropped", !Object.values(v4.layout.projects).some((pt) => pt.tabs.some((t) => leaves(t.root).some((l) => l.pane.kind === "host"))));
    check("the active session's project and tab are selected",
      v4.layout.activeProject === "p2" && visibleLeaves(v4.layout)[0]?.pane.kind === "session" &&
        (visibleLeaves(v4.layout)[0]!.pane as { id: string }).id === "pisession-1");
    check("an empty project has no tabs (and is still a project)", tabsOf(v4.layout, "p3").tabs.length === 0 && v4.projects.length === 3);
    check("the pool is carried over untouched",
      v4.servers === v3.servers && v4.openPaths.p1?.[0] === "/a/x.ts" && v4.activePath.p1 === "/a/x.ts" && !("panes" in v4));
    check("migration is deterministic", JSON.stringify(migrateV3toV4(v3)) === JSON.stringify(v4));
    check("with no active session, the first project with tabs is selected",
      migrateLayoutV3({ ...v3, activeSession: null }).activeProject === "p1");
    check("an empty v3 migrates to an empty layout",
      Object.keys(migrateLayoutV3({ servers: [], projects: [], sessions: [], activeSession: null }).projects).length === 0);
  }

  // ── normalize repairs ─────────────────────────────────────────────────────
  {
    const pool: Pool = {
      servers: [{ id: "srv" }],
      projects: [{ id: "p1" }, { id: "p2" }],
      sessions: [
        { id: "s1", projectId: "p1", name: "one" },
        { id: "s2", projectId: "p1", name: "two" },
        { id: "s3", projectId: "p2", name: "three" },
        { id: "s4", projectId: "p1", name: "four" },
      ],
    };
    const broken: Layout = {
      activeProject: "gone",
      projects: {
        p1: {
          activeTab: "nope",
          tabs: [
            // s1, plus a dead session — the dead leaf goes and the split collapses.
            { id: "t1", name: "t1", focus: "dead", root: row("x1", leaf("l1", "s1"), leaf("dead", "zz")) },
            // s1 again — a duplicate; the first wins, this tab empties and goes.
            { id: "t2", name: "t2", focus: "l2", root: leaf("l2", "s1") },
            // s3 is p2's — not allowed here; dropped and re-placed in p2.
            { id: "t3", name: "t3", focus: "l3", root: row("x3", leaf("l3", "s2"), leaf("l3b", "s3")), zoomed: true },
          ],
        },
        // A project that no longer exists.
        gone: { activeTab: "tg", tabs: [{ id: "tg", name: "g", focus: "lg", root: leaf("lg", "s1") }] },
      },
    };
    const N = normalizeLayout(broken, pool, counterMint());
    const err = holdsInvariant(N, pool);
    check("after normalize every session is in exactly one leaf of its own project", err === null, err ?? "");
    check("a leaf on a dead session is dropped and its split collapses",
      tabsOf(N, "p1").tabs.find((t) => t.id === "t1")?.root.kind === "leaf");
    check("a focus on a missing leaf is repaired", tabsOf(N, "p1").tabs.find((t) => t.id === "t1")?.focus === "l1");
    check("a duplicate keeps the first; the emptied tab is removed", !tabsOf(N, "p1").tabs.some((t) => t.id === "t2"));
    check("a session in the wrong project's tab moves to its own", locateSession(N, "s3")?.projectId === "p2");
    check("zoom with one pane left is cleared", !tabsOf(N, "p1").tabs.find((t) => t.id === "t3")?.zoomed);
    check("a session missing from every tab gets a tab of its own",
      tabsOf(N, "p1").tabs.some((t) => t.root.kind === "leaf" && t.root.pane.kind === "session" && t.root.pane.id === "s4" && t.name === "four"));
    check("a removed project's tabs are dropped", !("gone" in N.projects));
    check("an invalid active tab is repaired", tabsOf(N, "p1").tabs.some((t) => t.id === tabsOf(N, "p1").activeTab));
    check("an invalid active project is repaired", N.activeProject === "p1");
    check("normalize is idempotent — a clean layout comes back by reference",
      normalizeLayout(N, pool, counterMint()) === N);
    check("host and files leaves survive while their target exists",
      normalizeLayout(
        { activeProject: "p1", projects: { p1: { activeTab: "h", tabs: [{ id: "h", name: "h", focus: "hl", root: row("hs", { kind: "leaf", id: "hl", pane: { kind: "host", serverId: "srv" } }, { kind: "leaf", id: "fl", pane: { kind: "files", projectId: "p2" } }) }] } } },
        { ...pool, sessions: [] },
        counterMint(),
      ).projects.p1!.tabs[0]!.root.kind === "split");
    check("a host leaf on a removed server is dropped",
      normalizeLayout(
        { activeProject: "p1", projects: { p1: { activeTab: "h", tabs: [{ id: "h", name: "h", focus: "hl", root: { kind: "leaf", id: "hl", pane: { kind: "host", serverId: "old" } } }] } } },
        { ...pool, sessions: [] },
        counterMint(),
      ).projects.p1!.tabs.length === 0);
    check("a corrupt stored layout parses to empty rather than throwing",
      Object.keys(parseLayout({ projects: { p1: { tabs: [{ id: 1 }, null], activeTab: 3 } } }).projects.p1!.tabs).length === 0 &&
        parseLayout("garbage").activeProject === null);
  }

  // ── warm set ──────────────────────────────────────────────────────────────
  {
    const exists = (id: string) => id !== "dead";
    check("the active tab is warm and first", warmTabs("a", ["b", "c"], exists, 3).join() === "a,b,c");
    check("deduplicated, dead dropped, capped",
      warmTabs("a", ["a", "dead", "b", "c", "d"], exists, 3).join() === "a,b,c");
    check("no active tab still warms recent ones", warmTabs(null, ["b"], exists, 3).join() === "b");
  }

  // ── saved views carried from the old grid ─────────────────────────────────
  {
    const tree = treeFromPanes(
      ["a", "b", "c", "d"].map((id) => ({ kind: "session" as const, id })),
      2,
      counterMint(),
    )!;
    const r = leafRects(tree);
    check("a 2x2 grid becomes four equal panes",
      Object.values(r).length === 4 && Object.values(r).every((x) => Math.abs(x.w - 0.5) < 1e-9 && Math.abs(x.h - 0.5) < 1e-9));
    const three = leafRects(treeFromPanes(["a", "b", "c"].map((id) => ({ kind: "session" as const, id })), 3, counterMint())!);
    check("a 1x3 row is three equal columns", Object.values(three).every((x) => Math.abs(x.w - 1 / 3) < 1e-9));
    const views = parseSavedViews(
      [
        { id: "v1", name: "old grid", deck: { cols: 2, rows: 2 }, panes: [{ kind: "session", id: "a" }, null, { kind: "host", serverId: "srv" }] },
        { id: "v2", name: "new tree", root: row("s", leaf("x"), leaf("y")) },
        { id: "v3", name: "junk" },
        "nonsense",
      ],
      counterMint(),
    );
    check("an old grid view is carried forward as a tree, empty cells skipped",
      views.length === 2 && leaves(views[0]!.root).length === 2);
    check("a tree view round-trips", views[1]!.name === "new tree" && leaves(views[1]!.root).length === 2);
  }

  // ── the project grid ───────────────────────────────────────────────────────
  {
    const mint = counterMint();
    // Three projects, each with one tab of one session; P1 focused, one tile.
    let L: Layout = EMPTY_LAYOUT;
    for (const [pid, sid] of [["P1", "A"], ["P2", "B"], ["P3", "C"]] as const) {
      L = addTab(L, pid, { kind: "session", id: sid }, sid, mint, { activate: false }).layout;
    }
    L = selectProject(L, "P1");
    const pool: Pool = {
      servers: [],
      projects: [{ id: "P1" }, { id: "P2" }, { id: "P3" }],
      sessions: [
        { id: "A", projectId: "P1", name: "A" },
        { id: "B", projectId: "P2", name: "B" },
        { id: "C", projectId: "P3", name: "C" },
      ],
    };
    const sessionsOnScreen = (l: Layout) =>
      visibleLeaves(l).flatMap((x) => (x.pane.kind === "session" ? [x.pane.id] : [])).sort().join(",");

    check("no view is one tile showing the active project",
      !L.view && shownProjects(L).join() === "P1" && viewTree(L)?.kind === "leaf" && viewTree(L)!.id === SOLO_TILE);
    check("one tile: visible leaves are the active tab's", sessionsOnScreen(L) === "A");

    // A wide window: the focused tile is wider than tall, so beside = right.
    let G = openBeside(L, "P2", mint, 16 / 9);
    check("open beside adds a second tile, split right in a wide window",
      G.view?.kind === "split" && G.view.dir === "row" && shownProjects(G).join() === "P1,P2");
    check("the existing tile keeps its id (no remount)", G.view?.kind === "split" && G.view.a.id === SOLO_TILE);
    check("the new tile takes the focus", G.activeProject === "P2");
    check("visible leaves span every tile", sessionsOnScreen(G) === "A,B");

    // P2's tile is now 0.5 × 1 of a 16:9 area — still wider than tall (8:9 → tall). 0.5*16/9 = 0.89 < 1 → down.
    G = openBeside(G, "P3", mint, 16 / 9);
    const p2tile = tileOf(G, "P2")!;
    check("a tall focused tile opens the next one below",
      G.view?.kind === "split" && G.view.b.kind === "split" && G.view.b.dir === "col" && shownProjects(G).join() === "P1,P2,P3");
    check("three tiles: all three active tabs on screen", sessionsOnScreen(G) === "A,B,C");

    // A project is in at most one tile.
    const again = openBeside(G, "P1", mint);
    check("open beside on a shown project only focuses its tile",
      shownProjects(again).join() === "P1,P2,P3" && again.activeProject === "P1" && again.view === G.view);
    const clicked = selectProject(G, "P1");
    check("selecting a shown project focuses its tile, view untouched",
      clicked.activeProject === "P1" && clicked.view === G.view);
    const moved = placeTile(G, "P1", p2tile.id, "up", mint);
    check("placing a shown project elsewhere moves it (still once)",
      shownProjects(moved).filter((p) => p === "P1").length === 1 && shownProjects(moved).length === 3);
    check("…on the side it was dropped", (() => {
      const t = viewTree(moved)!;
      return t.kind === "split" && t.kind === "split" && leaves(t)[0]!.project === "P1" && t.dir === "col";
    })());

    // Replacing: clicking a project not on screen swaps the *focused* tile.
    const four = addTab(G, "P4", { kind: "session", id: "D" }, "D", mint, { activate: false }).layout;
    const swapped = selectProject(four, "P4");
    check("selecting an unshown project replaces the focused tile's project",
      shownProjects(swapped).join() === "P1,P2,P4" && swapped.activeProject === "P4");
    check("…and the replaced project's tabs are untouched", tabsOf(swapped, "P3").tabs.length === 1);
    const viaTab = focusLeaf(four, "P4", tabsOf(four, "P4").tabs[0]!.id, tabsOf(four, "P4").tabs[0]!.focus);
    check("activating a session in an unshown project brings it into the focused tile",
      shownProjects(viaTab).includes("P4") && viaTab.activeProject === "P4");
    check("nextUnshown skips what is on screen", nextUnshown(four, ["P1", "P2", "P3", "P4"]) === "P4" &&
      nextUnshown(G, ["P1", "P2", "P3"]) === null);

    // Focus crossing tiles: from A (left tile) right → the P2/P3 column.
    const atA = selectProject(G, "P1");
    const right = focusInDirection(atA, "right");
    check("focus right at a tile edge crosses into the tile beside",
      right.layout.activeProject === "P2" && right.leaf?.pane.kind === "session" && right.leaf.pane.id === "B");
    const down = focusInDirection(right.layout, "down");
    check("focus down crosses into the tile below", down.layout.activeProject === "P3" && down.leaf?.pane.kind === "session" && down.leaf.pane.id === "C");
    const left = focusInDirection(down.layout, "left");
    check("focus left from the lower tile returns to the left tile", left.layout.activeProject === "P1");
    const edge = focusInDirection(atA, "left");
    check("focus off the outer edge is a no-op", edge.layout === atA && edge.leaf === null);
    // Within a tile still wins: split P1, then right from its left pane stays in P1.
    const sp = splitFocused(atA, "right", { kind: "session", id: "E" }, mint)!;
    const leftOfE = focusInDirection(sp.layout, "left");
    check("a neighbour pane inside the tile wins over the tile beside", leftOfE.layout.activeProject === "P1" &&
      leftOfE.leaf?.pane.kind === "session" && leftOfE.leaf.pane.id === "A");
    // An empty tile is still a target.
    const emptyP = addTab(EMPTY_LAYOUT, "Q1", { kind: "session", id: "Z" }, "Z", mint).layout;
    const withEmpty = openBeside(emptyP, "Q2", mint, 16 / 9);
    const back = focusInDirection(selectProject(withEmpty, "Q1"), "right");
    check("focus can cross into a tile whose project has no tab", back.layout.activeProject === "Q2" && back.leaf === null);

    // Ratios clamp like pane dividers.
    const splitId = G.view!.kind === "split" ? G.view!.id : "";
    const r = setViewRatio(G, splitId, 0.02);
    check("tile dividers clamp to 0.1", r.view?.kind === "split" && r.view.ratio === 0.1);

    // Closing a tile removes the tile only.
    const p3tile = tileOf(G, "P3")!;
    let C = closeTile(G, p3tile.id);
    check("closing a tile collapses its split", shownProjects(C).join() === "P1,P2" && C.view?.kind === "split");
    check("…focus passes to the tile that took its place", C.activeProject === "P2");
    check("…and no tab or session is lost", tabsOf(C, "P3").tabs.length === 1 && !holdsInvariant(C, pool));
    C = closeTile(C, tileOf(C, "P2")!.id);
    check("down to one tile, the view is the plain single project again", !C.view && C.activeProject === "P1");
    check("the last tile cannot be closed", closeTile(C, SOLO_TILE) === C);
    const unfocused = closeTile(G, tileOf(G, "P1")!.id);
    check("closing an unfocused tile leaves the focus where it was", unfocused.activeProject === "P3");

    // Normalize: gone projects leave the grid; duplicates keep the first.
    const gone = normalizeLayout(G, { ...pool, projects: [{ id: "P1" }, { id: "P2" }], sessions: pool.sessions.slice(0, 2) }, mint);
    check("normalize drops a tile whose project is gone", shownProjects(gone).join() === "P1,P2" && gone.activeProject !== "P3");
    const dup: Layout = { ...G, view: { kind: "split", id: "s", dir: "row", ratio: 0.5,
      a: { kind: "leaf", id: "x", project: "P1" }, b: { kind: "leaf", id: "y", project: "P1" } }, activeProject: "P1" };
    const fixed = normalizeLayout(dup, pool, mint);
    check("normalize: a project in two tiles keeps one (and one tile is no grid)", !fixed.view && shownProjects(fixed).join() === "P1");
    check("normalize leaves a sound grid alone (same reference)", normalizeLayout(G, pool, mint) === G);
    const stray: Layout = { ...G, activeProject: "P4", projects: { ...G.projects, P4: four.projects.P4! } };
    const strayFixed = normalizeLayout(stray, { ...pool, projects: [...pool.projects, { id: "P4" }],
      sessions: [...pool.sessions, { id: "D", projectId: "P4", name: "D" }] }, mint);
    check("normalize puts a focus that is in no tile into one", shownProjects(strayFixed).includes("P4"));

    // Persistence: an old layout without `view` still parses, and a grid round-trips.
    const old = parseLayout(JSON.parse(JSON.stringify({ activeProject: "P1", projects: L.projects })));
    check("an old layout without view parses to one tile", !old.view && old.activeProject === "P1" && shownProjects(old).join() === "P1");
    const round = parseLayout(JSON.parse(JSON.stringify(G)));
    check("a grid round-trips through parseLayout", JSON.stringify(round.view) === JSON.stringify(G.view));
    const junk = parseLayout({ activeProject: "P1", projects: L.projects, view: { kind: "split", id: "s", a: 1 } });
    check("a malformed view is dropped, the tabs kept", !junk.view && tabsOf(junk, "P1").tabs.length === 1);
    check("every session still in exactly one leaf across the grid", !holdsInvariant(G, pool));
  }

  // ── drag a session / a tab beside a pane (`moveBeside`) ────────────────────
  {
    const mint = counterMint();
    const tab = (id: string, root: SplitNode, focus: string) => ({ id, name: id, root, focus });
    const base: Layout = {
      activeProject: "P",
      projects: {
        P: {
          activeTab: "T2",
          tabs: [
            tab("T1", row("r1", leaf("la", "A"), leaf("lb", "B")), "la"),
            tab("T2", leaf("lc", "C"), "lc"),
            tab("T3", row("r3", leaf("ld", "D"), leaf("le", "E")), "le"),
          ],
        },
        Q: { activeTab: "TQ", tabs: [tab("TQ", leaf("lq", "Q1"), "lq")] },
      },
    };
    const pool: Pool = {
      servers: [{ id: "S" }],
      projects: [{ id: "P" }, { id: "Q" }],
      sessions: ["A", "B", "C", "D", "E"].map((id) => ({ id, projectId: "P", name: id })).concat([{ id: "Q1", projectId: "Q", name: "Q1" }]),
    };
    check("move: the fixture holds the invariant", holdsInvariant(base, pool) === null);
    const T = (L: Layout, pid: string, id: string) => tabsOf(L, pid).tabs.find((t) => t.id === id);
    const at: { projectId: string; tabId: string; leafId: string } = { projectId: "P", tabId: "T1", leafId: "la" };

    const expect: Record<Direction, { dir: "row" | "col"; first: string }> = {
      left: { dir: "row", first: "lc" },
      right: { dir: "row", first: "la" },
      up: { dir: "col", first: "lc" },
      down: { dir: "col", first: "la" },
    };
    for (const side of ["left", "right", "up", "down"] as Direction[]) {
      const r = moveBeside(base, { kind: "session", sessionId: "C" }, at, side, mint);
      const t1 = r ? T(r.layout, "P", "T1") : undefined;
      const sp = t1?.root.kind === "split" ? t1.root.a : null;
      check(
        `move ${side}: the target pane splits ${expect[side].dir}, dragged pane on the ${side}`,
        !!sp && sp.kind === "split" && sp.dir === expect[side].dir && ids(sp) === (expect[side].first === "lc" ? "lc,la" : "la,lc"),
        sp ? ids(sp) : "no split",
      );
      check(`move ${side}: the leaf id is preserved (no remount)`, !!r && !!findLeaf(t1!.root, "lc") && r.focus === "lc");
      check(`move ${side}: the emptied single-pane tab is removed`, !!r && !T(r.layout, "P", "T2") && tabsOf(r.layout, "P").tabs.length === 2);
      check(`move ${side}: the dropped pane is focused, its tab active`, !!r && t1!.focus === "lc" && tabsOf(r.layout, "P").activeTab === "T1" && r.layout.activeProject === "P");
      check(`move ${side}: normalize has nothing to repair`, !!r && normalizeLayout(r.layout, pool, mint) === r.layout);
      check(`move ${side}: no session lost or doubled`, !!r && holdsInvariant(r.layout, pool) === null, r ? (holdsInvariant(r.layout, pool) ?? "") : "");
    }

    check("move: a drop on itself is a no-op",
      moveBeside(base, { kind: "session", sessionId: "A" }, at, "right", mint) === null &&
      !canMoveBeside(base, { kind: "session", sessionId: "A" }, at));
    check("move: a single-pane tab dropped on its own pane is a no-op",
      moveBeside(base, { kind: "tab", projectId: "P", tabId: "T2" }, { projectId: "P", tabId: "T2", leafId: "lc" }, "left", mint) === null);
    check("move: a tab dropped inside its own subtree is a no-op",
      moveBeside(base, { kind: "tab", projectId: "P", tabId: "T1" }, { projectId: "P", tabId: "T1", leafId: "lb" }, "left", mint) === null &&
      !canMoveBeside(base, { kind: "tab", projectId: "P", tabId: "T1" }, { projectId: "P", tabId: "T1", leafId: "lb" }));
    check("move: a target that does not exist is refused",
      !canMoveBeside(base, { kind: "session", sessionId: "C" }, { projectId: "P", tabId: "T1", leafId: "nope" }));

    // A multi-pane tab moves as its whole subtree, split ids and all.
    const m = moveBeside(base, { kind: "tab", projectId: "P", tabId: "T3" }, { projectId: "P", tabId: "T2", leafId: "lc" }, "right", mint);
    const t2 = m ? T(m.layout, "P", "T2") : undefined;
    check("move tab: a multi-pane tab moves its whole subtree beside the target",
      !!t2 && t2.root.kind === "split" && t2.root.a.kind === "leaf" && t2.root.a.id === "lc" &&
      t2.root.b.kind === "split" && t2.root.b.id === "r3" && ids(t2.root.b) === "ld,le");
    check("move tab: its own tab is gone, focus is its focused pane", !!m && !T(m.layout, "P", "T3") && t2!.focus === "le" && m.sessions.join() === "D,E");
    check("move tab: normalize has nothing to repair", !!m && normalizeLayout(m.layout, pool, mint) === m.layout);

    // A pane out of a bigger tab: its old split collapses, the tab stays.
    const b = moveBeside(base, { kind: "session", sessionId: "B" }, { projectId: "P", tabId: "T2", leafId: "lc" }, "down", mint);
    const t1b = b ? T(b.layout, "P", "T1") : undefined;
    check("move pane: the source split collapses, its tab stays", !!t1b && t1b.root.kind === "leaf" && t1b.root.id === "la" && t1b.focus === "la");
    check("move pane: normalize has nothing to repair", !!b && normalizeLayout(b.layout, pool, mint) === b.layout);

    // Zoom on the target tab is cleared, so the new half is actually seen.
    const zoomed: Layout = { ...base, projects: { ...base.projects, P: { ...base.projects.P!, tabs: base.projects.P!.tabs.map((t) => (t.id === "T1" ? { ...t, zoomed: true } : t)) } } };
    const z = moveBeside(zoomed, { kind: "session", sessionId: "C" }, at, "left", mint);
    check("move: a zoomed target tab is un-zoomed", !!z && !T(z.layout, "P", "T1")!.zoomed);

    // Cross-project: the session moves into the target's project; the source
    // project keeps its (now empty) entry, because the operator opened it.
    const x = moveBeside({ ...base, activeProject: "Q" }, { kind: "session", sessionId: "Q1" }, at, "right", mint);
    check("move across projects: reports where it came from", !!x && x.fromProject === "Q" && x.sessions.join() === "Q1");
    check("move across projects: the target project becomes active", !!x && x.layout.activeProject === "P");
    check("move across projects: the source project survives, empty", !!x && !!x.layout.projects.Q && tabsOf(x.layout, "Q").tabs.length === 0);
    const rehomed: Pool = { ...pool, sessions: pool.sessions.map((s) => (s.id === "Q1" ? { ...s, projectId: "P" } : s)) };
    check("move across projects: with the session re-homed, normalize has nothing to repair",
      !!x && normalizeLayout(x.layout, rehomed, mint) === x.layout && holdsInvariant(x.layout, rehomed) === null);
    check("move across projects: without re-homing, normalize would pull it back (why the store must)",
      !!x && normalizeLayout(x.layout, pool, mint) !== x.layout);
  }

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}

// Node entry: `node --experimental-strip-types ui/layout-tree-check.ts`.
if (typeof window === "undefined") {
  const ok = run((line) => console.log(line));
  if (!ok) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
}
