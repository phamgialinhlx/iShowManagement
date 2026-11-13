/**
 * Connect a server — port, key choice, and the server's identity.
 *
 *     node --experimental-strip-types ui/connect-check.ts
 *
 * Every failure here is silent in the app: a port that became `NaN`, a
 * `~/.ssh/config` HostName that quietly became the connection target, or a key
 * choice that re-keyed the server and orphaned its projects and sessions.
 */
import { formTarget, keyFileNotes, parsePort, type ConnectForm } from "./src/lib/connect-form.ts";
import { serverId } from "./src/lib/workspace-model.ts";
import { addProject, addServer, setServerIdentity, type Core } from "./src/lib/workspace-reducers.ts";

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    if (!ok) failures += 1;
    log(`${ok ? "PASS" : "FAIL"}  ${name}${ok || !detail ? "" : `  — ${detail}`}`);
  };
  const form = (patch: Partial<ConnectForm>): ConnectForm => ({
    host: "devbox",
    user: "",
    port: "",
    key: "default",
    keyPath: "",
    ...patch,
  });

  // ── port ──────────────────────────────────────────────────────────────
  check("empty port is ssh's default, not an error", JSON.stringify(parsePort("")) === "{}");
  check("22 parses", parsePort(" 22 ").port === 22);
  check("65535 parses", parsePort("65535").port === 65535);
  for (const bad of ["0", "65536", "-1", "22a", "2.2", "abc", "1e3"]) {
    check(`port ${JSON.stringify(bad)} is refused`, !!parsePort(bad).error, JSON.stringify(parsePort(bad)));
  }
  check("a bad port blocks the target", !formTarget(form({ port: "99999" })).target);

  // ── the target ────────────────────────────────────────────────────────
  const plain = formTarget(form({}));
  check("an alias goes verbatim, with nothing else", JSON.stringify(plain.target) === '{"host":"devbox"}', JSON.stringify(plain));
  const full = formTarget(form({ host: " 10.0.0.107 ", user: "dev", port: "2222" })).target;
  check(
    "host, user and port are carried",
    full?.host === "10.0.0.107" && full.user === "dev" && full.port === 2222,
    JSON.stringify(full),
  );
  check("no host is no target", !formTarget(form({ host: "  " })).target);
  check("a host with a space is refused", !formTarget(form({ host: "a b" })).target);
  check("DEFAULT carries no identity", formTarget(form({ keyPath: "/k" })).target?.identity === undefined);
  check("GENERATE carries no identity", formTarget(form({ key: "generate", keyPath: "/k" })).target?.identity === undefined);
  check(
    "KEY FILE carries the path",
    formTarget(form({ key: "file", keyPath: "/Users/me/.ssh/work" })).target?.identity === "/Users/me/.ssh/work",
  );
  check("KEY FILE with nothing chosen is refused", !formTarget(form({ key: "file" })).target);

  // ── identity is not part of the server's id ─────────────────────────────
  const t = { host: "10.0.0.107", user: "dev", port: 22 };
  check(
    "serverId ignores the chosen key",
    serverId(t) === serverId({ ...t, identity: "/Users/me/.ssh/rmux_dev_10.0.0.107_ed25519" }),
  );
  check("serverId still distinguishes ports", serverId(t) !== serverId({ ...t, port: 2222 }));

  let ws: Core = { servers: [], projects: [], sessions: [], activeSession: null } as unknown as Core;
  const added = addServer(ws, t);
  ws = added.ws;
  ws = addProject(ws, added.id, "/srv/app").ws;
  ws = setServerIdentity(ws, added.id, "/Users/me/.ssh/work");
  check("setting a key keeps the server's id", ws.servers[0]?.id === added.id);
  check("…and stores the path", ws.servers[0]?.target.identity === "/Users/me/.ssh/work");
  check("…and its projects still point at it", ws.projects[0]?.serverId === ws.servers[0]?.id);
  ws = setServerIdentity(ws, added.id, undefined);
  check("DEFAULT removes the field rather than storing undefined", !("identity" in ws.servers[0]!.target));
  const local = addServer(ws, {});
  check("the local machine never takes a key", !("identity" in setServerIdentity(local.ws, local.id, "/k").servers.find((s) => s.id === "local")!.target));

  // ── what the dialog says about a picked file ────────────────────────────
  const base = { path: "/k", exists: true, readable: true, isPublic: false, privateSibling: null, mode: 0o600, tooOpen: false };
  check("a good private key says nothing", keyFileNotes(base).length === 0);
  check("a missing file blocks", keyFileNotes({ ...base, exists: false })[0]?.blocking === true);
  check("an unreadable file blocks", keyFileNotes({ ...base, readable: false })[0]?.blocking === true);
  const pub = keyFileNotes({ ...base, path: "/k.pub", isPublic: true, privateSibling: "/k" });
  check("a .pub blocks and offers the private key", pub[0]?.blocking === true && pub[0]?.fix?.path === "/k", JSON.stringify(pub));
  const open = keyFileNotes({ ...base, mode: 0o644, tooOpen: true });
  check("a 0644 key warns without blocking", open.length === 1 && !open[0]!.blocking && open[0]!.text.includes("0644"), JSON.stringify(open));

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}

if (typeof window === "undefined") {
  const ok = run((line) => console.log(line));
  if (!ok) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
}
