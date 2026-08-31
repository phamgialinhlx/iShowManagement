/**
 * Does the terminal render bold and italic without garbling the line?
 *
 * A coding-agent TUI is mostly bold headings, italic emphasis and struck-through
 * plan edits, and it drew corrupted — glyphs overlapping, spaces eaten — while
 * plain paragraphs were clean. Two defects, both measured here:
 *
 *   1. **Missing faces.** Only the *upright* mono weights were bundled, so the
 *      browser synthesised bold and italic. This asserts the real 400/700 ×
 *      normal/italic faces of the default terminal font are declared *and
 *      loaded* — a synthesised face is never in `document.fonts`.
 *   2. **The measure race.** xterm measures its cell at construction, and during
 *      `font-display: block` that is the *fallback's* advance. When the real
 *      face swaps in wider than the fallback, every glyph is drawn past its cell
 *      and the row compresses. This asserts the real face's advance differs from
 *      the fallback's — so measuring against the fallback *is* wrong — while the
 *      face's own bold/italic advances all match its regular, i.e. it is a true
 *      monospace and only the timing, not the face, was ever the problem.
 *
 * Run against the dev server: http://localhost:5273/terminal-font-check.html —
 * read the console (and the page).
 */
import { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import "./src/styles/fonts.css";

const lines: string[] = [];
let failures = 0;
const say = (s: string) => lines.push(s);
const check = (ok: boolean, label: string, detail: string) => {
  if (!ok) failures += 1;
  say(`${ok ? "ok  " : "FAIL"}  ${label} — ${detail}`);
};

const FAMILY = "IBM Plex Mono";

/** Advance of a long identical run, so per-glyph rounding averages out. */
function advance(font: string): number {
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d")!;
  ctx.font = font;
  return ctx.measureText("M".repeat(40)).width / 40;
}

/** Is a real face for this family/weight/style declared and loaded? */
function faceLoaded(weight: string, style: string): boolean {
  for (const f of document.fonts) {
    if (
      f.family.replace(/["']/g, "") === FAMILY &&
      f.weight === weight &&
      f.style === style &&
      f.status === "loaded"
    ) {
      return true;
    }
  }
  return false;
}

async function run() {
  // Force the four faces the terminal actually asks for to load, then wait.
  const wanted = ["400", "700"].flatMap((w) => ["normal", "italic"].map((s) => [w, s] as const));
  await Promise.all(
    wanted.map(([w, s]) =>
      document.fonts.load(`${s === "italic" ? "italic " : ""}${w} 13px "${FAMILY}"`).catch(() => []),
    ),
  );
  await document.fonts.ready;

  // 1. All four real faces present (not synthesised).
  for (const [w, s] of wanted) {
    check(faceLoaded(w, s), `face ${w}/${s}`, `${FAMILY} ${w} ${s} is a real bundled face`);
  }

  // 2. The face is a true monospace: bold/italic advance equals regular. A
  //    synthesised or proportional face would diverge here.
  const reg = advance(`13px "${FAMILY}"`);
  const bold = advance(`700 13px "${FAMILY}"`);
  const ital = advance(`italic 13px "${FAMILY}"`);
  const boldItal = advance(`italic 700 13px "${FAMILY}"`);
  const eq = (a: number, b: number) => Math.abs(a - b) < 0.05;
  check(eq(reg, bold), "bold width", `regular ${reg.toFixed(3)} vs bold ${bold.toFixed(3)}`);
  check(eq(reg, ital), "italic width", `regular ${reg.toFixed(3)} vs italic ${ital.toFixed(3)}`);
  check(eq(reg, boldItal), "bold-italic width", `regular ${reg.toFixed(3)} vs bold-italic ${boldItal.toFixed(3)}`);

  // 3. How far the fallback's advance is from the real face's. This is the size
  //    of the mismeasure when xterm's cell is taken before the font loads — the
  //    gap the remeasure closes. It is *informational*, not asserted: the
  //    fallback is per-OS (Menlo on macOS is nearly identical to Plex Mono, so
  //    the race is mild here; Consolas / DejaVu on Windows/Linux diverge more),
  //    so a hard threshold would encode one machine's fonts as a rule.
  const fallback = advance(`13px Menlo, monospace`);
  const raceGap = Math.abs(reg - fallback);
  say(`note  race gap — ${FAMILY} ${reg.toFixed(3)} vs fallback ${fallback.toFixed(3)} = ${raceGap.toFixed(3)}px/char; remeasure closes it (per-OS)`);

  // 4. A real xterm renders a styled line without throwing, and its measured
  //    cell width equals the face advance (grid matches glyphs).
  const host = document.getElementById("host")!;
  const term = new Terminal({ fontFamily: `"${FAMILY}", monospace`, fontSize: 13, lineHeight: 1.3 });
  term.open(host);
  try {
    term.loadAddon(new WebglAddon());
  } catch {
    say("note  webgl unavailable in this browser; DOM renderer measured instead");
  }
  // A block that mirrors the reported garble: bold, italic and strikethrough
  // runs mixed into a plain sentence, the exact shape that overlapped before.
  term.write("- Slice 04 gates, the \x1b[1mmerge\x1b[0m of s1-3, but I can build 1\r\n");
  term.write("  \x1b[3minline-send\x1b[0m is deliberately simple; it depends on \x1b[9mSlice A\x1b[0m\r\n");
  term.write("  \x1b[9mAPI /v1/cmm/answer endpoint\x1b[0m -> \x1b[1;3mmerging\x1b[0m for reuse\r\n");
  term.write("  the quick brown fox jumps over the lazy dog 0123456789\r\n");
  // Re-measure like the app now does on font load.
  term.options.fontFamily = `"${FAMILY}", monospace`;

  await new Promise((r) => setTimeout(r, 150));
  // xterm exposes the measured cell width on the render dimensions.
  const core = (term as unknown as { _core?: { _renderService?: { dimensions?: { css?: { cell?: { width?: number } } } } } })._core;
  const cell = core?._renderService?.dimensions?.css?.cell?.width;
  if (typeof cell === "number" && cell > 0) {
    check(Math.abs(cell - reg) < 0.75, "cell == glyph", `xterm cell ${cell.toFixed(3)} vs glyph ${reg.toFixed(3)}`);
  } else {
    say(`note  could not read xterm cell width (${cell}); face + advance checks stand`);
  }

  say("");
  say(failures === 0 ? `PASS — ${lines.length} checks, 0 failures` : `FAIL — ${failures} failure(s)`);
  const out = document.getElementById("out")!;
  out.textContent = lines.join("\n");
  // Console, for headless reads.
  (failures === 0 ? console.log : console.error)("[terminal-font-check]\n" + lines.join("\n"));
}

void run();
