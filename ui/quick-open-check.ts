/**
 * Quick open's ranking (`src/lib/quick-open.ts`).
 *
 *     node --experimental-strip-types ui/quick-open-check.ts
 *
 * Every failure here is silent in the app: the file you typed is the fourth
 * row, or a directory match outranks the file actually named that.
 */
import { MAX_RESULTS, pieces, rankFiles, scorePath } from "./src/lib/quick-open.ts";

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    if (!ok) failures += 1;
    log(`${ok ? "PASS" : "FAIL"}  ${name}${ok || !detail ? "" : `  — ${detail}`}`);
  };
  const top = (paths: string[], q: string, recent: string[] = []) => rankFiles(paths, q, recent).rows.map((r) => r.path);

  const paths = [
    "src/main/helpers/util.rs",
    "src/main.rs",
    "crates/rmux-fs/src/serve.rs",
    "ui/src/components/FileTree.tsx",
    "ui/src/components/FilesPane.tsx",
    "docs/file-tree.md",
    "foo/bar/index.ts",
    "FooBar.tsx",
    "README.md",
  ];

  check("a file-name match beats a directory match", top(paths, "main")[0] === "src/main.rs", top(paths, "main").join());
  check("camel humps in the name", top(paths, "fbar")[0] === "FooBar.tsx", top(paths, "fbar").join());
  check("ftree finds FileTree.tsx first", top(paths, "ftree")[0] === "ui/src/components/FileTree.tsx", top(paths, "ftree").join());
  check("a slash means the path", top(paths, "rmux-fs/serve")[0] === "crates/rmux-fs/src/serve.rs");
  check("case-insensitive", top(paths, "readme")[0] === "README.md");
  check("a non-match is out", scorePath("src/main.rs", "zzz") === null);
  check("spaces are ignored", top(paths, "file tree")[0] === "ui/src/components/FileTree.tsx", top(paths, "file tree").join());

  const hit = scorePath("ui/src/components/FileTree.tsx", "comft")!;
  check("dir and name ranges are both reported", hit.dirRanges.length > 0 && hit.nameRanges.length > 0, JSON.stringify(hit));
  check(
    "pieces reassemble the text",
    pieces("FileTree.tsx", [[0, 1], [4, 5]]).map((p) => p.text).join("") === "FileTree.tsx",
  );

  // Empty query: recent first, then the shallowest.
  const empty = top(paths, "", ["ui/src/components/FilesPane.tsx"]);
  check("empty query leads with the recent file", empty[0] === "ui/src/components/FilesPane.tsx", empty.join());
  check("then the shallowest", empty[1] === "FooBar.tsx" || empty[1] === "README.md", empty.join());

  // The ceiling: a monorepo has far more matches than fit.
  const many = Array.from({ length: 5000 }, (_, i) => `pkg${i}/src/index.ts`);
  const ranked = rankFiles(many, "index");
  check("results are capped", ranked.rows.length === MAX_RESULTS && ranked.total === 5000);

  // Instant: a quarter of a million paths, one keystroke.
  const huge = Array.from({ length: 250_000 }, (_, i) => `dir${i % 997}/sub${i % 13}/file${i}.ts`);
  const t0 = performance.now();
  rankFiles(huge, "file12345");
  const ms = performance.now() - t0;
  log(`      (250k paths ranked in ${ms.toFixed(0)} ms)`);
  check("250k paths rank in well under a second", ms < 1500, `${ms.toFixed(0)} ms`);

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}

if (typeof window === "undefined") {
  const ok = run((line) => console.log(line));
  if (!ok) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
}
