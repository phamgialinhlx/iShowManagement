/**
 * The worktree mirror's model (`src/lib/worktree-model.ts`): patches, the child
 * index, visible rows and the scroll anchor.
 *
 *     node --experimental-strip-types ui/worktree-check.ts
 */
import { Tree, anchoredScrollTop, visibleRows, type Entry } from "./src/lib/worktree-model.ts";

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    if (!ok) failures += 1;
    log(`${ok ? "PASS" : "FAIL"}  ${name}${ok || !detail ? "" : `  — ${detail}`}`);
  };
  const f = (path: string, version = "1"): Entry => ({ path, kind: "file", size: 1, version, ignored: false, lazy: false });
  const d = (path: string, lazy = false): Entry => ({ path, kind: "dir", size: 0, version: "1", ignored: lazy, lazy });

  const t = new Tree();
  t.apply({ upserts: [d("src"), f("src/b.rs"), f("src/A.rs"), d("src/z"), f("src/z/x"), f("README"), d("node_modules", true)] });
  const names = t.children("src").map((e) => e.path);
  check("directories first, then names case-insensitively", names.join() === "src/z,src/A.rs,src/b.rs", names.join());
  check("root children", t.children("").map((e) => e.path).join() === "node_modules,src,README");

  const changed = t.apply({ upserts: [f("src/b.rs", "2"), f("src/new.rs")] });
  check("a modified file is reported, a new one is not", changed.length === 1 && changed[0]!.path === "src/b.rs" && changed[0]!.version === "2");
  check("the new file is indexed", t.children("src").some((e) => e.path === "src/new.rs"));

  const renamed = t.apply({ renames: [{ from: "src/z", to: "lib" }] });
  check("rename moves the subtree", t.entries.has("lib/x") && !t.entries.has("src/z/x") && t.children("lib").length === 1);
  check("a renamed directory with no files reports nothing", renamed.every((c) => c.path !== "lib"));

  const atomic = t.apply({ renames: [{ from: "src/new.rs", to: "src/b.rs" }] });
  check(
    "write-temp-then-rename reports the destination",
    atomic.some((c) => c.path === "src/b.rs" && c.version === "1"),
    JSON.stringify(atomic),
  );

  const gone = t.apply({ removes: ["src"] });
  check("removing a directory removes everything under it", !t.entries.has("src/A.rs") && t.children("src").length === 0);
  check("and reports each file in it as gone", gone.some((c) => c.path === "src/A.rs" && c.version === null));

  // Visible rows follow the expanded set.
  const t2 = new Tree();
  t2.apply({ upserts: [d("a"), f("a/1"), f("a/2"), d("b"), f("b/1"), f("c")] });
  const rows = visibleRows((dir) => t2.children(dir), new Set(["a"]));
  check("rows walk expanded folders depth-first", rows.map((r) => `${r.depth}:${r.entry.path}`).join() === "0:a,1:a/1,1:a/2,0:b,0:c");
  const lazy = visibleRows((dir) => (dir === "b" ? null : t2.children(dir)), new Set(["b"]));
  check("a folder still listing shows no children yet", lazy.length === 3);

  // Scroll anchoring: a file created above the viewport must not move it.
  const before = visibleRows((dir) => t2.children(dir), new Set(["a"]));
  const anchor = { path: "b", offset: -5, index: before.findIndex((r) => r.entry.path === "b") };
  t2.apply({ upserts: [f("a/0")] });
  const after = visibleRows((dir) => t2.children(dir), new Set(["a"]));
  const top = anchoredScrollTop(after, anchor, 22);
  check("the anchored row keeps its place", top === after.findIndex((r) => r.entry.path === "b") * 22 + 5, String(top));
  check("no anchor, no correction", anchoredScrollTop(after, null, 22) === null);

  // A directory replaced by a file loses its subtree.
  const t3 = new Tree();
  t3.apply({ upserts: [d("x"), f("x/y")] });
  t3.apply({ upserts: [f("x")] });
  check("dir → file drops the subtree", !t3.entries.has("x/y") && t3.children("x").length === 0);

  // Big trees stay cheap to index.
  const big = new Tree();
  const upserts: Entry[] = [];
  for (let i = 0; i < 1000; i++) {
    upserts.push(d(`d${i}`));
    for (let j = 0; j < 100; j++) upserts.push(f(`d${i}/f${j}`));
  }
  const t0 = performance.now();
  big.apply({ upserts });
  big.children("d500");
  big.apply({ upserts: [f("d500/new")] });
  big.children("d500");
  const ms = performance.now() - t0;
  log(`      (101k entries indexed and patched in ${ms.toFixed(0)} ms)`);
  check("100k entries index in well under a second", ms < 1500);

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}

if (typeof window === "undefined") {
  const ok = run((line) => console.log(line));
  if (!ok) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
}
