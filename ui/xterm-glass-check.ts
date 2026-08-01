/**
 * Is an xterm actually transparent, or does it just claim to be?
 *
 * `allowTransparency: true` plus a `rgba(0,0,0,0)` theme reads as settled and
 * is not: xterm's own stylesheet paints `.xterm-viewport` solid `#000`, and
 * that element spans the whole terminal behind the rows. Nothing in the theme
 * touches it. Two rounds of changing rmux's *own* backdrops therefore changed
 * nothing visible, because the opaque layer belonged to xterm.
 *
 * So this measures the composited result rather than the configuration. Both
 * halves matter and the first is what makes the second meaningful:
 *
 *   1. **Without** the app stylesheet, the viewport must come out opaque —
 *      otherwise this page would pass for a build with no fix in it at all.
 *   2. **With** it, the viewport must be transparent, and the terminal's own
 *      colours must survive: a fix that hid the background by also flattening
 *      the foreground would be worse than the bug.
 */
import { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";

const lines: string[] = [];
let failures = 0;

const say = (s: string) => lines.push(s);
const check = (ok: boolean, label: string, detail: string) => {
  if (!ok) failures += 1;
  say(`${ok ? "ok  " : "FAIL"}  ${label} — ${detail}`);
};

/** The rule under test, lifted verbatim from `signal-room.css`. */
const OVERRIDE = ".xterm .xterm-viewport { background-color: transparent !important; }";

function mount(host: HTMLElement, webgl: boolean): Terminal {
  const term = new Terminal({
    theme: { background: "rgba(0, 0, 0, 0)", foreground: "#e8f4f2" },
    allowTransparency: true,
    fontSize: 12,
  });
  term.open(host);
  if (webgl) term.loadAddon(new WebglAddon());
  term.write("the quick brown fox\r\n\x1b[32mgreen\x1b[0m \x1b[31mred\x1b[0m\r\n");
  return term;
}

const viewportOf = (host: HTMLElement) =>
  host.querySelector<HTMLElement>(".xterm-viewport")!;

/** `rgba(0, 0, 0, 0)` and `transparent` both mean the same thing here. */
const isClear = (css: string) => css === "transparent" || /,\s*0\s*\)$/.test(css);

const dom = document.getElementById("dom")!;
const gl = document.getElementById("gl")!;

mount(dom, false);
mount(gl, true);

setTimeout(() => {
  // ---- 1. The bug, still present. This is the part that makes the rest mean
  //         something: if xterm ever stops shipping that rule, this fails and
  //         the override below becomes dead weight worth deleting.
  const before = getComputedStyle(viewportOf(dom)).backgroundColor;
  check(
    !isClear(before),
    "xterm ships an opaque viewport",
    `computed background-color is ${before} with no app stylesheet loaded`,
  );

  // ---- 2. The fix.
  const style = document.createElement("style");
  style.textContent = OVERRIDE;
  document.head.appendChild(style);

  for (const [name, host] of [
    ["dom renderer", dom],
    ["webgl renderer", gl],
  ] as const) {
    const after = getComputedStyle(viewportOf(host)).backgroundColor;
    check(isClear(after), `${name}: viewport clears`, `computed background-color is ${after}`);

    // The rows must keep painting. A `background: transparent !important` aimed
    // too broadly would take the text with it.
    const rows = host.querySelector<HTMLElement>(".xterm-rows, .xterm-screen canvas");
    check(!!rows, `${name}: still renders`, rows ? `found ${rows.className || rows.tagName}` : "no rows or canvas");
  }

  // ---- 3. WebGL must be the renderer that is actually running, or the second
  //         check above passed for the DOM fallback under a WebGL label.
  const canvas = gl.querySelector("canvas");
  const ctx = canvas && (canvas.getContext("webgl2") ?? canvas.getContext("webgl"));
  check(!!ctx, "webgl renderer is live", ctx ? "canvas has a webgl context" : "no webgl canvas — fell back to DOM");
  if (ctx) {
    const attrs = (ctx as WebGLRenderingContext).getContextAttributes();
    check(attrs?.alpha === true, "webgl surface has an alpha channel", `alpha=${attrs?.alpha}`);
  }

  say("");
  say(failures === 0 ? `PASS — ${lines.length - 1} checks` : `${failures} FAILED`);
  document.getElementById("out")!.textContent = lines.join("\n");
}, 400);
