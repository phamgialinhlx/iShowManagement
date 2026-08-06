import {
  EMPTY_CORE,
  addServer,
  addProject,
  addSession,
  removeSession,
  assignPane,
  closePane,
  type Core,
} from "./src/lib/workspace-reducers.ts";
import { serverId } from "./src/lib/workspace-model.ts";

/**
 * The invariants the workspace store's pure transitions must keep. The Zustand
 * store is thin glue over these, so proving them here is proving the store's
 * logic without booting it.
 */

/** Build a small workspace: one server, one project, two sessions (1 claude, 1 term). */
function seed(): { ws: Core; sid: string; pid: string } {
  let ws = EMPTY_CORE;
  const srv = addServer(ws, { host: "prod" });
  ws = srv.ws;
  const prj = addProject(ws, srv.id, "/home/me/api");
  ws = prj.ws;
  ws = addSession(ws, { id: "claude-1", projectId: prj.id, kind: "claude", name: "api" });
  ws = addSession(ws, { id: "term-1", projectId: prj.id, kind: "terminal", name: "sh 1" });
  return { ws, sid: srv.id, pid: prj.id };
}

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail && !ok ? ` — ${detail}` : ""}`);
    if (!ok) failures++;
  };

  // ── Create dedups by derived id ───────────────────────────────────────────
  {
    const { ws, sid, pid } = seed();
    check("one server, one project, two sessions", ws.servers.length === 1 && ws.projects.length === 1 && ws.sessions.length === 2);

    const again = addServer(ws, { host: "prod" });
    check("re-adding the same server is a no-op", again.ws.servers.length === 1 && again.id === sid);
    const reproj = addProject(again.ws, sid, "/home/me/api");
    check("re-adding the same project is a no-op", reproj.ws.projects.length === 1 && reproj.id === pid);

    // A different user is a different server.
    const root = addServer(ws, { host: "prod", user: "root" });
    check("different user forks a new server", root.ws.servers.length === 2 && root.id === serverId({ host: "prod", user: "root" }));

    check("adding a session makes it active", ws.activeSession === "term-1");
  }

  // ── Removing a session: structure survives, panes clear, active hands off ──
  {
    let { ws } = seed();
    ws = assignPane(ws, 0, { kind: "session", id: "claude-1" });
    ws = assignPane(ws, 1, { kind: "session", id: "term-1" });
    ws = removeSession(ws, "term-1");

    check("the session is gone", !ws.sessions.some((s) => s.id === "term-1"));
    check("its Project stays (empty projects persist)", ws.projects.length === 1);
    check("its pane is cleared to null", ws.panes[1] === null);
    check("the other pane is untouched", ws.panes[0]?.kind === "session" && (ws.panes[0] as { id: string }).id === "claude-1");
    check("active hands off to the survivor", ws.activeSession === "claude-1");
    check("removing an unknown id is a no-op", removeSession(ws, "nope") === ws);
  }

  // ── Pane placement: a session lives in exactly one tile ───────────────────
  {
    let { ws } = seed();
    ws = assignPane(ws, 0, { kind: "session", id: "claude-1" });
    ws = assignPane(ws, 2, { kind: "session", id: "claude-1" }); // moved, not duplicated
    const held = ws.panes.filter((p) => p && p.kind === "session" && p.id === "claude-1").length;
    check("a session occupies exactly one tile", held === 1, `held in ${held}`);
    check("the tile grew to index 2", ws.panes.length === 3 && ws.panes[0] === null && ws.panes[2]?.kind === "session");

    // Host/files panes may repeat.
    ws = assignPane(ws, 0, { kind: "host", serverId: "local" });
    ws = assignPane(ws, 1, { kind: "host", serverId: "local" });
    check("host panes may repeat across tiles",
      ws.panes.filter((p) => p?.kind === "host").length === 2);
  }

  // ── Closing a pane frees the tile without touching the session ────────────
  {
    let { ws } = seed();
    ws = assignPane(ws, 0, { kind: "session", id: "claude-1" });
    ws = closePane(ws, 0);
    check("close clears the tile", ws.panes[0] === null);
    check("the session still exists after closing its pane", ws.sessions.some((s) => s.id === "claude-1"));
    check("closing an out-of-range tile is a no-op", closePane(ws, 99) === ws);
  }

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}
