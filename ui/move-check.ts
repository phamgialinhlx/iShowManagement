/**
 * `moveBeside` in the store — drag-to-split. A session dropped on a pane in
 * another project of the same server is re-homed there in the same write as
 * the layout; one on another server is refused; and **moving is never
 * closing**: every session still exists afterwards and nothing that ends work
 * is called.
 *
 * The store imports without extensions, so it is bundled first:
 *
 *     pnpm exec vite build --ssr "$PWD/ui/move-check.ts" \
 *       --outDir "$PWD/node_modules/.cache/move-check" --emptyOutDir
 *     node node_modules/.cache/move-check/move-check.js
 */
export {};

const mem = new Map<string, string>();
(globalThis as Record<string, unknown>).localStorage = {
  getItem: (k: string) => mem.get(k) ?? null,
  setItem: (k: string, v: string) => void mem.set(k, v),
  removeItem: (k: string) => void mem.delete(k),
};
const { useWorkspace } = await import("./src/lib/workspace.ts");
const { makeProject } = await import("./src/lib/workspace-model.ts");
const { locateSession, normalizeLayout, tabsOf } = await import("./src/lib/layout-tree.ts");

let failures = 0;
const check = (name: string, ok: boolean) => {
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}`);
  if (!ok) failures++;
};

const S = "ssh\u0000box\u0000\u0000";
const T = "ssh\u0000other\u0000\u0000";
const a = makeProject(S, "/home/u/a");
const b = makeProject(S, "/home/u/b");
const far = makeProject(T, "/srv");
const sessions = [
  { id: "one", projectId: a.id, kind: "claude" as const, name: "one" },
  { id: "two", projectId: a.id, kind: "terminal" as const, name: "two" },
  { id: "three", projectId: b.id, kind: "terminal" as const, name: "three" },
  { id: "remote", projectId: far.id, kind: "terminal" as const, name: "remote" },
];
let n = 0;
const mint = (p: string) => `${p}-t${++n}`;
const servers = [{ id: S }, { id: T }];
const pool = { servers, projects: [a, b, far], sessions };
const layout = normalizeLayout({ activeProject: a.id, projects: {} }, pool as never, mint as never);

// Anything that ends work is a spy: a move must not call it.
const killed: string[] = [];
useWorkspace.setState({
  servers: servers as never,
  projects: [a, b, far],
  sessions,
  layout,
  removeSession: (id: string) => void killed.push(`removeSession:${id}`),
  closeTab: (_p: string, id: string) => void killed.push(`closeTab:${id}`),
  closeLeaf: (_p: string, _t: string, id: string) => void killed.push(`closeLeaf:${id}`),
} as never);

const where = (id: string) => locateSession(useWorkspace.getState().layout, id)!;

// Same project: "two" beside "one".
const one = where("one");
check("same project: the move is accepted", useWorkspace.getState().moveBeside({ kind: "session", sessionId: "two" }, one, "right"));
let st = useWorkspace.getState();
check("same project: both now share one tab", where("two").tabId === where("one").tabId);
check("same project: the dropped session is focused and active", st.activeSession === "two");

// Cross project, same server: "three" (project b) onto "one" (project a).
check("cross project: the move is accepted",
  useWorkspace.getState().moveBeside({ kind: "session", sessionId: "three" }, where("one"), "down"));
st = useWorkspace.getState();
check("cross project: the session is re-homed into the target project", st.sessions.find((s) => s.id === "three")?.projectId === a.id);
check("cross project: and placed in the target's tab", where("three").projectId === a.id && where("three").tabId === where("one").tabId);
check("cross project: the source project is kept, though empty", st.projects.some((p) => p.id === b.id) && tabsOf(st.layout, b.id).tabs.length === 0);
check("cross project: normalize has nothing to repair", normalizeLayout(st.layout, st, mint as never) === st.layout);

// Another server: refused, nothing changes.
const before = useWorkspace.getState().layout;
check("another server: refusal says why", useWorkspace.getState().moveRefusal({ kind: "session", sessionId: "remote" }, where("one")) === "server");
check("another server: the move is refused",
  !useWorkspace.getState().moveBeside({ kind: "session", sessionId: "remote" }, where("one"), "left"));
check("another server: the layout is untouched", useWorkspace.getState().layout === before);
check("another server: the session stays in its own project", useWorkspace.getState().sessions.find((s) => s.id === "remote")?.projectId === far.id);

// Self-drop: a no-op.
check("self drop: reported as a no-op", useWorkspace.getState().moveRefusal({ kind: "session", sessionId: "one" }, where("one")) === "noop");
check("self drop: nothing moves", !useWorkspace.getState().moveBeside({ kind: "session", sessionId: "one" }, where("one"), "left"));

// Between tabs: break "two" out into a new tab, then move it back into one's tab.
{
  const from = where("two").tabId;
  const leaf = where("two").leafId;
  check("break out: accepted", useWorkspace.getState().moveToNewTab({ kind: "session", sessionId: "two" }, a.id));
  const st2 = useWorkspace.getState();
  check("break out: a tab of its own, same leaf id", where("two").tabId !== from && where("two").leafId === leaf);
  check("break out: the new tab is active and the session focused",
    tabsOf(st2.layout, a.id).activeTab === where("two").tabId && st2.activeSession === "two");
  check("break out again: refused as a no-op",
    useWorkspace.getState().tabMoveRefusal({ kind: "session", sessionId: "two" }, a.id, null) === "noop");
  const own = where("two").tabId;
  check("into a tab: accepted", useWorkspace.getState().moveToTab({ kind: "session", sessionId: "two" }, a.id, where("one").tabId));
  check("into a tab: shares one's tab, same leaf id", where("two").tabId === where("one").tabId && where("two").leafId === leaf);
  check("into a tab: the tab it emptied is gone", !tabsOf(useWorkspace.getState().layout, a.id).tabs.some((t) => t.id === own));
  check("into another server's tab: refused",
    useWorkspace.getState().tabMoveRefusal({ kind: "session", sessionId: "remote" }, a.id, where("one").tabId) === "server" &&
      !useWorkspace.getState().moveToTab({ kind: "session", sessionId: "remote" }, a.id, where("one").tabId) &&
      !useWorkspace.getState().moveToNewTab({ kind: "session", sessionId: "remote" }, a.id));
  // Cross project, same server, into a new tab: re-homed in the same write.
  useWorkspace.getState().moveToNewTab({ kind: "session", sessionId: "three" }, b.id);
  const st3 = useWorkspace.getState();
  check("new tab in another project: re-homed there",
    st3.sessions.find((s) => s.id === "three")?.projectId === b.id && where("three").projectId === b.id);
  check("new tab in another project: normalize has nothing to repair", normalizeLayout(st3.layout, st3, mint as never) === st3.layout);
}

// Moving is never closing.
st = useWorkspace.getState();
check("every session still exists after the moves", ["one", "two", "three", "remote"].every((id) => st.sessions.some((s) => s.id === id)));
check("every session is still placed", ["one", "two", "three", "remote"].every((id) => !!locateSession(st.layout, id)));
check("nothing that ends work was called", killed.length === 0);

// Adopting spawned children: beside the parent while a usable pane fits, then
// an overflow tab — and never the focus.
{
  const ws = useWorkspace.getState();
  ws.focusLeaf(where("one").projectId, where("one").tabId, where("one").leafId, { keyboard: false });
  const parent = ws.sessions.find((x) => x.id === "one")!;
  const before = useWorkspace.getState();
  const tabBefore = tabsOf(before.layout, a.id).activeTab;
  const focusBefore = tabsOf(before.layout, a.id).tabs.find((t) => t.id === tabBefore)!.focus;
  const parentTab = where("one").tabId;
  const adopted: string[] = [];
  for (let i = 1; i <= 4; i++) {
    const id = useWorkspace.getState().adoptChild(
      S,
      {
        child: { session: `claude-kid${i}`, parent: "claude-one", kind: "claude", cwd: "/home/u/a", name: `kid${i}`, createdAt: 0 },
        kind: "claude",
        parentId: parent.id,
        projectId: a.id,
        name: `kid${i}`,
      },
      16 / 9,
      { w: 1200, h: 700 },
    );
    if (id) adopted.push(id);
  }
  const st4 = useWorkspace.getState();
  check("adopt: four children adopted", adopted.length === 4);
  const tabsOfKids = new Set(adopted.map((id) => where(id).tabId));
  check("adopt: the first goes beside the parent", where(adopted[0]!).tabId === parentTab);
  check("adopt: later ones overflow into a tab of their own", [...tabsOfKids].some((t) => t !== parentTab));
  const overflow = tabsOf(st4.layout, a.id).tabs.find((t) => t.id !== parentTab && tabsOfKids.has(t.id))!;
  check("adopt: the overflow tab is named after the parent", /·2$/.test(overflow.name));
  check("adopt: the active tab and its focus never moved",
    tabsOf(st4.layout, a.id).activeTab === tabBefore &&
      tabsOf(st4.layout, a.id).tabs.find((t) => t.id === tabBefore)!.focus === focusBefore &&
      st4.layout.activeProject === before.layout.activeProject);
  check("adopt: the active session is unchanged", st4.activeSession === before.activeSession);
  check("adopt: normalize has nothing to repair", normalizeLayout(st4.layout, st4, mint as never) === st4.layout);
}

console.log(failures ? `\n${failures} FAILED` : "\nALL CHECKS PASSED");
if (failures) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
