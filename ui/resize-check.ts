/**
 * Does the divider land where the pointer is?
 *
 * Run in **Safari** at http://localhost:5273/resize-check.html and read the
 * console. Safari specifically, for the same reason `zoom-check.html` says so:
 * WebKit implements standardised `zoom` and Chrome still uses the legacy one, so
 * Chrome gives a different — and here, misleadingly passing — answer.
 *
 * The geometry is built with real elements rather than stubbed numbers, because
 * the entire bug was a disagreement between two coordinate systems. A test that
 * supplied both of them would have agreed with the broken code.
 */
import { measureScale, widthFromPointer, TREE_MIN, TREE_MAX } from "./src/lib/pane-resize";

let failures = 0;

function check(name: string, ok: boolean, detail: string) {
  if (ok) {
    console.log(`%c PASS %c ${name} — ${detail}`, "background:#2b7;color:#000", "");
  } else {
    failures++;
    console.error(`FAIL  ${name} — ${detail}`);
  }
}

/** A pane inset from the window's left edge, inside a zoomed subtree. */
function pane(zoom: number, offsetLeft: number): HTMLElement {
  const outer = document.createElement("div");
  outer.style.cssText = `zoom:${zoom};position:absolute;top:0;left:0;width:1400px`;
  const el = document.createElement("div");
  // `margin-left`, not a sibling spacer: a block-level spacer offsets the next
  // element *vertically*, which left `rect.left` at 0 and made this fixture
  // silently test the one case the bug did not affect.
  el.style.cssText = `width:800px;height:400px;margin-left:${offsetLeft}px`;
  outer.append(el);
  document.body.append(outer);
  return el;
}

// --- the scale is what the browser is actually applying -----------------------

for (const zoom of [1, 1.09, 1.25, 0.9]) {
  const el = pane(zoom, 0);
  const scale = measureScale(el);
  check(
    `scale at zoom ${zoom}`,
    Math.abs(scale - zoom) < 0.01,
    `measured ${scale.toFixed(3)}, expected ${zoom}`,
  );
}

const hidden = document.createElement("div");
hidden.style.display = "none";
document.body.append(hidden);
check(
  "a hidden pane does not produce NaN",
  measureScale(hidden) === 1,
  `offsetWidth 0 → ${measureScale(hidden)} (a NaN width would blank the view)`,
);

// --- the divider lands under the pointer, wherever the pane is ---------------
//
// This is the whole bug. The old code was `clientX - 220`, so it only worked for
// a pane whose left edge happened to be at 220 viewport pixels, at zoom 1.

{
  // The reported case: a session in the top-right cell of a 2x2 grid.
  const offsetLeft = 620;
  const el = pane(1, offsetLeft);
  const rect = el.getBoundingClientRect();
  const scale = measureScale(el);

  // Point 300 layout px into the pane and ask for the width back.
  const clientX = rect.left + 300;
  const got = widthFromPointer(clientX, rect.left, scale);
  check(
    "a pane in a grid cell tracks the pointer",
    got === 300,
    `pane starts at ${Math.round(rect.left)}px, pointer 300px in → ${got}`,
  );

  const old = Math.min(Math.max(clientX - 220, TREE_MIN), TREE_MAX);
  check(
    "the old arithmetic is genuinely broken here (red-then-green)",
    old === TREE_MAX && got !== old,
    `clientX-220 = ${Math.round(clientX - 220)} → clamps to ${old}, pinned at the maximum`,
  );
}

{
  // Under zoom, one pixel of pointer travel must be one pixel of pane.
  const el = pane(1.25, 100);
  const rect = el.getBoundingClientRect();
  const scale = measureScale(el);

  const a = widthFromPointer(rect.left + 250, rect.left, scale);
  const b = widthFromPointer(rect.left + 300, rect.left, scale);
  check(
    "zoomed: the pane moves at the pointer's rate",
    Math.abs(b - a - 50 / 1.25) < 1.5,
    `50 viewport px of travel moved the pane ${b - a}px (expected ${(50 / 1.25).toFixed(0)})`,
  );

  const naive = 300; // what you get by ignoring the scale
  check(
    "zoomed: ignoring the scale is measurably wrong",
    Math.abs(b - naive) > 20,
    `scaled ${b} vs unscaled ${naive} — the pane would slide out from under the cursor`,
  );
}

// --- the clamps hold ---------------------------------------------------------

{
  const el = pane(1, 0);
  const rect = el.getBoundingClientRect();
  check(
    "dragging past the left edge stops at the minimum",
    widthFromPointer(rect.left - 500, rect.left, 1) === TREE_MIN,
    `→ ${TREE_MIN}`,
  );
  check(
    "dragging past the right edge stops at the maximum",
    widthFromPointer(rect.left + 5000, rect.left, 1) === TREE_MAX,
    `→ ${TREE_MAX}`,
  );
  check(
    "the width is a whole number",
    Number.isInteger(widthFromPointer(rect.left + 300.7, rect.left, 1.09)),
    "a fractional width blurs the tree's text on a non-integer boundary",
  );
}

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASSED ",
  `background:${failures ? "#e63b2e" : "#2b7"};color:#000;font-weight:bold`,
);
