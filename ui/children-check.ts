/**
 * Child adoption (`src/lib/children.ts` + `placeBeside` in `layout-tree.ts`):
 * which spawned sub-sessions a workspace adopts, into which project, and where
 * their pane goes.
 *
 *     node --experimental-strip-types ui/children-check.ts
 *
 * Every failure here is silent in the app: a child mapped to the wrong parent
 * splits a stranger's pane; a child adopted twice mounts one shell in two
 * panes; a forgotten decision drags a pane the operator closed back three
 * seconds later; and a placement that moves the focus sends the rest of a
 * sentence into the wrong terminal.
 */
import {
  chooseProject,
  endedChildren,
  parseSeen,
  planChildren,
  pollInterval,
  sameFolder,
  sessionByDaemonName,
  type ChildPool,
  type HostChild,
} from "./src/lib/children.ts";
import {
  EMPTY_LAYOUT,
  activeTab,
  addTab,
  focusLeaf,
  leaves,
  locateSession,
  normalizeLayout,
  placeBeside,
  splitLeaf,
  tabsOf,
  toggleZoom,
  type Layout,
  type Mint,
} from "./src/lib/layout-tree.ts";
import { makeProject, projectId, type SessionV3 } from "./src/lib/workspace-model.ts";

const counterMint = (): Mint => {
  let n = 0;
  return (p) => `${p}-c${++n}`;
};

const S1 = "ssh\u0000box\u0000\u0000";
const S2 = "ssh\u0000other\u0000\u0000";
const P_W = makeProject(S1, "/w");
const P_X = makeProject(S1, "/w/x");
const P_OTHER = makeProject(S2, "/w");

const child = (over: Partial<HostChild>): HostChild => ({
  session: "claude-kid",
  parent: "claude-mom",
  kind: "claude",
  cwd: "/w",
  name: "lint",
  createdAt: 1,
  ...over,
});

function pool(sessions: SessionV3[], projects = [P_W, P_X, P_OTHER]): ChildPool & { sessions: SessionV3[] } {
  return { projects, sessions };
}

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail && !ok ? ` — ${detail}` : ""}`);
    if (!ok) failures++;
  };

  // A Claude session minted here (id "mom" → daemon "claude-mom") and an adopted
  // shell attached by a verbatim host name.
  const mom: SessionV3 = { id: "mom", projectId: P_W.id, kind: "claude", name: "mother" };
  const adopted: SessionV3 = { id: "term-z", projectId: P_W.id, kind: "terminal", name: "sh", hostName: "term-hostside" };
  // Same daemon name on another server — must never be confused with ours.
  const twin: SessionV3 = { id: "mom", projectId: P_OTHER.id, kind: "claude", name: "twin" };

  // ── parent mapping ─────────────────────────────────────────────────────────
  {
    const p = pool([mom, adopted]);
    check("parent maps by reattachName (claude-<id>)", sessionByDaemonName(p, S1, "claude-mom")?.id === "mom");
    check("parent maps by hostName, verbatim", sessionByDaemonName(p, S1, "term-hostside")?.id === "term-z");
    check("a derived name does not match an adopted session", sessionByDaemonName(p, S1, "term-z") === undefined);
    const plan = planChildren(S1, [child({ parent: "term-hostside", session: "pi-k", kind: "pi" })], p, []);
    check("a child of an adopted session is adopted under it", plan.adopt[0]?.parentId === "term-z");

    const cross = pool([twin]);
    const none = planChildren(S1, [child({})], cross, []);
    check("a same-named session on another server is not the parent", none.adopt.length === 0);

    const orphan = planChildren(S1, [child({ parent: "claude-nobody" })], p, []);
    check("an unknown parent adopts nothing", orphan.adopt.length === 0);
    check("…and is not marked seen, so it can follow its parent in later", !orphan.seen.includes("claude-kid"));
    const nullParent = planChildren(S1, [child({ parent: null })], p, []);
    check("a child with no parent adopts nothing", nullParent.adopt.length === 0);
  }

  // ── project choice ─────────────────────────────────────────────────────────
  {
    const p = pool([mom]);
    check("same folder as the parent → parent's project", chooseProject(p, S1, P_W.id, "/w").projectId === P_W.id);
    check("trailing slash is the same folder", chooseProject(p, S1, P_W.id, "/w/").projectId === P_W.id);
    check("no cwd → parent's project", chooseProject(p, S1, P_W.id, "").projectId === P_W.id);
    check(
      "another folder (a symlink's real path, a subdirectory) → still the parent's project",
      chooseProject(p, S1, P_W.id, "/data/w").projectId === P_W.id &&
        chooseProject(p, S1, P_W.id, "/w/x").projectId === P_W.id &&
        !chooseProject(p, S1, P_W.id, "/w/new").create,
    );
    // A parent in the adopted-sessions bucket: place the child by its folder.
    const P_RUN = { ...makeProject(S1, "(running)"), running: true };
    const runPool = pool([mom], [P_W, P_X, P_OTHER, P_RUN]);
    const sub = chooseProject(runPool, S1, P_RUN.id, "/w/x");
    check("bucket parent, folder with a project on this server → that project", sub.projectId === P_X.id && !sub.create);
    const fresh = chooseProject(runPool, S1, P_RUN.id, "/w/new");
    check(
      "bucket parent, unknown folder → a new project for it",
      fresh.projectId === projectId(S1, "/w/new") && fresh.create?.folder === "/w/new",
    );
    const elsewhere = chooseProject(pool([mom], [P_W, P_OTHER, P_RUN]), S1, P_RUN.id, "/w/x");
    check("a project for that folder on another server does not count", !!elsewhere.create);
    const plan = planChildren(
      S1,
      [child({ session: "claude-a", cwd: "/w/new" }), child({ session: "claude-b", cwd: "/w/new" })],
      pool([{ ...mom, projectId: P_RUN.id }], [P_W, P_X, P_OTHER, P_RUN]),
      [],
    );
    check(
      "two children in one new folder create it once",
      plan.adopt.length === 2 && !!plan.adopt[0]!.create && !plan.adopt[1]!.create &&
        plan.adopt[0]!.projectId === plan.adopt[1]!.projectId,
    );
    check("sameFolder keeps / as /", sameFolder("/", "/") && !sameFolder("/", "/w"));
  }

  // ── kind and name ──────────────────────────────────────────────────────────
  {
    const p = pool([mom]);
    const plan = planChildren(
      S1,
      [child({ session: "term-1", kind: "terminal", name: "" }), child({ session: "pi-2", kind: "pi", name: "review" })],
      p,
      [],
    );
    check("terminal kind is a terminal", plan.adopt[0]?.kind === "terminal");
    check("an empty name falls back to the session name", plan.adopt[0]?.name === "term-1");
    check("pi kind and name carried", plan.adopt[1]?.kind === "pi" && plan.adopt[1]?.name === "review");
  }

  // ── no double adoption, seen-set suppression, pruning ─────────────────────
  {
    const p = pool([mom]);
    const first = planChildren(S1, [child({})], p, []);
    check("a new child is adopted once", first.adopt.length === 1 && first.seen.includes("claude-kid"));
    const again = planChildren(S1, [child({})], p, first.seen);
    check("the next poll adopts nothing (seen)", again.adopt.length === 0);

    // The operator closed/detached it: the row is gone, the decision stays.
    const afterClose = planChildren(S1, [child({})], p, again.seen);
    check("a closed or detached child is not re-adopted", afterClose.adopt.length === 0);

    // Already in the workspace by hand (HOST ▸ SESSIONS), seen list lost.
    const byHand: SessionV3 = { id: "x", projectId: P_W.id, kind: "claude", name: "k", hostName: "claude-kid" };
    const present = planChildren(S1, [child({})], pool([mom, byHand]), []);
    check("a child already in the workspace is not adopted again", present.adopt.length === 0);
    check("…but its decision is recorded", present.seen.includes("claude-kid"));

    const dup = planChildren(S1, [child({}), child({ name: "again" })], p, []);
    check("a duplicate row adopts once", dup.adopt.length === 1);

    const pruned = planChildren(S1, [child({ session: "claude-new" })], p, ["claude-gone", "claude-kid"]);
    check("seen is pruned of names the host no longer lists", !pruned.seen.includes("claude-gone") && !pruned.seen.includes("claude-kid"));
    check("…and extended by this poll's decisions", pruned.seen.includes("claude-new"));
  }

  // ── malformed input ────────────────────────────────────────────────────────
  {
    const p = pool([mom]);
    const junk = [
      null,
      7,
      { kind: "claude" },
      { session: "", kind: "claude", parent: "claude-mom" },
      { session: "x-1", kind: "browser", parent: "claude-mom" },
      { session: "claude-ok", kind: "claude", parent: "claude-mom" },
    ] as unknown as HostChild[];
    const plan = planChildren(S1, junk, p, []);
    check("malformed rows are skipped, good ones kept", plan.adopt.length === 1 && plan.adopt[0]!.child.session === "claude-ok");
    check("…a row with no cwd/name still has a name", plan.adopt[0]?.name === "claude-ok");
    check("a non-array list is no children", planChildren(S1, "nope" as unknown as HostChild[], p, []).adopt.length === 0);
    check("parseSeen tolerates junk", Object.keys(parseSeen("x")).length === 0 && Object.keys(parseSeen([1])).length === 0);
    const seen = parseSeen({ a: ["x", 3, "y"], b: "z" });
    check("parseSeen keeps string lists only", seen.a?.join() === "x,y" && seen.b === undefined);
  }

  // ── placement ──────────────────────────────────────────────────────────────
  {
    const mint = counterMint();
    let L: Layout = addTab(EMPTY_LAYOUT, P_W.id, { kind: "session", id: "mom" }, "mother", mint).layout;
    L = addTab(L, P_W.id, { kind: "session", id: "other" }, "other", mint).layout; // the active tab
    const before = activeTab(L)!;
    const momLoc = locateSession(L, "mom")!;
    const momTabBefore = tabsOf(L, P_W.id).tabs.find((t) => t.id === momLoc.tabId)!;

    const wide = placeBeside(L, P_W.id, "mom", { kind: "session", id: "kid" }, mint, 16 / 9);
    check("parent in the same project → split", wide.placed === "split" && wide.tabId === momLoc.tabId);
    check("a wide pane splits right (row)", wide.dir === "row");
    const momTab = tabsOf(wide.layout, P_W.id).tabs.find((t) => t.id === momLoc.tabId)!;
    check("the child is beside the parent", leaves(momTab.root).map((l) => l.pane.kind === "session" && l.pane.id).join() === "mom,kid");
    check("the parent tab's focus is unchanged", momTab.focus === momTabBefore.focus);
    const after = activeTab(wide.layout)!;
    check(
      "the active project and tab are unchanged (no focus moves)",
      wide.layout.activeProject === L.activeProject && after.tab.id === before.tab.id && after.tab.focus === before.tab.focus,
    );

    // A tall pane: split the parent's tab right first, then the half is 0.5 × 1.
    const tall = placeBeside(wide.layout, P_W.id, "kid", { kind: "session", id: "grandkid" }, mint, 1);
    check("a tall pane splits down (col)", tall.dir === "col");
    const wideHalf = placeBeside(wide.layout, P_W.id, "kid", { kind: "session", id: "gk" }, mint, 3);
    check("…but the same half in a very wide window splits right", wideHalf.dir === "row");

    const otherProject = placeBeside(L, P_X.id, "mom", { kind: "session", id: "kid" }, mint);
    check("parent in another project → a new tab", otherProject.placed === "tab");
    check(
      "…not activated: active project and its tab unchanged",
      otherProject.layout.activeProject === L.activeProject &&
        tabsOf(otherProject.layout, P_W.id).activeTab === tabsOf(L, P_W.id).activeTab,
    );
    const noParent = placeBeside(L, P_W.id, null, { kind: "session", id: "kid" }, mint);
    check("no parent → a new tab, not activated", noParent.placed === "tab" && activeTab(noParent.layout)!.tab.id === before.tab.id);

    // Zoomed parent tab: still split, zoom and focus kept.
    let Z = focusLeaf(L, P_W.id, momLoc.tabId, momLoc.leafId);
    Z = { ...Z, projects: { ...Z.projects } };
    const zTab = tabsOf(Z, P_W.id).tabs.find((t) => t.id === momLoc.tabId)!;
    const zTwo = splitLeaf(zTab.root, momLoc.leafId, "row", { kind: "leaf", id: "leaf-x", pane: { kind: "session", id: "sib" } }, "split-x");
    Z = { ...Z, projects: { ...Z.projects, [P_W.id]: { ...tabsOf(Z, P_W.id), tabs: tabsOf(Z, P_W.id).tabs.map((t) => (t.id === zTab.id ? { ...t, root: zTwo } : t)) } } };
    Z = toggleZoom(Z);
    const zoomedPlace = placeBeside(Z, P_W.id, "mom", { kind: "session", id: "kid" }, mint);
    const zt = tabsOf(zoomedPlace.layout, P_W.id).tabs.find((t) => t.id === momLoc.tabId)!;
    check("placing into a zoomed tab keeps its zoom and focus", zt.zoomed === true && zt.focus === momLoc.leafId);

    // The invariant holds once the store's pool includes the child.
    const sessions = [
      { id: "mom", projectId: P_W.id, name: "mother" },
      { id: "other", projectId: P_W.id, name: "other" },
      { id: "kid", projectId: P_W.id, name: "kid" },
    ];
    const N = normalizeLayout(wide.layout, { servers: [{ id: S1 }], projects: [P_W, P_X], sessions }, mint);
    check("normalizeLayout accepts the placement unchanged (one leaf per session)", N === wide.layout);
    const kidLeaves = Object.values(N.projects).flatMap((pt) => pt.tabs.flatMap((t) => leaves(t.root)))
      .filter((l) => l.pane.kind === "session" && l.pane.id === "kid");
    check("the child is mounted exactly once", kidLeaves.length === 1);
  }

  // ── ended children ────────────────────────────────────────────────────────
  {
    const kid: SessionV3 = { id: "k", projectId: P_W.id, kind: "claude", name: "k", hostName: "claude-kid", parentId: "mom" };
    const plainAdopted: SessionV3 = { id: "a", projectId: P_W.id, kind: "terminal", name: "a", hostName: "term-a" };
    const p = pool([mom, kid, plainAdopted]);
    check("a listed child has not ended", endedChildren(S1, [child({})], p).length === 0);
    const gone = endedChildren(S1, [], p);
    check("an adopted child missing from the list has ended", gone.join() === "k");
    check("a session that is not a child is never reported ended", !gone.includes("a"));
    check("another server's list says nothing about this one", endedChildren(S2, [], p).length === 0);
  }

  // ── cadence ───────────────────────────────────────────────────────────────
  check("busy servers poll every 3s, idle every 10s", pollInterval(true) === 3000 && pollInterval(false) === 10000);

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}

if (typeof window === "undefined") {
  const ok = run((line) => console.log(line));
  if (!ok) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
}
