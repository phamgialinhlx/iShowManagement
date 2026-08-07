import {
  EMPTY_CORE,
  addServer,
  addProject,
  addSession,
  removeSession,
  removeProjectCore,
  removeServerCore,
  detachSessionCore,
  assignPane,
  closePane,
  type Core,
} from "./src/lib/workspace-reducers.ts";
import { isClaudeSession, reattachName, serverId } from "./src/lib/workspace-model.ts";

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

  // ── Detaching a session: row disappears, process stays on the server ────────
  // Structurally identical to removal (the rail row must vanish — detach means
  // "put it back in the background"), but the store's IO half never sends the
  // agent kill, so the daemon keeps the shell for the import picker to restore.
  {
    let { ws } = seed();
    ws = assignPane(ws, 0, { kind: "session", id: "claude-1" });
    ws = assignPane(ws, 1, { kind: "session", id: "term-1" });
    ws = detachSessionCore(ws, "claude-1");

    check("detach removes the session row", !ws.sessions.some((s) => s.id === "claude-1"));
    check("detach clears the session's panes", ws.panes[0] === null);
    check("detach leaves other panes alone", ws.panes[1]?.kind === "session");
    check("activeSession hands off to the survivor", ws.activeSession === "term-1");
    check("detaching an unknown id is a no-op", detachSessionCore(ws, "nope") === ws);
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

  // ── Removing a project: it goes, and files panes pointing at it clear ─────
  {
    let { ws, pid } = seed();
    // A second project on the same server, with its own files pane.
    const second = addProject(ws, serverId({ host: "prod" }), "/home/me/web");
    ws = second.ws;
    ws = assignPane(ws, 0, { kind: "files", projectId: pid });
    ws = assignPane(ws, 1, { kind: "files", projectId: second.id });
    ws = assignPane(ws, 2, { kind: "host", serverId: serverId({ host: "prod" }) });

    const removed = removeProjectCore(ws, pid);
    check("the project is gone", !removed.projects.some((p) => p.id === pid));
    check("the other project stays", removed.projects.some((p) => p.id === second.id));
    check("its files pane is cleared to null", removed.panes[0] === null);
    check("the other project's files pane is untouched",
      removed.panes[1]?.kind === "files" && (removed.panes[1] as { projectId: string }).projectId === second.id);
    // The server may still hold other projects, so its host pane survives.
    check("a host pane for the project's server is kept", removed.panes[2]?.kind === "host");
    check("sessions under it are untouched (the store cascades kills separately)",
      removed.sessions.length === 2);
    check("removing an unknown project is a no-op", removeProjectCore(ws, "nope") === ws);
  }

  // ── Removing a server: it goes, and host panes pointing at it clear ───────
  {
    let { ws, sid, pid } = seed();
    ws = assignPane(ws, 0, { kind: "files", projectId: pid });
    ws = assignPane(ws, 1, { kind: "host", serverId: sid });
    // A second server, with its own host pane, to prove only this one clears.
    const other = addServer(ws, { host: "staging" });
    ws = other.ws;
    ws = assignPane(ws, 2, { kind: "host", serverId: other.id });

    const removed = removeServerCore(ws, sid);
    check("the server is gone", !removed.servers.some((s) => s.id === sid));
    check("the other server stays", removed.servers.some((s) => s.id === other.id));
    check("its host pane is cleared to null", removed.panes[1] === null);
    check("the other server's host pane is untouched",
      removed.panes[2]?.kind === "host" && (removed.panes[2] as { serverId: string }).serverId === other.id);
    // `removeProjectCore` is applied by the store's cascade, not by this reducer.
    check("projects stay (the store cascades removeProject first)",
      removed.projects.some((p) => p.id === pid));
    check("removing an unknown server is a no-op", removeServerCore(ws, "nope") === ws);
  }

  // ── Session-kind detection off the daemon's `command`, not the name ────────
  {
    check("a claude login command is Claude", isClaudeSession("claude --resume abc"));
    check("a bare claude command is Claude", isClaudeSession("claude"));
    check("a shell is not Claude", !isClaudeSession("bash"));
    check("an absent command is not Claude", !isClaudeSession(undefined));
    check("a null command is not Claude", !isClaudeSession(null));
  }

  // ── Reattach names: the daemon key a Session attaches by ───────────────────
  {
    const term = { id: "term-1", kind: "terminal" as const };
    const claude = { id: "session-x", kind: "claude" as const };
    check("a terminal reattaches verbatim", reattachName(term) === "term-1");
    check("a claude session reattaches with the claude- prefix",
      reattachName(claude) === "claude-session-x");
  }

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}
