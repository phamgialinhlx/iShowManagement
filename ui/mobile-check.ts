/**
 * The mobile UI's pure halves: grouping the session list, shaping a transcript
 * into chat bubbles, when the conversation may move, and what a composer key does.
 *
 *     node --experimental-strip-types ui/mobile-check.ts
 *
 * Every failure here is a silent one on a phone: a roll-up that loses to the
 * wrong state hides a blocked session, a key built from a list index makes a
 * "load more" remount every bubble, and an update applied while the reader is
 * scrolled up moves the text they were reading.
 */
import { groupSessions, serverLabel, totalNeedsYou } from "./src/lib/mobile-sessions.ts";
import { shapeBubbles, summarise, toolsLabel, type EntryLike } from "./src/lib/chat-bubbles.ts";
import { isNearBottom, mayFetch, planUpdate } from "./src/lib/chat-scroll.ts";
import { canSend, composerHeight, composerKey, QUICK_KEYS } from "./src/lib/composer-keys.ts";

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    if (!ok) failures += 1;
    log(`${ok ? "PASS" : "FAIL"}  ${name}${ok || !detail ? "" : `  — ${detail}`}`);
  };

  // ── grouping ──────────────────────────────────────────────────────────────
  const servers = [
    { id: "local", target: {} },
    { id: "ssh-a", target: { host: "devbox", user: "me" } },
    { id: "ssh-empty", target: { host: "nothing" } },
  ];
  const projects = [
    { id: "p1", serverId: "local", folder: "/w/api", label: "api" },
    { id: "p2", serverId: "ssh-a", folder: "/w/web", label: "web" },
    { id: "p3", serverId: "ssh-a", folder: "(running)", label: "(running)", running: true },
  ];
  const sessions = [
    { id: "c1", projectId: "p1", kind: "claude", name: "fix login" },
    { id: "t1", projectId: "p1", kind: "terminal", name: "shell" },
    { id: "pi1", projectId: "p2", kind: "pi", name: "pi work" },
    { id: "x1", projectId: "p2", kind: "codex", name: "codex" },
    { id: "d1", projectId: "p2", kind: "dsh", name: "dsh" },
    { id: "c2", projectId: "p3", kind: "claude", name: "adopted" },
  ];
  const groups = groupSessions({
    servers,
    projects,
    sessions,
    runtime: {
      c1: { status: "waiting" },
      pi1: { status: "working" },
      x1: { status: "idle", lastStatusAt: 50 },
    },
    seen: { x1: 10 },
  });
  check("servers with nothing to show are left out", groups.length === 2, JSON.stringify(groups.map((g) => g.id)));
  const local = groups.find((g) => g.id === "local")!;
  check("local server is named for what it is", local.label === "this host");
  check("ssh label is the alias, user first", serverLabel({ host: "devbox", user: "me" }) === "me@devbox");
  check("terminals are a separate group", local.projects[0]!.terminals.map((t) => t.id).join() === "t1");
  check("a terminal carries no state", local.projects[0]!.terminals[0]!.state === null);
  check("blocked comes from waiting", local.projects[0]!.agents[0]!.state === "blocked");
  const remote = groups.find((g) => g.id === "ssh-a")!;
  const web = remote.projects.find((p) => p.id === "p2")!;
  check("dsh is not listed", !web.agents.some((a) => a.kind === "dsh"));
  check("codex idle and unseen is done", web.agents.find((a) => a.id === "x1")!.state === "done");
  check("project rolls up to its most urgent", web.state === "done", String(web.state));
  check("an agent with no runtime is unknown, not idle", remote.projects.find((p) => p.id === "p3")!.agents[0]!.state === "unknown");
  check("running bucket gets a readable label", remote.projects.find((p) => p.id === "p3")!.label === "running on the host");
  check("needs-you counts blocked and done", totalNeedsYou(groups) === 2, String(totalNeedsYou(groups)));

  // ── bubbles ───────────────────────────────────────────────────────────────
  const entries: EntryLike[] = [
    { speaker: "system", text: "<command-name>/clear</command-name>" },
    { speaker: "user", text: "fix the login bug", timestamp: "t1" },
    { speaker: "assistant", text: "Looking.", timestamp: "t2" },
    { speaker: "tool", tool: "Read", text: "src/login.ts\n...", timestamp: "t3" },
    { speaker: "tool", tool: "Read", text: "src/auth.ts", timestamp: "t4" },
    { speaker: "tool", tool: "Edit", text: "  \n  src/login.ts: -1 +2", timestamp: "t5" },
    { speaker: "assistant", text: "   ", timestamp: "t6" },
    { speaker: "assistant", text: "Done.", timestamp: "t7" },
  ];
  const shaped = shapeBubbles(entries);
  check("system is hidden and counted", shaped.hiddenSystem === 1);
  check(
    "consecutive tools collapse into one row",
    shaped.bubbles.map((b) => b.kind).join() === "message,message,tools,message",
    shaped.bubbles.map((b) => b.kind).join(),
  );
  const tools = shaped.bubbles[2]!;
  check("tool row lists every call", tools.kind === "tools" && tools.calls.length === 3);
  check("tool label counts by name", tools.kind === "tools" && toolsLabel(tools.calls) === "Read 2, Edit");
  check("summary is the first non-blank line", summarise("  \n  hello  \nworld") === "hello");
  check("summary is cut", summarise("x".repeat(200), 10).length === 10);
  // Keys must not depend on position: prepending older turns ("load more")
  // must leave the existing bubbles' keys unchanged.
  const older: EntryLike[] = [{ speaker: "user", text: "earlier", timestamp: "t0" }, ...entries];
  const before = shapeBubbles(entries).bubbles.map((b) => b.key);
  const after = shapeBubbles(older).bubbles.map((b) => b.key);
  check("keys survive a load-more", before.every((k) => after.includes(k)), JSON.stringify({ before, after }));
  const dup = shapeBubbles([
    { speaker: "user", text: "yes" },
    { speaker: "user", text: "yes" },
  ]).bubbles.map((b) => b.key);
  check("identical turns get distinct keys", dup.length === 2 && dup[0] !== dup[1]);

  // ── scroll ────────────────────────────────────────────────────────────────
  check("at the end is near bottom", isNearBottom({ scrollHeight: 1000, scrollTop: 600, clientHeight: 400 }));
  check("scrolled up is not", !isNearBottom({ scrollHeight: 1000, scrollTop: 100, clientHeight: 400 }));
  check("at bottom: apply and follow", planUpdate({ atBottom: true, selecting: false }) === "apply-follow");
  check("scrolled up: hold behind the pill", planUpdate({ atBottom: false, selecting: false }) === "hold-pill");
  check("selecting beats everything", planUpdate({ atBottom: true, selecting: true }) === "hold");
  check("no fetch while selecting", !mayFetch({ selecting: true, hidden: false }));
  check("no fetch while hidden", !mayFetch({ selecting: false, hidden: true }));

  // ── composer ──────────────────────────────────────────────────────────────
  const k = (over: Partial<Parameters<typeof composerKey>[0]>) =>
    composerKey({ key: "Enter", metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, isComposing: false, ...over });
  check("plain Enter is a newline", k({}) === "default");
  check("Cmd+Enter sends", k({ metaKey: true }) === "send");
  check("Ctrl+Enter sends", k({ ctrlKey: true }) === "send");
  check("never during IME composition", k({ metaKey: true, isComposing: true }) === "default");
  check("other keys do nothing", k({ key: "a", metaKey: true }) === "default");
  check("whitespace is not a message", !canSend("  \n ") && canSend(" hi "));
  check("height floors at one line", composerHeight(5, 20, 16, 6) === 36);
  check("height caps at max lines", composerHeight(900, 20, 16, 6) === 136);
  check("quick keys are named, not bytes", QUICK_KEYS.every((q) => !/[\x00-\x1f]/.test(q.name)));
  check(
    "quick keys are the asked-for set",
    QUICK_KEYS.map((q) => q.name).join() === "1,2,3,up,down,enter,esc",
  );

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}

if (typeof window === "undefined") {
  const ok = run((line) => console.log(line));
  if (!ok) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
}
