/**
 * A fake rmux-web for the mobile bundle's dev server — **dev and checks only**.
 *
 * Enabled by `RMUX_MOBILE_FIXTURE=1` (`pnpm dev:mobile:fixture`), it answers the
 * same wire protocol `crates/rmux-web` speaks (`docs/rmux-web.md` › "Wire
 * protocol (web)"): `GET /api/seed`, `POST /api/invoke/<cmd>` and the `/api/ws`
 * stream with its `hello`, events and channel frames. So the real `bridge.ts`,
 * status watch, transcript reader and terminal attach all run unmodified, against
 * canned data, with no host, no Claude and nothing sent anywhere.
 *
 * It exists so renderlint (`renderlint.mobile.config.mjs`) and screenshots are
 * deterministic. It is never part of a build: `vite build` does not run
 * `configureServer`, and nothing in `ui/src` imports this file.
 *
 * Every name, folder and URL in here is invented. Do not put a real host in it —
 * this repository is public.
 */
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const SEP = "\u0000";
const LOCAL = { id: "local", target: {} };
const DEVBOX = { id: ["ssh", "devbox", "dev", ""].join(SEP), target: { host: "devbox", user: "dev" } };

const projectId = (server, folder) => [server.id, folder].join(SEP);
const P_API = { id: projectId(LOCAL, "/home/dev/projects/billing-api"), serverId: LOCAL.id, folder: "/home/dev/projects/billing-api", label: "billing-api" };
const P_WEB = { id: projectId(LOCAL, "/home/dev/projects/storefront"), serverId: LOCAL.id, folder: "/home/dev/projects/storefront", label: "storefront" };
const P_SRV = { id: projectId(DEVBOX, "/srv/app"), serverId: DEVBOX.id, folder: "/srv/app", label: "app" };

/** Fixture sessions. `status` drives both the pushed `claude-status` events and
 *  what the screen poll (`claude_state`) reports, so the two never disagree. */
export const SESSIONS = [
  { id: "session-login", projectId: P_API.id, kind: "claude", name: "Fix the login redirect loop", resume: "conv-login", status: "idle", conversation: "long" },
  { id: "session-blocked", projectId: P_API.id, kind: "claude", name: "Rotate the webhook signing secret", resume: "conv-blocked", status: "waiting", conversation: "blocked" },
  { id: "pisession-parser", projectId: P_WEB.id, kind: "pi", name: "Refactor the price parser", status: "busy", conversation: "short" },
  { id: "codexsession-tests", projectId: P_WEB.id, kind: "codex", name: "Migrate tests to vitest", status: null, conversation: "empty" },
  { id: "term-shell", projectId: P_API.id, kind: "terminal", name: "shell" },
  { id: "term-logs", projectId: P_API.id, kind: "terminal", name: "tail -f logs/app.log" },
  {
    id: "session-deploy",
    projectId: P_SRV.id,
    kind: "claude",
    name: "Investigate why the deployment pipeline invalidates the whole CDN cache on every release",
    resume: "conv-deploy",
    status: "idle",
    conversation: "short",
  },
];

function seed() {
  const workspace = {
    version: 3,
    servers: [LOCAL, DEVBOX],
    projects: [P_API, P_WEB, P_SRV],
    sessions: SESSIONS.map(({ id, projectId: p, kind, name, resume }) => ({ id, projectId: p, kind, name, ...(resume ? { resume } : {}) })),
    panes: [],
    activeSession: null,
    openPaths: {},
    activePath: {},
  };
  return { keys: { "rmux.workspace.v3": JSON.stringify(workspace) } };
}

// ── conversations ───────────────────────────────────────────────────────────

const at = (minute) => new Date(Date.UTC(2026, 10, 13, 9, minute, 0)).toISOString();

const LONG_PATH =
  "/home/dev/projects/billing-api/node_modules/.pnpm/@example+session-middleware@4.12.0_express@5.1.0/node_modules/@example/session-middleware/dist/esm/internal/redirect-guard.js";
const LONG_URL =
  "https://auth.example.com/oauth2/v1/authorize?client_id=0oa1b2c3d4e5f6g7h8i9&redirect_uri=https%3A%2F%2Fapp.example.com%2Fauth%2Fcallback&response_type=code&scope=openid%20profile%20email&state=Zm9vYmFyYmF6cXV4cXV1eGNvcmdlZ3JhdWx0";

const CODE = `\`\`\`typescript
// src/auth/redirect.ts
export function safeRedirect(target: string | undefined, fallback = "/dashboard"): string {
  if (!target) return fallback;
  // Only same-origin paths. A protocol-relative URL ("//evil.example") parses as a path in naive checks.
  if (!target.startsWith("/") || target.startsWith("//") || target.startsWith("/\\\\")) return fallback;
  const url = new URL(target, "https://placeholder.invalid");
  if (url.origin !== "https://placeholder.invalid") return fallback;
  if (url.pathname === "/login" || url.pathname.startsWith("/login/")) return fallback; // the loop itself
  return url.pathname + url.search + url.hash;
}
\`\`\``;

const TABLE = `| Route | Before | After | Notes |
|---|---|---|---|
| \`/login?next=/login\` | 302 → \`/login?next=/login\` (loop) | 302 → \`/dashboard\` | the reported bug |
| \`/login?next=//evil.example\` | 302 → \`//evil.example\` | 302 → \`/dashboard\` | open redirect, fixed too |
| \`/login?next=/settings/billing?tab=invoices\` | 302 → same | 302 → same | unchanged |
| \`/logout\` | 302 → \`/login\` | 302 → \`/login\` | unchanged |`;

const LOG = `$ pnpm vitest run src/auth
 ✓ src/auth/redirect.test.ts (14 tests) 23ms
 ✓ src/auth/session.test.ts (31 tests) 118ms
 ✓ src/auth/callback.test.ts (9 tests) 41ms

 Test Files  3 passed (3)
      Tests  54 passed (54)
   Start at  09:41:07
   Duration  1.32s (transform 402ms, setup 0ms, collect 610ms, tests 182ms)`;

function longConversation() {
  const e = [];
  let m = 0;
  const push = (speaker, text, tool) => e.push({ speaker, text, ...(tool ? { tool } : {}), timestamp: at(m++) });
  push("system", "<command-name>/clear</command-name>");
  push("user", `After signing in, some users bounce between /login and /dashboard forever. The browser eventually says "too many redirects". Can you find out why and fix it? Logs mention ${LONG_PATH}`);
  push("assistant", "I'll start by reading the login route and the session middleware it depends on.");
  push("tool", "src/routes/login.ts (148 lines)", "Read");
  push("tool", "src/auth/session.ts (212 lines)", "Read");
  push("tool", `${LONG_PATH} (90 lines)`, "Read");
  push("tool", "grep -rn \"next=\" src/ | head -40\nsrc/routes/login.ts:41:  const next = req.query.next as string;\nsrc/routes/login.ts:57:  res.redirect(next ?? \"/dashboard\");", "Bash");
  push(
    "assistant",
    `Found it. The login route trusts \`next\` verbatim, and the session middleware sends an unauthenticated request to \`/login?next=<current path>\`. When the session cookie is set on a different path than the one being checked, \`next\` becomes \`/login\` itself — so a signed-in user is redirected to the login page, which redirects back to \`next\`, which is the login page.\n\nThe identity provider's authorize URL is fine; it is only our own redirect that loops:\n\n${LONG_URL}\n\nHere is the guard I'd like to add:\n\n${CODE}`,
  );
  push("user", "Looks right. Also make sure we don't open-redirect to another site while you're in there.");
  push("assistant", "The guard above already refuses anything that is not a same-origin path, including protocol-relative URLs. Applying it and adding tests.");
  push("tool", "src/auth/redirect.ts — created (24 lines)", "Write");
  push("tool", "src/routes/login.ts — 2 lines changed\n-  res.redirect(next ?? \"/dashboard\");\n+  res.redirect(safeRedirect(next));", "Edit");
  push("tool", "src/auth/redirect.test.ts — created (96 lines)", "Write");
  push("tool", LOG, "Bash");
  push("system", "<system-reminder>The user may want a summary.</system-reminder>");
  push(
    "assistant",
    `All 54 auth tests pass. What changes for each kind of \`next\`:\n\n${TABLE}\n\nI did **not** touch the cookie path mismatch itself — that is what produces \`next=/login\` in the first place, and changing it logs everyone out once. Want me to fix that too, in a separate change?`,
  );
  push("user", "Yes, separately. Put it behind the release flag.");
  push("assistant", "Will do. I'll start a new branch for it so this fix can ship on its own.");
  return e;
}

function blockedConversation() {
  return [
    { speaker: "user", text: "Rotate the webhook signing secret and update the three consumers.", timestamp: at(30) },
    { speaker: "assistant", text: "I need to write the new secret to `config/webhooks.production.toml`, which is outside the folders you allowed.", timestamp: at(31) },
    { speaker: "tool", tool: "Edit", text: "config/webhooks.production.toml — waiting for permission", timestamp: at(32) },
  ];
}

function shortConversation() {
  return [
    { speaker: "user", text: "Make the price parser accept `1.234,56 €` as well as `€1,234.56`.", timestamp: at(40) },
    { speaker: "assistant", text: "Reading the current parser and its tests first.", timestamp: at(41) },
    { speaker: "tool", tool: "read", text: "src/lib/price.ts", timestamp: at(42) },
  ];
}

function transcriptFor(conversation) {
  const entries =
    conversation === "long" ? longConversation() : conversation === "blocked" ? blockedConversation() : conversation === "empty" ? [] : shortConversation();
  const bytes = JSON.stringify(entries).length;
  return {
    sessionId: conversation,
    entries,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, turns: entries.length },
    perTurn: [],
    // The long one claims more on disk than was read, so LOAD EARLIER shows.
    totalBytes: conversation === "long" ? bytes * 40 : bytes,
    readBytes: bytes,
    status: {},
  };
}

// ── the terminal screen ─────────────────────────────────────────────────────

const ESC = "\x1b";
function screen(session) {
  const lines =
    session.kind === "terminal"
      ? [`${ESC}[32mdev@host${ESC}[0m:${ESC}[34m~/projects/billing-api${ESC}[0m$ git status --short`, " M src/routes/login.ts", "?? src/auth/redirect.ts", `${ESC}[32mdev@host${ESC}[0m:${ESC}[34m~/projects/billing-api${ESC}[0m$ `]
      : [
          `${ESC}[38;5;174m✻${ESC}[0m ${session.name}`,
          "",
          `${ESC}[1m●${ESC}[0m All 54 auth tests pass.`,
          "",
          session.status === "waiting" ? `${ESC}[33mDo you want to make this edit to webhooks.production.toml?${ESC}[0m` : "",
          session.status === "waiting" ? ` ${ESC}[36m❯ 1. Yes${ESC}[0m` : "",
          session.status === "waiting" ? "   2. Yes, and don't ask again this session" : "",
          session.status === "waiting" ? "   3. No, and tell Claude what to do differently" : "",
          "",
          "╭──────────────────────────────────────╮",
          "│ >                                    │",
          "╰──────────────────────────────────────╯",
        ];
  return Buffer.from(lines.join("\r\n"), "utf8");
}

// ── the websocket, by hand (server → client frames only) ────────────────────

function frame(opcode, payload) {
  const len = payload.length;
  let head;
  if (len < 126) head = Buffer.from([0x80 | opcode, len]);
  else if (len < 65536) {
    head = Buffer.alloc(4);
    head[0] = 0x80 | opcode;
    head[1] = 126;
    head.writeUInt16BE(len, 2);
  } else {
    head = Buffer.alloc(10);
    head[0] = 0x80 | opcode;
    head[1] = 127;
    head.writeBigUInt64BE(BigInt(len), 2);
  }
  return Buffer.concat([head, payload]);
}
const sendText = (socket, value) => socket.writable && socket.write(frame(0x1, Buffer.from(JSON.stringify(value))));
const sendBytes = (socket, channel, bytes) => {
  if (!socket.writable) return;
  const id = Buffer.alloc(4);
  id.writeUInt32BE(channel, 0);
  socket.write(frame(0x2, Buffer.concat([id, bytes])));
};

function statusEvents() {
  const out = [];
  for (const s of SESSIONS) {
    if (!s.status) continue;
    const project = [P_API, P_WEB, P_SRV].find((p) => p.id === s.projectId);
    const targetId = project.serverId === LOCAL.id ? "local" : "dev@devbox";
    if (s.kind === "claude") {
      out.push({ targetId, sessionId: s.resume, cwd: project.folder, status: s.status, updatedAt: Date.UTC(2026, 10, 13, 9, 59) });
    } else if (s.kind === "pi") {
      out.push({ targetId, agent: "pi", sessionId: `pi-${s.id}`, status: s.status, updatedAt: Date.UTC(2026, 10, 13, 9, 58) });
    }
  }
  return out;
}

// ── the plugin ──────────────────────────────────────────────────────────────

export function rmuxFixture() {
  const conns = new Map();
  const handles = new Map(); // handle id → session
  let nextConn = 1;

  const readBody = (req) =>
    new Promise((resolve) => {
      let raw = "";
      req.on("data", (c) => (raw += c));
      req.on("end", () => {
        try {
          resolve(raw ? JSON.parse(raw) : {});
        } catch {
          resolve({});
        }
      });
    });

  const channelOf = (value) => (typeof value === "string" && value.startsWith("__CHANNEL__:") ? Number(value.slice(12)) : null);

  const sessionByName = (name) =>
    SESSIONS.find((s) => name === s.id || name === `claude-${s.id}` || name === `pi-${s.id}` || name === `codex-${s.id}`);

  const start = (args, conn) => {
    const session = sessionByName(String(args.sessionName ?? args.session ?? "")) ?? SESSIONS[0];
    const id = `fx-${session.id}`;
    handles.set(id, session);
    const channel = channelOf(args.output);
    const socket = conns.get(conn);
    if (socket && channel != null) setTimeout(() => sendBytes(socket, channel, screen(session)), 150);
    return { id };
  };

  const answer = async (cmd, args, conn) => {
    switch (cmd) {
      case "claude_transcript":
      case "pi_transcript":
      case "codex_transcript": {
        const session =
          SESSIONS.find((s) => s.resume && s.resume === args.session) ??
          SESSIONS.find((s) => s.kind === cmd.split("_")[0] && [P_API, P_WEB, P_SRV].some((p) => p.id === s.projectId && (p.folder === args.folder || p.folder === args.cwd)));
        return transcriptFor(session?.conversation ?? "empty");
      }
      case "claude_start":
      case "pi_start":
      case "codex_start":
      case "terminal_open":
        return start(args, conn);
      case "claude_state":
      case "codex_state": {
        const s = handles.get(args.id);
        const waiting = s?.status === "waiting";
        return {
          prompt: waiting ? { question: "Do you want to make this edit?", choices: [], fingerprint: "fx" } : null,
          working: s?.status === "busy",
        };
      }
      case "claude_send":
        await new Promise((r) => setTimeout(r, 400));
        return null;
      case "claude_list_sessions":
      case "claude_list_all_sessions":
      case "pi_list_all_sessions":
      case "codex_list_all_sessions":
        return [];
      default:
        return null;
    }
  };

  const api = async (req, res, next) => {
    const url = req.url ?? "";
    if (url === "/api/seed") {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify(seed()));
      return;
    }
    if (url.startsWith("/api/invoke/") && req.method === "POST") {
      const cmd = decodeURIComponent(url.slice("/api/invoke/".length));
      const args = await readBody(req);
      const result = await answer(cmd, args, req.headers["x-rmux-conn"]);
      res.setHeader("Content-Type", "application/json");
      res.end(result === null ? "" : JSON.stringify(result));
      return;
    }
    next();
  };

  const upgrade = (req, socket) => {
    if (!(req.url ?? "").startsWith("/api/ws")) return;
    const key = req.headers["sec-websocket-key"];
    const accept = createHash("sha1").update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest("base64");
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    const conn = `fx-conn-${nextConn++}`;
    conns.set(conn, socket);
    const drop = () => conns.delete(conn);
    socket.on("close", drop);
    socket.on("error", drop);
    socket.on("data", (buf) => {
      if ((buf[0] & 0x0f) === 0x8) socket.end();
    });
    sendText(socket, { hello: { conn } });
    setTimeout(() => {
      for (const payload of statusEvents()) sendText(socket, { event: "claude-status", payload });
    }, 30);
  };

  /** What rmux-web injects into every page it serves (`crates/rmux-web/src/ui.rs`). */
  const META = '<meta name="rmux-shell" content="web">';
  const inject = (html) => html.replace(/<head>/i, `<head>\n    ${META}`);

  return {
    name: "rmux-mobile-fixture",
    transformIndexHtml: inject,
    configureServer(server) {
      server.middlewares.use(api);
      server.httpServer?.on("upgrade", upgrade);
    },
    // `vite preview` serves the *built* bundle — the artefact that ships — with
    // the same fake backend. The page is served the way rmux-web serves it:
    // `index.html` for every non-file path, with the shell meta injected.
    configurePreviewServer(server) {
      const outDir = join(server.config.root, server.config.build.outDir);
      server.middlewares.use(api);
      server.middlewares.use((req, res, next) => {
        const path = (req.url ?? "/").split("?")[0];
        if (req.method !== "GET" || (path !== "/" && path !== "/index.html")) return next();
        res.setHeader("Content-Type", "text/html; charset=utf-8");
        res.setHeader("Cache-Control", "no-store");
        res.end(inject(readFileSync(join(outDir, "index.html"), "utf8")));
      });
      server.httpServer?.on("upgrade", upgrade);
    },
  };
}
