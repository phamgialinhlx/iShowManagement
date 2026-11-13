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

// Moving is never closing.
st = useWorkspace.getState();
check("every session still exists after the moves", ["one", "two", "three", "remote"].every((id) => st.sessions.some((s) => s.id === id)));
check("every session is still placed", ["one", "two", "three", "remote"].every((id) => !!locateSession(st.layout, id)));
check("nothing that ends work was called", killed.length === 0);

console.log(failures ? `\n${failures} FAILED` : "\nALL CHECKS PASSED");
if (failures) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
