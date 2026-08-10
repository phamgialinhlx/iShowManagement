/**
 * Does the context menu stay reachable, and stay put as it grows?
 *
 * Open http://localhost:5273/menu-check.html and read the console.
 *
 * ## Why this exists
 *
 * A right-click near the bottom of a long list used to put half the items below
 * the edge of the app — unreachable, with nothing indicating they existed.
 * Delete was simply gone. The fix is **flip before clamp**, and the reason it
 * has to be flip rather than clamp is not cosmetic: clamping slides the list up
 * *under* the stationary cursor, so the pointer ends up over whichever item
 * happens to land there. That is how someone deletes a file they meant to
 * rename.
 *
 * That behaviour has now been moved out of `TreeMenu` into `MenuSurface` so the
 * rail and the file tree share one menu. Moving load-bearing geometry is exactly
 * when it silently breaks, so it is measured here rather than eyeballed.
 *
 * ## The one that a dependency array cannot catch
 *
 * A menu changes height *after* it opens — a rename field appears, a delete
 * confirmation replaces the items. The old version listed those states in a
 * `useLayoutEffect` dependency array, so every new inline state a caller added
 * was a placement bug waiting for someone to right-click near the bottom of the
 * screen. `MenuSurface` observes its own size instead. Test 4 is that promise.
 */
import "./src/styles/signal-room.css";
import { createElement, useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";

import { MenuItem, MenuSurface } from "./src/components/Menu";

let failures = 0;
const check = (name: string, ok: boolean, detail: string) => {
  if (ok) console.log(`%c PASS %c ${name} — ${detail}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name} — ${detail}`);
  }
};

const stage = document.querySelector<HTMLElement>("#stage")!;

/** Mount a menu at a point and hand back its rendered box. */
function open(at: { x: number; y: number }, items: number): DOMRect {
  const host = document.createElement("div");
  stage.appendChild(host);
  flushSync(() => {
    createRoot(host).render(
      createElement(MenuSurface, {
        at,
        onClose: () => {},
        children: Array.from({ length: items }, (_, i) =>
          createElement(MenuItem, { key: i, label: `Item ${i + 1}`, onClick: () => {} }),
        ),
      }),
    );
  });
  return document.querySelector<HTMLElement>("[data-menu]")!.getBoundingClientRect();
}

const reset = () => {
  stage.innerHTML = "";
};

// 1. Opened with room below, the menu hangs from the cursor.
{
  reset();
  const at = { x: 40, y: 40 };
  const r = open(at, 4);
  check(
    "with room below, it grows downward from the cursor",
    Math.abs(r.top - at.y) < 2 && r.bottom < window.innerHeight,
    `top ${Math.round(r.top)} for a click at ${at.y}`,
  );
}

// 2. Near the bottom it FLIPS: the cursor must end up on the menu's edge, not
//    somewhere in the middle of the list.
{
  reset();
  const at = { x: 40, y: window.innerHeight - 30 };
  const r = open(at, 4);
  const flipped = Math.abs(r.bottom - at.y) < 2;
  check(
    "near the bottom it flips upward",
    flipped,
    `bottom ${Math.round(r.bottom)} for a click at ${at.y} (flipped=${flipped})`,
  );
  check(
    "and stays inside the window",
    r.top >= 0 && r.bottom <= window.innerHeight + 1,
    `top ${Math.round(r.top)}, bottom ${Math.round(r.bottom)}, window ${window.innerHeight}`,
  );
  // The point of flipping rather than clamping: no item sits under the cursor.
  check(
    "the cursor is not left over an item",
    at.y >= r.bottom - 2,
    `click ${at.y} vs menu bottom ${Math.round(r.bottom)}`,
  );
}

// 3. Too tall to flip → clamp, because unreachable is worse than displaced.
{
  reset();
  const at = { x: 40, y: window.innerHeight - 20 };
  const r = open(at, 60);
  check(
    "a menu too tall to flip is clamped into view",
    r.top >= 0 && r.top < window.innerHeight,
    `top ${Math.round(r.top)} with 60 items (height ${Math.round(r.height)})`,
  );
}

// 4. Near the right edge it flips left rather than overflowing.
{
  reset();
  const at = { x: window.innerWidth - 10, y: 40 };
  const r = open(at, 3);
  check(
    "near the right edge it flips left",
    r.right <= window.innerWidth + 1,
    `right ${Math.round(r.right)}, window ${window.innerWidth}`,
  );
}

// 5. **Growing after it opened re-places it.** This is the one a dependency
//    array misses: the menu is opened near the bottom, then a confirmation
//    replaces its items and makes it taller.
{
  reset();
  const at = { x: 40, y: window.innerHeight - 30 };
  const host = document.createElement("div");
  stage.appendChild(host);

  let grow: () => void = () => {};
  function Growing() {
    const [tall, setTall] = useState(false);
    grow = () => setTall(true);
    return createElement(MenuSurface, {
      at,
      onClose: () => {},
      children: Array.from({ length: tall ? 12 : 2 }, (_, i) =>
        createElement(MenuItem, { key: i, label: `Item ${i + 1}`, onClick: () => {} }),
      ),
    });
  }

  flushSync(() => createRoot(host).render(createElement(Growing)));
  const before = document.querySelector<HTMLElement>("[data-menu]")!.getBoundingClientRect();

  flushSync(() => grow());
  // ResizeObserver delivers asynchronously, so give it a frame.
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  const after = document.querySelector<HTMLElement>("[data-menu]")!.getBoundingClientRect();

  check(
    "it actually grew",
    after.height > before.height + 10,
    `${Math.round(before.height)}px → ${Math.round(after.height)}px`,
  );
  check(
    "after growing it is still inside the window",
    after.bottom <= window.innerHeight + 1 && after.top >= 0,
    `top ${Math.round(after.top)}, bottom ${Math.round(after.bottom)}, window ${window.innerHeight}`,
  );
}

reset();
console.log(
  failures === 0
    ? "%c ALL PASS %c the menu flips, clamps and re-places as it grows"
    : `%c ${failures} FAILED %c`,
  failures === 0 ? "background:#2b7;color:#000" : "background:#e63b2e;color:#fff",
  "",
);
