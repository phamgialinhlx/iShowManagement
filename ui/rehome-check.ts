/**
 * `rehomeChildren` — a child that the old folder rule put in a second project
 * (`~/arrow` → `/data/arrow`) moves back beside its parent, the emptied project
 * goes, and nothing else moves.
 *
 * The store imports without extensions, so it is bundled first:
 *
 *     pnpm exec vite build --ssr "$PWD/ui/rehome-check.ts" \
 *       --outDir "$PWD/node_modules/.cache/rehome-check" --emptyOutDir
 *     node node_modules/.cache/rehome-check/rehome-check.js
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
const { locateSession, normalizeLayout } = await import("./src/lib/layout-tree.ts");

let failures = 0;
const check = (name: string, ok: boolean) => {
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}`);
  if (!ok) failures++;
};

const S = "ssh\u0000box\u0000\u0000";
const home = makeProject(S, "/home/u/arrow");
const away = makeProject(S, "/data/arrow");
const other = makeProject(S, "/srv");
const sessions = [
  { id: "mom", projectId: home.id, kind: "claude" as const, name: "mother" },
  { id: "kid", projectId: away.id, kind: "claude" as const, name: "xss", hostName: "claude-k", parentId: "mom" },
  { id: "solo", projectId: other.id, kind: "terminal" as const, name: "sh" },
];
let n = 0;
const mint = (p: string) => `${p}-t${++n}`;
const pool = { servers: [{ id: S }], projects: [home, away, other], sessions };
const layout = normalizeLayout({ activeProject: away.id, projects: {} }, pool as never, mint as never);
useWorkspace.setState({ projects: [home, away, other], sessions, layout } as never);

const moved = useWorkspace.getState().rehomeChildren();
const st = useWorkspace.getState();
check("the misplaced child is moved", moved.length === 1 && moved[0] === "kid");
check("…into its parent's project", st.sessions.find((s) => s.id === "kid")?.projectId === home.id);
const kid = locateSession(st.layout, "kid");
const mom = locateSession(st.layout, "mom");
check("…as a split in the parent's tab", !!kid && !!mom && kid.projectId === home.id && kid.tabId === mom.tabId);
check("the emptied project is gone", !st.projects.some((p) => p.id === away.id) && !st.layout.projects[away.id]);
check("an unrelated project stays", st.projects.some((p) => p.id === other.id));
check("the active project is no longer the removed one", st.layout.activeProject === home.id);
check("running it again moves nothing", useWorkspace.getState().rehomeChildren().length === 0);

console.log(failures ? `\n${failures} FAILED` : "\nALL CHECKS PASSED");
if (failures) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
