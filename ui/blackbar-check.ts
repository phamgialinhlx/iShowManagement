/**
 * Which layer paints the strip below the last row?
 *
 * Open http://localhost:5273/blackbar-check.html and read the console.
 *
 * ## What the first version of this got wrong
 *
 * It built an xterm, never called `fit()`, and measured. An unfitted terminal is
 * 80x24 whatever its container is, so it reported the screen hanging 159px
 * *below* the host — a fact about the harness, not about rmux. Two CSS changes
 * were then made on a hypothesis that no measurement had tested, and neither
 * worked: "the pane is not a whole number of cells tall" and "the terminal is
 * laid out taller than its pane" are different faults with opposite fixes.
 *
 * So this replicates the Claude pane's **actual** DOM chain —
 *
 *     <div class="relative min-h-0 flex-1">     the pane area
 *       <div class="h-full w-full p-2">         the host `fit()` measures
 *         <div class="xterm">                   xterm's own root
 *
 * — inside a flex column of fixed height, fits it, fills the scrollback so a
 * scrollbar genuinely exists, and reports every rectangle in the chain plus what
 * each element stacked at a point inside the strip is painting.
 *
 * The two questions it exists to separate:
 *
 * 1. **Is anything overflowing?** If `.xterm` or `.xterm-viewport` is taller
 *    than the pane, the bar is scrollable content sitting outside it and no
 *    background rule will ever hide it — that is a layout bug.
 * 2. **If nothing overflows, who paints the remainder?** `fit()` floors to whole
 *    rows, so up to one cell height is always left over. Then the bar is a
 *    background and the only question is whose.
 */
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal as Xterm } from "@xterm/xterm";

import "@xterm/xterm/css/xterm.css";
import "./src/styles/signal-room.css";

// **This has to be read in Safari, not Chrome.** WebKit implements the
// standardised `zoom` and Chrome still uses the legacy one, so they disagree on
// precisely the question being asked here — see the zoom rules in CLAUDE.md.
const line = (label: string, value: string) => {
  console.log(`%c${label.padEnd(28)}%c${value}`, "color:#7e7b74", "color:#e8e6e1");
};

// ---------------------------------------------------------------- the pane

// `?zoom=<n>` puts the whole chain under `zoom`, which is how the interface
// scale is applied — `#root` carries `zoom: var(--ui-zoom)`. This is a *second*
// way for `fit()` to undershoot, independent of the padding one, and it is the
// one that grows with the window: `FitAddon` reads the host height from
// `getComputedStyle` and the cell height from xterm's own rendered measurement,
// and if those two are not in the same coordinate space the error is a
// *proportion* of the pane rather than a fixed remainder.
const ZOOM = Number(new URLSearchParams(location.search).get("zoom") ?? "1") || 1;
const root = document.createElement("div");
root.style.zoom = String(ZOOM);
document.body.append(root);

const column = document.createElement("div");
column.style.cssText = "display:flex;flex-direction:column;height:521px;width:760px;background:#141414";
const header = document.createElement("div");
header.style.cssText = "flex:0 0 auto;height:29px;background:#1c1c1c";
// `?fixed` builds the corrected chain: the padding moves to the *pane*, so the
// element `fit()` measures is exactly the terminal's box. Without it, this is
// the shipped structure and the check is expected to fail.
const FIXED = new URLSearchParams(location.search).has("fixed");

const pane = document.createElement("div"); // relative min-h-0 flex-1 [p-2]
pane.style.cssText = `position:relative;min-height:0;flex:1 1 0%${FIXED ? ";padding:8px;box-sizing:border-box" : ""}`;
const host = document.createElement("div"); // h-full w-full [p-2]
host.style.cssText = `height:100%;width:100%;box-sizing:border-box${FIXED ? "" : ";padding:8px"}`;
pane.append(host);
column.append(header, pane);
root.append(column);

const term = new Xterm({
  allowTransparency: true,
  fontFamily: '"IBM Plex Mono", ui-monospace, Menlo, monospace',
  fontSize: 12,
  lineHeight: 1.3,
  scrollback: 5000,
  theme: { background: "rgba(0,0,0,0)" },
});
const fit = new FitAddon();
term.loadAddon(fit);
term.open(host);
try {
  term.loadAddon(new WebglAddon());
} catch {
  console.warn("no webgl here — the DOM renderer is being measured instead");
}

fit.fit();
// Enough to overflow the view, so a scrollbar exists: the reported bar has a
// scrollbar thumb *inside* it, which is the whole reason this matters.
for (let i = 0; i < 200; i++) term.write(`line ${i} ${"-".repeat(40)}\r\n`);

await new Promise((r) => setTimeout(r, 500));
fit.fit();
await new Promise((r) => setTimeout(r, 300));

// ---------------------------------------------------------------- measure

const q = (sel: string) => host.querySelector(sel) as HTMLElement | null;
const chain: [string, HTMLElement | null][] = [
  ["pane (flex-1)", pane],
  ["host (p-2)", host],
  [".xterm", q(".xterm")],
  [".xterm-viewport", q(".xterm-viewport")],
  [".xterm-screen", q(".xterm-screen")],
  [".xterm-scroll-area", q(".xterm-scroll-area")],
  ["canvas", host.querySelector("canvas")],
];

console.log(
  `%c GEOMETRY %c zoom ${ZOOM}, ${navigator.userAgent.includes("Chrome") ? "CHROME" : "WEBKIT"}`,
  "background:#e8e6e1;color:#000;font-weight:bold",
  "",
);
const rects = new Map<string, DOMRect>();
for (const [name, el] of chain) {
  if (!el) {
    line(name, "absent");
    continue;
  }
  const r = el.getBoundingClientRect();
  rects.set(name, r);
  line(
    name,
    `top ${r.top.toFixed(1)}  bottom ${r.bottom.toFixed(1)}  h ${r.height.toFixed(1)}  w ${r.width.toFixed(1)}`,
  );
}

const hostRect = rects.get("host (p-2)")!;
const paneRect = rects.get("pane (flex-1)")!;
const cell = (
  term as unknown as {
    _core: { _renderService: { dimensions: { css: { cell: { height: number } } } } };
  }
)._core._renderService.dimensions.css.cell.height;

const hcs = getComputedStyle(host);
const hostPadVer = parseFloat(hcs.paddingTop) + parseFloat(hcs.paddingBottom);
const contentHeight = hostRect.height - hostPadVer;

console.log("%c FIT ", "background:#e8e6e1;color:#000;font-weight:bold");
line("mode", FIXED ? "?fixed — padding on the pane" : "shipped — padding on the host");
line("cols x rows", `${term.cols} x ${term.rows}`);
line("cell height (css px)", cell.toFixed(3));
line("rows x cell", (term.rows * cell).toFixed(1));
line("host content height", contentHeight.toFixed(1));
line("rows that actually fit", String(Math.floor(contentHeight / cell)));
// The heart of it: what FitAddon reads is the *padding* box, because
// `box-sizing: border-box` makes the resolved `height` the border box — and the
// padding it subtracts is read from `.xterm`, which has none.
line("getComputedStyle(host).height", hcs.height);
line("padding on .xterm", getComputedStyle(q(".xterm")!).paddingTop);

// -------------------------------------------------------------- zoom
//
// **The two spaces `FitAddon` might mix, and the answer is that it does not.**
// `getComputedStyle(host).height` is a length in the *zoomed* coordinate space;
// `getBoundingClientRect()` reports viewport pixels, which include the zoom. If
// xterm measured its cell with the latter while the addon divided the former,
// every fit would be out by the zoom factor — a shortfall of
// `height x (1 - 1/zoom)`, invisible on a small pane and a thick band on a large
// one, which matched the report ("when I resize the window bigger it happens")
// exactly enough to be worth building this for.
//
// **Measured in WebKit at zoom 1.09: predicted 71.4px, actual 9.2px.** The two
// spaces agree; the residual is the ordinary sub-cell remainder. So a visible
// band is *not* explained by interface scale, and this section exists to keep
// anyone (including me, twice) from spending an afternoon on it again. Run it in
// Safari — Chrome's legacy `zoom` gives the opposite answer and would "confirm"
// the hypothesis.

console.log("%c ZOOM ", "background:#e8e6e1;color:#000;font-weight:bold");
line("--ui-zoom applied", ZOOM.toFixed(3));
const measuredZoom = hostRect.height / parseFloat(hcs.height);
line("rect / computed height", `${hostRect.height.toFixed(1)} / ${parseFloat(hcs.height).toFixed(1)} = ${measuredZoom.toFixed(3)}`);
const xtermRect = rects.get(".xterm");
if (xtermRect) {
  const shortfall = hostRect.height - xtermRect.height;
  line("host - .xterm (viewport px)", `${shortfall.toFixed(1)}`);
  line("predicted if spaces differ", `${(hostRect.height * (1 - 1 / ZOOM)).toFixed(1)}`);
}

// ------------------------------------------------- overflow or remainder?

let failures = 0;
const over = term.rows * cell - contentHeight;
if (over > 0.5) {
  failures++;
  console.error(
    `FAIL  fit() asked for ${term.rows} rows = ${(term.rows * cell).toFixed(1)}px into ${contentHeight.toFixed(1)}px ` +
      `— ${over.toFixed(1)}px more than the pane has`,
  );
} else {
  console.log(
    `%c PASS %c ${term.rows} rows = ${(term.rows * cell).toFixed(1)}px inside ${contentHeight.toFixed(1)}px ` +
      `(${(-over).toFixed(1)}px remainder, less than one cell)`,
    "background:#2b7;color:#000",
    "",
  );
}

for (const [name] of chain.slice(2)) {
  const r = rects.get(name);
  if (!r) continue;
  const past = r.bottom - paneRect.bottom;
  if (past > 0.5) {
    failures++;
    console.error(`FAIL  ${name} extends ${past.toFixed(1)}px below the pane`);
  }
}
if (!failures) {
  console.log(
    "%c PASS %c nothing overflows the pane",
    "background:#2b7;color:#000",
    "",
  );
}

// ------------------------------------------------------- who paints there?

// **This section answers less than it looks like it does, and that cost a day.**
// `elementsFromPoint` plus `getComputedStyle().backgroundColor` sees *elements*.
// It is blind to scrollbars, to `::before`/`::after`, and to anything a renderer
// draws into a canvas — which is precisely where a dark band can hide. Nine
// rounds of "nothing paints here" were read as evidence that nothing painted
// there, while the operator was looking straight at it.
//
// If this reports nothing and a band is visible, **stop reading properties and
// paint instead**: give every candidate in the chain a distinct colour and let
// the pixels name the owner. That test is what finally excluded this entire
// chain, and it took one round.
const probeY = paneRect.bottom - 2;
const probeX = paneRect.left + 40;
console.log("%c PAINTING IN THE BOTTOM STRIP ", "background:#e8e6e1;color:#000;font-weight:bold");
line("probe point", `${probeX.toFixed(0)}, ${probeY.toFixed(0)}`);
for (const el of document.elementsFromPoint(probeX, probeY)) {
  const cs = getComputedStyle(el);
  const cls = String((el as HTMLElement).className || "");
  const tag = `${el.tagName.toLowerCase()}${cls ? `.${cls.split(" ").join(".")}` : ""}`;
  const opaque = cs.backgroundColor !== "rgba(0, 0, 0, 0)" && cs.backgroundColor !== "transparent";
  console.log(
    `%c${opaque ? " PAINTS " : "        "}%c ${tag.slice(0, 56).padEnd(56)} ${cs.backgroundColor}`,
    opaque ? "background:#e63b2e;color:#000" : "",
    "",
  );
}

// ------------------------------------------------------- growing the window
//
// **The trigger the operator found: it appears when the window is made
// bigger.** That is a different fault from the one above and survives fixing
// it, so it gets its own reproduction.
//
// `?grow` fits at one size, then enlarges the column and refits — exactly what
// dragging the window edge does. Claude runs *inline*, so it does not repaint
// the whole screen the way a fullscreen TUI would; the rows that appear at the
// bottom are never written to. If those rows come back black rather than
// transparent, the renderer is showing an uninitialised buffer and no CSS
// anywhere can reach it.
//
// Screenshot the page to judge it — the body is a bright gradient on purpose,
// so anything opaque is obvious.
if (new URLSearchParams(location.search).has("grow")) {
  document.body.style.background = "linear-gradient(120deg,#7fb2d9,#cfa06a 60%,#8fc98f)";
  column.style.background = "rgba(20,20,20,0.55)";
  const before = { rows: term.rows, h: column.offsetHeight };
  column.style.height = "700px";
  await new Promise((r) => setTimeout(r, 60));
  fit.fit();
  await new Promise((r) => setTimeout(r, 400));
  console.log("%c AFTER GROWING ", "background:#e8e6e1;color:#000;font-weight:bold");
  line("column height", `${before.h} -> ${column.offsetHeight}`);
  line("rows", `${before.rows} -> ${term.rows}`);
  const c = host.querySelector("canvas") as HTMLCanvasElement;
  line("canvas css size", `${c.style.width} x ${c.style.height}`);
  line("canvas buffer size", `${c.width} x ${c.height}`);
  line("canvas rect h", c.getBoundingClientRect().height.toFixed(1));
  const marker = document.createElement("div");
  marker.textContent = "^ everything below the last line should be gradient, not black";
  marker.style.cssText = "color:#000;font:12px monospace;padding:4px";
  document.body.append(marker);
}

const viewport = q(".xterm-viewport");
if (viewport) {
  console.log("%c VIEWPORT SCROLL ", "background:#e8e6e1;color:#000;font-weight:bold");
  line("scrollHeight", String(viewport.scrollHeight));
  line("clientHeight", String(viewport.clientHeight));
  line("offsetHeight - clientHeight", String(viewport.offsetHeight - viewport.clientHeight));
  line("overflow-y", getComputedStyle(viewport).overflowY);
}
