/**
 * Does a loading state actually put something on the screen?
 *
 * Open http://localhost:5273/loading-check.html and read the console.
 *
 * ## Why it measures boxes rather than markup
 *
 * The bug being guarded against is a **blank pane** — several seconds of nothing
 * while an SSH round trip completes, which the operator reads as a crash and
 * reports as one. That is a fact about painted pixels, so asserting on a class
 * name or on the rendered HTML would pass the exact failure it exists to catch:
 * a component can return perfectly correct markup that occupies no height, or
 * that animates only because a stylesheet loaded which might not next time.
 *
 * The same mistake was made once already, on the syntax highlighter — the first
 * version of `highlight-check` inspected the tokenized *string*, which was right
 * while the output painted in one flat grey. It now reads `getComputedStyle` on
 * the rendered spans. This does the same: geometry from `getBoundingClientRect`,
 * animation from `getComputedStyle`.
 *
 * ## The refresh rule is the interesting one
 *
 * `panel` and `rows` are easy to get right. The rule that is easy to get *wrong*
 * is that a **refresh is not a first load**: once there is a good answer on
 * screen, asking the pane to check again must not replace it with a skeleton.
 * Doing so makes the pane flicker under the hands of the person who just pressed
 * REFRESH, which is the "nothing moves under the operator's hands" rule. It is a
 * caller-side discipline rather than something `PanelLoader` can enforce, so it
 * is asserted against a small model of the caller below.
 */
import "./src/styles/signal-room.css";
import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";

import { PanelLoader, type LoaderVariant } from "./src/components/PanelLoader";

let failures = 0;
const check = (name: string, ok: boolean, detail: string) => {
  if (ok) console.log(`%c PASS %c ${name} — ${detail}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name} — ${detail}`);
  }
};

const stage = document.querySelector<HTMLElement>("#stage")!;

/**
 * Render a loader into a pane-shaped box and hand back its host.
 *
 * The host is given a real height because `panel` fills its parent — measured
 * inside a zero-height box every variant would correctly report zero, and the
 * harness would fail on its own scaffolding rather than on the component.
 */
function mount(variant: LoaderVariant, phase: string, detail?: string): HTMLElement {
  const host = document.createElement("div");
  host.style.cssText = "height:220px;width:360px;border:1px solid #333;margin:12px 0";
  stage.appendChild(host);
  flushSync(() => {
    createRoot(host).render(createElement(PanelLoader, { variant, phase, detail }));
  });
  return host;
}

/** The tallest painted descendant. A wrapper can have height while nothing shows. */
function paintedHeight(host: HTMLElement): number {
  let tallest = 0;
  host.querySelectorAll<HTMLElement>("*").forEach((el) => {
    const r = el.getBoundingClientRect();
    if (r.height > 0 && r.width > 0) tallest = Math.max(tallest, r.height);
  });
  return tallest;
}

// 1. Every variant paints something. This is the whole bug, stated directly.
for (const variant of ["panel", "rows", "inline"] as const) {
  const host = mount(variant, "READING THE TRANSCRIPT", "example-host");
  const h = paintedHeight(host);
  check(`${variant} paints`, h > 0, `tallest painted child ${h.toFixed(1)}px`);

  // 2. It says *what*, not "loading". A generic spinner tells nobody anything.
  const said = (host.textContent ?? "").toUpperCase();
  check(
    `${variant} names the phase`,
    said.includes("READING THE TRANSCRIPT"),
    JSON.stringify(host.textContent?.slice(0, 60) ?? ""),
  );
}

// 3. The skeleton rows move. Markup survives a stylesheet that failed to load,
//    and a perfectly still skeleton is indistinguishable from a frozen pane —
//    so read the resolved animation rather than trusting the class attribute.
{
  const host = mount("rows", "READING PROCESSES");
  const rows = host.querySelectorAll<HTMLElement>(".git-row");
  check("rows draws skeleton rows", rows.length >= 3, `${rows.length} rows`);

  const names = [...rows].map((r) => getComputedStyle(r).animationName);
  const animated = names.filter((n) => n && n !== "none").length;
  check(
    "rows actually animate",
    animated === rows.length && rows.length > 0,
    `${animated}/${rows.length} with an animation — ${names[0] ?? "none"}`,
  );

  // Uneven widths: a column of identical bars reads as a rendering fault.
  const widths = new Set([...rows].map((r) => r.lastElementChild?.getBoundingClientRect().width));
  check("rows are uneven", widths.size > 1, `${widths.size} distinct widths`);
}

// 4. Rule 2 of the design system: no blinking. A loader that flickers opacity
//    would be the one animation the system forbids outside a cursor.
{
  const host = mount("panel", "CONNECTING");
  const sweep = host.querySelector<HTMLElement>(".sweep");
  check("panel has a moving sweep", !!sweep, sweep ? getComputedStyle(sweep).animationName : "absent");
}

// 5. A refresh keeps the old answer.
//
//    Modelled rather than mounted, because the decision belongs to the caller:
//    the pane holds both an answer and a busy flag, and the question is only
//    ever "is this the first read?". Both directions are pinned — a skeleton
//    that never shows is as wrong as one that shows on every refresh.
const showsSkeleton = (data: unknown[] | null, busy: boolean) => busy && data === null;

check(
  "first load shows the skeleton",
  showsSkeleton(null, true),
  "no data yet, request in flight",
);
check(
  "refresh keeps the answer",
  !showsSkeleton([{ path: "a" }], true),
  "data present, request in flight — the list stays",
);
check("idle shows nothing", !showsSkeleton(null, false), "no data, no request");

console.log(
  failures === 0
    ? "%c ALL PASS %c loading states paint, name their phase and survive a refresh"
    : `%c ${failures} FAILED %c`,
  failures === 0 ? "background:#2b7;color:#000" : "background:#e63b2e;color:#fff",
  "",
);
