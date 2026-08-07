import {
  basename,
  migrateV2toV3,
  resolveWorkspace,
  makeServer,
  makeProject,
  serverId,
  projectId,
  reattachName,
  type PersistedV2,
} from "./src/lib/workspace-model.ts";

/**
 * The rules the v2 → v3 workspace migration must keep.
 *
 * Pure logic, so this is a plain assertion list. The one that actually matters —
 * and the reason migration exists at all rather than a fresh start — is
 * **reattach-name preservation**: if a migrated session's `claude-<id>` /
 * `term-<id>` name drifts, the live shell or conversation on the host is orphaned.
 * Everything else here is grouping/dedup that a glance at the rail would catch.
 */

const v2: PersistedV2 = {
  // Two sessions in the SAME folder on the SAME host → one Project, two Claude
  // sessions. A third on a different host. A fourth on the same host as #1 but a
  // different USER → a different Server (different authenticated connection).
  sessions: [
    { id: "s1", name: "api", target: { host: "prod" }, folder: "/home/me/api", resume: "conv-1", skipPermissions: true },
    { id: "s2", name: "api 2", target: { host: "prod" }, folder: "/home/me/api", modelProfile: "glm" },
    { id: "s3", name: "web", target: { host: "prod" }, folder: "/home/me/web", jiraProject: "RMX" },
    { id: "s4", name: "api as root", target: { host: "prod", user: "root" }, folder: "/home/me/api" },
    { id: "s5", name: "local scratch", target: {}, folder: "/tmp/scratch" },
  ],
  activeSession: "s2",
  terminals: [
    { id: "term-a", sessionId: "s1", title: "sh 1" },
    { id: "term-b", sessionId: "s1", title: "build" },
    { id: "term-c", sessionId: "s3", title: "sh 1" },
    { id: "term-orphan", sessionId: "gone", title: "orphan" }, // parent missing
  ],
  activeTerminal: { s1: "term-b" },
  openPaths: {
    s1: ["/home/me/api/src/main.rs", "/home/me/api/Cargo.toml"],
    s2: ["/home/me/api/Cargo.toml", "/home/me/api/README.md"], // shares s1's project
    s3: ["/home/me/web/index.html"],
  },
  activePath: { s1: "/home/me/api/src/main.rs", s3: "/home/me/web/index.html" },
  gridSlots: ["s1", null, "s3"],
};

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail && !ok ? ` — ${detail}` : ""}`);
    if (!ok) failures++;
  };

  const v3 = migrateV2toV3(v2);

  // ── The one that matters most: reattach names survive verbatim ────────────
  {
    const claude = v3.sessions.filter((s) => s.kind === "claude");
    const namesOk = ["s1", "s2", "s3", "s4", "s5"].every((id) => {
      const s = claude.find((x) => x.id === id);
      return s && reattachName(s) === `claude-${id}`;
    });
    check("every Claude session reattaches as claude-<oldId>", namesOk);

    const t = v3.sessions.find((x) => x.kind === "terminal" && x.id === "term-a");
    check("a terminal reattaches by its own id (term- prefix intact)", !!t && reattachName(t) === "term-a");
  }

  // ── Dedup: same (host, folder) → one Project, both Claude sessions ────────
  {
    const pid = projectId(serverId({ host: "prod" }), "/home/me/api");
    const inApi = v3.sessions.filter((s) => s.projectId === pid && s.kind === "claude");
    check("two sessions in one folder share a Project", inApi.length === 2, `got ${inApi.length}`);
    check("that Project exists once", v3.projects.filter((p) => p.id === pid).length === 1);
  }

  // ── Server identity: alias, user and port all participate ─────────────────
  {
    // prod (default user) and prod/root are different Servers.
    check("same host, different user = two Servers",
      serverId({ host: "prod" }) !== serverId({ host: "prod", user: "root" }));
    const prodServers = v3.servers.filter((s) => s.target.host === "prod");
    check("both prod Servers are present", prodServers.length === 2, `got ${prodServers.length}`);
    check("local target maps to the 'local' Server id", serverId({}) === "local");
    check("a local Server was created", v3.servers.some((s) => s.id === "local"));
  }

  // ── Terminals land under their parent's Project; orphans are dropped ──────
  {
    const apiPid = projectId(serverId({ host: "prod" }), "/home/me/api");
    const webPid = projectId(serverId({ host: "prod" }), "/home/me/web");
    const terms = v3.sessions.filter((s) => s.kind === "terminal");
    check("two terminals under the api Project",
      terms.filter((t) => t.projectId === apiPid).length === 2);
    check("one terminal under the web Project",
      terms.filter((t) => t.projectId === webPid).length === 1);
    check("the orphan terminal is dropped", !terms.some((t) => t.id === "term-orphan"));
    check("total sessions = 5 claude + 3 terminals", v3.sessions.length === 8, `got ${v3.sessions.length}`);
  }

  // ── Open files re-key to the Project and merge (dedup) ────────────────────
  {
    const apiPid = projectId(serverId({ host: "prod" }), "/home/me/api");
    const merged = v3.openPaths[apiPid] ?? [];
    check("s1+s2 open paths merge under one Project",
      merged.length === 3, `got ${merged.length}: ${JSON.stringify(merged)}`);
    check("the shared Cargo.toml is not duplicated",
      merged.filter((p) => p.endsWith("Cargo.toml")).length === 1);
    check("activePath resolves to the api Project", v3.activePath[apiPid] === "/home/me/api/src/main.rs");
  }

  // ── Grid layout: cells become session panes ───────────────────────────────
  {
    check("gridSlots become session panes",
      v3.panes.length === 3 &&
        v3.panes[0]?.kind === "session" &&
        (v3.panes[0] as { id: string }).id === "s1" &&
        v3.panes[1] === null &&
        v3.panes[2]?.kind === "session");
  }

  // ── Order: first-appearance is preserved (rail order survives upgrade) ────
  {
    // Servers appear in the order their first session does: prod, prod/root, local.
    const order = v3.servers.map((s) => s.id);
    check("Servers keep first-appearance order",
      order[0] === serverId({ host: "prod" }) &&
        order[1] === serverId({ host: "prod", user: "root" }) &&
        order[2] === "local",
      JSON.stringify(order));
    check("activeSession is carried through", v3.activeSession === "s2");
    check("version stamped", v3.version === 3);
  }

  // ── Empty input is safe (fresh install path) ──────────────────────────────
  {
    const empty = migrateV2toV3({ sessions: [], activeSession: null, terminals: [] });
    check("empty v2 → empty v3",
      empty.servers.length === 0 && empty.projects.length === 0 && empty.sessions.length === 0);
  }

  // ── resolveWorkspace: which blob wins ─────────────────────────────────────
  {
    const v2raw = JSON.stringify(v2);
    const v3raw = JSON.stringify({ version: 3, servers: [{ id: "local", target: {} }], projects: [], sessions: [], panes: [], activeSession: null, openPaths: {}, activePath: {} });

    check("a v3 blob wins over a v2 blob",
      resolveWorkspace(v3raw, v2raw).sessions.length === 0);
    check("no v3 blob → the v2 blob is migrated (not dropped)",
      resolveWorkspace(null, v2raw).sessions.length === 8);
    check("neither blob → empty workspace",
      resolveWorkspace(null, null).sessions.length === 0);
    check("a corrupt v3 blob falls back to migrating v2",
      resolveWorkspace("{not json", v2raw).sessions.length === 8);
    check("a partial v3 blob is spread over defaults (no blank app)",
      resolveWorkspace(JSON.stringify({ version: 3, servers: [{ id: "local", target: {} }] }), null).panes.length === 0);
  }

  // ── creation helpers dedup by derived id ──────────────────────────────────
  {
    check("makeServer is deterministic (re-add = same id)",
      makeServer({ host: "prod" }).id === makeServer({ host: "prod" }).id);
    const sid = serverId({ host: "prod" });
    check("makeProject dedups on the same folder",
      makeProject(sid, "/home/me/api").id === makeProject(sid, "/home/me/api").id);
    check("makeProject labels from the basename",
      makeProject(sid, "/home/me/api").label === "api");

    // A *local* project on Windows. Splitting on "/" alone left the label as
    // the whole path, in the pane header, the rail row and the deck tile.
    check("a windows drive path labels from its last component",
      basename("C:\\Users\\me\\proj") === "proj");
    check("a windows path with a trailing separator still labels",
      basename("C:\\Users\\me\\proj\\") === "proj");
    check("a UNC path labels from its last component",
      basename("\\\\build01\\share\\proj") === "proj");
    check("mixed separators resolve", basename("C:/Users/me/proj") === "proj");

    // The other half, and the reason backslash is not a separator everywhere:
    // a POSIX name may contain one, and most projects here are remote.
    check("a posix folder containing a backslash keeps it",
      basename("/home/me/we\\ird") === "we\\ird");
    check("a bare posix path is unchanged", basename("/home/me/api/") === "api");
  }

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}
