import { gridLayout } from "./src/lib/grid";

/**
 * Checks for the grid's cell assignment.
 *
 * Open `http://localhost:5273/grid-check.html` and read the console, or run it
 * under `node --experimental-strip-types`.
 *
 * The failures this guards are all silent. The version before it derived cells
 * from list order — `sessions.slice(0, grid * grid)` — so with eight sessions
 * in a 2x2 the last four were simply unreachable, and nothing said so. The two
 * worse failures are a session that quietly vanishes from the grid, and one
 * that appears in two cells at once: that mounts its view twice, which means
 * two Claude panes attached to the same conversation, both typing into it.
 */

let failures = 0;
const check = (name: string, ok: boolean) => {
  if (ok) console.log(`ok   ${name}`);
  else {
    failures += 1;
    console.error(`FAIL ${name}`);
  }
};

const s = (id: string) => ({ id });
const ids = (cells: ({ id: string } | null)[]) => cells.map((c) => c?.id ?? null);

const eight = ["a", "b", "c", "d", "e", "f", "g", "h"].map(s);

// ---- auto-fill: the grid is useful before anyone arranges anything
check(
  "an empty arrangement fills in rail order",
  JSON.stringify(ids(gridLayout(eight, [], 2))) === JSON.stringify(["a", "b", "c", "d"]),
);
check("a 3x3 has nine cells", gridLayout(eight, [], 3).length === 9);
check(
  "cells past the session count are empty",
  ids(gridLayout(eight, [], 3)).slice(8).every((x) => x === null),
);

// ---- assignment: the whole point. Any session, in any cell.
check(
  "an assignment wins over rail order",
  JSON.stringify(ids(gridLayout(eight, ["h"], 2))) === JSON.stringify(["h", "a", "b", "c"]),
);
check(
  "assignments hold their own cells",
  JSON.stringify(ids(gridLayout(eight, [null, "g", null, "h"], 2))) ===
    JSON.stringify(["a", "g", "b", "h"]),
);
check(
  "every cell can be chosen",
  JSON.stringify(ids(gridLayout(eight, ["e", "f", "g", "h"], 2))) ===
    JSON.stringify(["e", "f", "g", "h"]),
);

// ---- the invariant that matters most
check(
  "a session never appears twice",
  JSON.stringify(ids(gridLayout(eight, ["a", "a", null, null], 2))) ===
    JSON.stringify(["a", "b", "c", "d"]),
);
check(
  "auto-fill skips what is already placed",
  !ids(gridLayout(eight, ["a"], 2))
    .slice(1)
    .includes("a"),
);

// ---- stale assignments must not hold a cell hostage
check(
  "a closed session's cell is reused",
  JSON.stringify(ids(gridLayout(eight, ["gone", null, null, null], 2))) ===
    JSON.stringify(["a", "b", "c", "d"]),
);

// ---- shrinking the grid must not lose anything permanently
check(
  "a smaller grid keeps the assignments it can show",
  JSON.stringify(ids(gridLayout(eight, [null, null, null, null, "h"], 2))) ===
    JSON.stringify(["a", "b", "c", "d"]),
);
check(
  "…and growing it back restores them",
  ids(gridLayout(eight, [null, null, null, null, "h"], 3))[4] === "h",
);

// ---- degenerate inputs
check("no sessions gives empty cells", ids(gridLayout([], ["a"], 2)).every((x) => x === null));
check("a 1x1 grid has one cell", gridLayout(eight, [], 1).length === 1);

console.log(failures === 0 ? "\nall grid checks passed" : `\n${failures} grid check(s) FAILED`);
