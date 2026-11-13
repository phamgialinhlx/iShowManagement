/**
 * Moving panes between tabs, and children past what a tab can hold
 * (`src/lib/layout-tree.ts`: `childPlacement` / `placeChild`, `moveToTab`,
 * `moveToNewTab`, the `leaf` move source).
 *
 *     node --experimental-strip-types ui/tab-move-check.ts
 *
 * What this guards is silent in the app: a child squeezed into a pane too small
 * to use looks like a broken TUI; one placed in an overflow tab that took the
 * focus takes the keyboard from someone mid-sentence; and a move that minted a
 * fresh leaf id remounts the terminal behind it, while one that dropped a leaf
 * leaves a session running with nothing on screen to reach it.
 */
import {
  EMPTY_LAYOUT,
  MIN_CHILD_PANE,
  activeTab,
  addTab,
  childPlacement,
  findLeaf,
  leaves,
  locateSession,
  moveToNewTab,
  moveToTab,
  normalizeLayout,
  placeChild,
  resolveMoveSource,
  roomToSplit,
  splitFor,
  tabsOf,
  type Layout,
  type Mint,
} from "./src/lib/layout-tree.ts";
import { makeProject } from "./src/lib/workspace-model.ts";

let failures = 0;
const check = (name: string, ok: boolean) => {
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}`);
  if (!ok) failures++;
};
let n = 0;
const mint: Mint = (p) => `${p}-${++n}`;

const S = "ssh\u0000box\u0000\u0000";
const P = makeProject(S, "/w");
const Q = makeProject(S, "/q");
const sess = (id: string) => ({ kind: "session" as const, id });
const ids = (L: Layout, tabId: string) =>
  leaves(tabsOf(L, P.id).tabs.find((t) => t.id === tabId)!.root).map((l) => (l.pane.kind === "session" ? l.pane.id : l.id));

// ── the overflow rule ────────────────────────────────────────────────────────
{
  const big = { w: 1600, h: 900 };
  const root = { kind: "leaf" as const, id: "L", pane: sess("x") };
  const s = splitFor(root, "L", big);
  check("splitFor: a wide pane splits side by side, halving the width", s.dir === "row" && s.half?.w === 800 && s.half.h === 900);
  check("roomToSplit: a 1600×900 pane has room", roomToSplit(root, "L", big));
  check("roomToSplit: a 900×400 pane does not (half is 450 wide, under 480)", !roomToSplit(root, "L", { w: 900, h: 400 }));
  check("roomToSplit: unknown size falls back to the pane count", roomToSplit(root, "L", null));
  check("MIN_CHILD_PANE is about 60 cols × 12 rows", MIN_CHILD_PANE.w >= 60 * 7.5 && MIN_CHILD_PANE.h >= 12 * 17);
}

{
  // The parent, its tab active and focused; another tab in front of the operator.
  let L: Layout = addTab(EMPTY_LAYOUT, P.id, sess("mom"), "mom", mint).layout;
  L = addTab(L, P.id, sess("other"), "other", mint).layout;
  const momTab = locateSession(L, "mom")!.tabId;
  const otherTab = locateSession(L, "other")!.tabId;
  const before = activeTab(L)!;
  const kids = new Set<string>();
  const opts = (area: { w: number; h: number } | null) => ({
    area,
    isSibling: (id: string) => kids.has(id),
    parentName: "mom",
  });
  const area = { w: 1600, h: 900 };

  const add = (id: string, a: { w: number; h: number } | null = area) => {
    const r = placeChild(L, P.id, "mom", sess(id), mint, opts(a));
    L = r.layout;
    kids.add(id);
    return r;
  };

  const k1 = add("k1");
  check("1st child: split beside the parent (800×900 half fits)", k1.placement.kind === "split" && k1.tabId === momTab && !k1.placement.overflow);
  const k2 = add("k2");
  check("2nd child: still beside (a 800×900 pane splits to 800×450)", k2.placement.kind === "split" && k2.tabId === momTab);
  const k3 = add("k3");
  // mom is now 800×450: a split leaves 400×450 — too narrow.
  check("3rd child: the parent's pane is too small → a new overflow tab", k3.placement.kind === "newTab" && k3.placement.overflow);
  const pt = tabsOf(L, P.id);
  const at = pt.tabs.findIndex((t) => t.id === k3.tabId);
  check("…placed right after the parent's tab", pt.tabs[at - 1]?.id === momTab);
  check("…named after the parent, ·2", pt.tabs[at]!.name === "mom ·2" && pt.tabs[at]!.renamed === true);
  check("…not activated: the active tab and its focus are unchanged",
    activeTab(L)!.tab.id === before.tab.id && activeTab(L)!.tab.focus === before.tab.focus && L.activeProject === before.projectId);
  const parentTab = pt.tabs.find((t) => t.id === momTab)!;
  check("…and the parent's tab is untouched", ids(L, momTab).sort().join() === "k1,k2,mom" && parentTab.focus === leaves(parentTab.root)[0]!.id);

  const k4 = add("k4");
  check("4th child: reuses the overflow tab (split, overflow)", k4.placement.kind === "split" && k4.placement.overflow && k4.tabId === k3.tabId);
  add("k5");
  add("k6");
  const k7 = add("k7");
  check("once the overflow tab is full too: a second overflow tab, ·3", k7.placement.kind === "newTab" && k7.placement.name === "mom ·3");
  const order = tabsOf(L, P.id).tabs.map((t) => t.id);
  check("…after the first overflow tab, before the unrelated one",
    order.indexOf(k7.tabId) === order.indexOf(k3.tabId) + 1 && order.indexOf(otherTab) > order.indexOf(k7.tabId));
  check("still nothing activated after seven children", activeTab(L)!.tab.id === before.tab.id);

  // The invariant holds: every session in exactly one leaf.
  const pool = {
    servers: [{ id: S }],
    projects: [P],
    sessions: ["mom", "other", ...kids].map((id) => ({ id, projectId: P.id, name: id })),
  };
  check("normalizeLayout has nothing to repair", normalizeLayout(L, pool, mint) === L);

  // A tab holding something that is not a sibling is not an overflow tab.
  kids.delete("k4");
  const k8 = childPlacement(L, P.id, "mom", { ...opts(area), childName: "k8" });
  check("a tab holding a non-sibling is not reused as overflow",
    !(k8.kind === "split" && k8.tabId === k3.tabId));
}

{
  // Unknown size: the fallback cap of 4 panes per tab.
  let L: Layout = addTab(EMPTY_LAYOUT, P.id, sess("mom"), "mom", mint).layout;
  const placements: string[] = [];
  for (const id of ["a", "b", "c", "d"]) {
    const r = placeChild(L, P.id, "mom", sess(id), mint, { area: null, parentName: "mom", isSibling: (x) => "abcd".includes(x) });
    L = r.layout;
    placements.push(r.placement.kind);
  }
  check("unsized: three splits, then the 5th pane goes to a tab", placements.join() === "split,split,split,newTab");
  // Parent in another project: a plain tab, as `placeBeside` did.
  const away = childPlacement(L, Q.id, "mom", { area: { w: 1600, h: 900 }, childName: "kid" });
  check("a parent outside the project → a plain (non-overflow) new tab", away.kind === "newTab" && !away.overflow && away.name === "kid");
}

// ── moving between tabs ──────────────────────────────────────────────────────
{
  let L: Layout = addTab(EMPTY_LAYOUT, P.id, sess("a"), "a", mint).layout;
  L = addTab(L, P.id, sess("b"), "b", mint).layout;
  const tabA = locateSession(L, "a")!.tabId;
  const tabB = locateSession(L, "b")!.tabId;
  // Put "c" beside "a" by moving it in from a tab of its own.
  L = addTab(L, P.id, sess("c"), "c", mint).layout;
  const cLeaf = locateSession(L, "c")!.leafId;
  const tabC = locateSession(L, "c")!.tabId;

  const m = moveToTab(L, { kind: "session", sessionId: "c" }, { projectId: P.id, tabId: tabA }, mint, { w: 1600, h: 900 })!;
  check("moveToTab: the session joins the target tab", !!m && ids(m.layout, tabA).join() === "a,c");
  check("…keeping its leaf id (no remount by id)", locateSession(m.layout, "c")!.leafId === cLeaf);
  check("…the tab it left (now empty) is removed", !tabsOf(m.layout, P.id).tabs.some((t) => t.id === tabC));
  check("…the target tab is active and focused on the moved pane",
    activeTab(m.layout)!.tab.id === tabA && activeTab(m.layout)!.tab.focus === cLeaf);
  check("…split along the long side (side by side in a wide pane)",
    (() => {
      const t = tabsOf(m.layout, P.id).tabs.find((x) => x.id === tabA)!;
      return t.root.kind === "split" && t.root.dir === "row";
    })());
  check("moveToTab into its own tab is a no-op", moveToTab(m.layout, { kind: "session", sessionId: "c" }, { projectId: P.id, tabId: tabA }, mint) === null);
  check("moveToTab to a tab that is gone is a no-op", moveToTab(m.layout, { kind: "session", sessionId: "c" }, { projectId: P.id, tabId: "nope" }, mint) === null);

  // Break "c" back out: a new tab right after the one it left.
  L = m.layout;
  const b = moveToNewTab(L, { kind: "session", sessionId: "c" }, P.id, mint)!;
  const order = tabsOf(b.layout, P.id).tabs.map((t) => t.id);
  check("moveToNewTab: a new tab holding just the pane", !!b && ids(b.layout, b.tabId).join() === "c");
  check("…inserted right after the tab it left", order.indexOf(b.tabId) === order.indexOf(tabA) + 1 && order.at(-1) === tabB);
  check("…same leaf id, and the old tab keeps the rest", locateSession(b.layout, "c")!.leafId === cLeaf && ids(b.layout, tabA).join() === "a");
  check("…the new tab is active", activeTab(b.layout)!.tab.id === b.tabId);
  check("…the tab it left is focused on what remains",
    (() => {
      const t = tabsOf(b.layout, P.id).tabs.find((x) => x.id === tabA)!;
      return !!findLeaf(t.root, t.focus);
    })());
  check("moveToNewTab on a pane already alone in its tab is a no-op",
    moveToNewTab(b.layout, { kind: "session", sessionId: "c" }, P.id, mint) === null);

  // A host pane has no session id: it moves by position.
  let H = b.layout;
  H = addTab(H, P.id, { kind: "host", serverId: S }, "host", mint).layout;
  const hostTab = tabsOf(H, P.id).tabs.at(-1)!;
  const hostLeaf = hostTab.root.id;
  const src = { kind: "leaf" as const, projectId: P.id, tabId: hostTab.id, leafId: hostLeaf };
  check("a leaf source resolves to that pane", resolveMoveSource(H, src)?.node.id === hostLeaf);
  check("a stale leaf source resolves to nothing", resolveMoveSource(H, { ...src, leafId: "gone" }) === null);
  const hm = moveToTab(H, src, { projectId: P.id, tabId: tabB }, mint)!;
  check("a host pane moves into a tab by position, same leaf id",
    !!hm && !!findLeaf(tabsOf(hm.layout, P.id).tabs.find((t) => t.id === tabB)!.root, hostLeaf));
  check("moving never drops a session", ["a", "b", "c"].every((id) => !!locateSession(hm.layout, id)));
}

console.log(failures ? `\n${failures} FAILED` : "\nALL CHECKS PASSED");
if (failures) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
