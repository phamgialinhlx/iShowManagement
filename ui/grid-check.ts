import { gridCells, isFocus, layoutPanes, openTarget, parseGridLayout } from "./src/lib/grid";
import type { PaneRef } from "./src/lib/workspace-model";

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
const eight = ["a", "b", "c", "d", "e", "f", "g", "h"].map(s);

const S = (id: string): PaneRef => ({ kind: "session", id });
const E: PaneRef = { kind: "empty" };
const pcell = (cells: (PaneRef | null)[]) =>
  cells.map((c) => (c ? (c.kind === "session" ? c.id : c.kind) : null));

// ---- the layout table: cells follow rows × cols, and only 1x1 is focus mode
check("a 1x1 layout has one cell", layoutPanes([], eight, null, "1x1").length === 1);
check("a 1x2 layout has two cells", layoutPanes([], eight, null, "1x2").length === 2);
check("a 1x3 layout has three cells", layoutPanes([], eight, null, "1x3").length === 3);
check("a 4x4 layout has sixteen cells", layoutPanes([], eight, null, "4x4").length === 16);
check("gridCells matches rows × cols", gridCells("1x3") === 3 && gridCells("3x3") === 9);
check("only 1x1 is focus mode", isFocus("1x1") && !isFocus("1x2") && !isFocus("2x2"));

// ---- reading the persisted layout (the old numeric format included)
check('old "2".."4" read as their squares',
  parseGridLayout("2") === "2x2" && parseGridLayout("3") === "3x3" && parseGridLayout("4") === "4x4");
check('old "1" reads as focus', parseGridLayout("1") === "1x1");
check("new ids round-trip", parseGridLayout("1x3") === "1x3" && parseGridLayout("2x2") === "2x2");
check("garbage falls back to focus",
  parseGridLayout(null) === "1x1" && parseGridLayout("garbage") === "1x1" && parseGridLayout("5") === "1x1");

// ---- auto-fill: the grid is useful before anyone arranges anything
check(
  "an empty arrangement fills in rail order",
  JSON.stringify(pcell(layoutPanes([], eight, null, "2x2"))) === JSON.stringify(["a", "b", "c", "d"]),
);
check(
  "cells past the session count are empty",
  pcell(layoutPanes([], eight.slice(0, 2), null, "2x2")).slice(2).every((x) => x === null),
);

// ---- assignment: the whole point. Any session, in any cell.
check(
  "an assignment wins over rail order",
  JSON.stringify(pcell(layoutPanes([S("h")], eight, null, "2x2"))) ===
    JSON.stringify(["h", "a", "b", "c"]),
);
check(
  "assignments hold their own cells",
  JSON.stringify(pcell(layoutPanes([null, S("g"), null, S("h")], eight, null, "2x2"))) ===
    JSON.stringify(["a", "g", "b", "h"]),
);

// ---- the invariant that matters most
check(
  "a session never appears twice",
  (() => {
    const cells = layoutPanes([S("h")], eight, null, "2x2");
    const sids = cells.flatMap((c) => (c && c.kind === "session" ? [c.id] : []));
    return new Set(sids).size === sids.length && sids.includes("h");
  })(),
);

// ---- focusing a pane must never reshuffle a grid (the bug where clicking the
//      bottom-right cell swapped panes and restarts jumped around)
check(
  "a 2x2 pane layout is independent of the active session",
  JSON.stringify(pcell(layoutPanes([], eight, "a", "2x2"))) ===
    JSON.stringify(pcell(layoutPanes([], eight, "d", "2x2"))),
);
check(
  "the ultrawide split (1x2) does not reshuffle on click either",
  JSON.stringify(pcell(layoutPanes([], eight, "a", "1x2"))) ===
    JSON.stringify(pcell(layoutPanes([], eight, "b", "1x2"))),
);
check(
  "focus mode (1x1) shows the active session in its one cell",
  (() => {
    const c = layoutPanes([], eight, "c", "1x1")[0];
    return !!c && c.kind === "session" && c.id === "c";
  })(),
);
check(
  "an explicit host pane holds its cell; the rest auto-fill in rail order",
  JSON.stringify(pcell(layoutPanes([{ kind: "host", serverId: "s1" }], eight, "d", "2x2"))) ===
    JSON.stringify(["host", "a", "b", "c"]),
);

// ---- a tile cleared on purpose stays cleared
check(
  "auto-fill does not refill an empty-kind tile",
  JSON.stringify(pcell(layoutPanes([E], eight, null, "1x2"))) === JSON.stringify(["empty", "a"]),
);

// ---- where an "open into the deck" lands (openTarget)
check("the focused cell wins", openTarget([S("a"), null], 0) === 0);
check("with no focus, the first empty tile takes it", openTarget([S("a"), null], null) === 1);
check("an explicitly cleared tile counts as open-able", openTarget([S("a"), E, S("b")], null) === 1);
check("a full deck falls back to cell 0", openTarget([S("a"), S("b")], null) === 0);
check("an out-of-range focus is ignored", openTarget([S("a"), null], 5) === 1);

// ---- degenerate inputs
check(
  "no sessions gives empty cells",
  pcell(layoutPanes([], [], null, "2x2")).every((x) => x === null),
);

console.log(failures === 0 ? "\nall grid checks passed" : `\n${failures} grid check(s) FAILED`);
