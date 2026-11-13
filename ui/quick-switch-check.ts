/**
 * The quick switcher's ranking (`src/lib/quick-switch.ts`): fuzzy scoring and
 * its order, highlight ranges, the `is:` / `@host` filters, the empty-query
 * order and the row ceiling.
 *
 *     node --experimental-strip-types ui/quick-switch-check.ts
 *
 * Every failure here is silent in the app: the session you typed is the second
 * row, or a filter quietly lets everything through, or the list grows past the
 * window with a thousand sessions on a host.
 */
import {
  MAX_ROWS,
  fuzzy,
  parseQuery,
  rank,
  segments,
  toRanges,
  touchMru,
  type SwitchItem,
} from "./src/lib/quick-switch.ts";

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    if (!ok) failures += 1;
    log(`${ok ? "PASS" : "FAIL"}  ${name}${ok || !detail ? "" : `  — ${detail}`}`);
  };

  const s = (id: string, name: string, extra: Partial<SwitchItem> = {}): SwitchItem => ({
    kind: "session",
    id,
    name,
    sessionKind: "claude",
    state: "idle",
    machine: "local",
    project: "rmux",
    ...extra,
  });
  const items: SwitchItem[] = [
    s("a", "shell", { sessionKind: "terminal", state: null, machine: "devbox", project: "infra" }),
    s("b", "api gateway", { machine: "devbox", project: "backend", branch: "main" }),
    s("c", "rmux-api", { state: "working", project: "rmux", folder: "/home/x/rmux" }),
    s("d", "docs", { state: "blocked", machine: "lab", project: "api", lastStatusAt: 10 }),
    s("e", "notes", { state: "done", machine: "lab", project: "notes", lastStatusAt: 20 }),
    s("f", "zebra", { tab: "apiary", project: "zoo" }),
    { kind: "project", id: "p1", name: "backend", state: null, machine: "devbox", folder: "/srv/backend" },
  ];
  const ids = (q: string, mru: string[] = [], current: string | null = null) =>
    rank(items, q, mru, current).rows.map((r) => r.item.id);

  // ── fuzzy ──
  check("subsequence matches", !!fuzzy("rmx", "rmux"));
  check("a missing letter does not", fuzzy("rmz", "rmux") === null);
  check("case-insensitive", !!fuzzy("API", "rmux-api"));
  const contiguous = fuzzy("api", "x-api")!.score;
  const scattered = fuzzy("api", "xa_p_i")!.score;
  check("contiguous beats scattered", contiguous > scattered, `${contiguous} vs ${scattered}`);
  const wordStart = fuzzy("g", "api gateway")!.score;
  const midWord = fuzzy("g", "ringo")!.score;
  check("a word start beats mid-word", wordStart > midWord, `${wordStart} vs ${midWord}`);
  check("lands on the word start, not the first occurrence", fuzzy("ap", "map-api")!.indices[0] === 4);
  check("camelCase hump is a word start", fuzzy("gw", "apiGateWay")!.indices.join() === "3,7");
  check("deterministic", JSON.stringify(fuzzy("ra", "rmux-api")) === JSON.stringify(fuzzy("ra", "rmux-api")));

  // ── ranking ──
  const api = ids("api");
  check("a name match outranks a project/tab match", api[0] === "c" || api[0] === "b", api.join(","));
  check(
    "name matches come before project-only and tab-only matches",
    api.indexOf("c") < api.indexOf("d") && api.indexOf("b") < api.indexOf("f"),
    api.join(","),
  );
  check("rmux-api (contiguous) above api gateway? both name hits, tie broken by state", api.slice(0, 2).sort().join() === "b,c");
  check("machine matches", ids("devbox").includes("a") && ids("devbox").includes("b"));
  check("branch matches", ids("main")[0] === "b");
  check("multi-word: every term must match somewhere", ids("api lab").join() === "d");
  check("no match, no rows", ids("qqqq").length === 0);

  // ── highlight ──
  const hit = rank(items, "api", [], null).rows.find((r) => r.item.id === "c")!;
  check("highlight ranges cover the matched name chars", JSON.stringify(hit.ranges) === "[[5,8]]", JSON.stringify(hit.ranges));
  const tabHit = rank(items, "apiary", [], null).rows.find((r) => r.item.id === "f")!;
  check("a match outside the name highlights nothing in it", tabHit.ranges.length === 0);
  check("ranges merge", JSON.stringify(toRanges([0, 1, 2, 5])) === "[[0,3],[5,6]]");
  check(
    "segments split plain and hit runs",
    JSON.stringify(segments("rmux-api", [[5, 8]])) ===
      JSON.stringify([{ text: "rmux-", hit: false }, { text: "api", hit: true }]),
  );

  // ── filters ──
  check("is:blocked", ids("is:blocked").join() === "d");
  check("is:b abbreviates", ids("is:b").join() === "d");
  check("b: short form", ids("b:").join() === "d");
  check("is:working", ids("is:working").join() === "c");
  check("is:done", ids("is:done").join() === "e");
  check("is:idle excludes terminals and projects", ids("is:idle").sort().join() === "b,f");
  check("two states union", ids("is:blocked is:done").sort().join() === "d,e");
  check("@host narrows to the machine", ids("@lab").sort().join() === "d,e");
  check("@host keeps projects there", ids("@devbox").sort().join() === "a,b,p1");
  check("filter plus text", ids("@devbox back").join() === "p1,b" || ids("@devbox back").join() === "b,p1");
  check("a half-typed `is:` filters nothing out", ids("is:").length === items.length);
  const pq = parseQuery("is:w @Lab foo");
  check("parse keeps filters out of the terms", pq.terms.join() === "foo" && pq.states.join() === "working" && pq.hosts.join() === "lab");

  // ── empty query ──
  const empty = ids("", ["f", "c", "a"], "f");
  check("recent sessions first, current one last of them", empty.slice(0, 3).join() === "c,a,f", empty.join(","));
  check("then needing attention: blocked before done", empty.slice(3, 5).join() === "d,e", empty.join(","));
  check("then the rest in sidebar order, projects last", empty.slice(5).join() === "b,p1", empty.join(","));
  check("no recency: attention first", ids("").slice(0, 2).join() === "d,e");

  // ── ceiling ──
  const many: SwitchItem[] = Array.from({ length: 400 }, (_, i) => s(`m${i}`, `session ${i}`));
  const capped = rank(many, "", [], null);
  check(`at most ${MAX_ROWS} rows`, capped.rows.length === MAX_ROWS);
  check("total reports everything that matched", capped.total === 400);
  check("a query is capped too", rank(many, "session", [], null).rows.length === MAX_ROWS);

  // ── recency list ──
  check("touch moves to front", touchMru(["a", "b", "c"], "c").join() === "c,a,b");
  check("touch prunes unknown ids", touchMru(["a", "gone", "b"], "b", (id) => id !== "gone").join() === "b,a");
  check("touch caps", touchMru(Array.from({ length: 50 }, (_, i) => `x${i}`), "new").length === 30);

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}

if (typeof window === "undefined") {
  const ok = run((line) => console.log(line));
  if (!ok) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
}
