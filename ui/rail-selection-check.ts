/**
 * Can you see which session is selected?
 *
 * Open http://localhost:5273/rail-selection-check.html and read the console.
 *
 * This measures the **composited** row — the real computed CSS, alpha-blended
 * over the panel and then over a backdrop — rather than the declared colour.
 * Declared colours are what made the previous version look fine on paper: the
 * selected row was `--hover` (5.5% white) and an on-screen one 3%, which is a
 * difference right up until you put a photograph behind a translucent window,
 * at which point both are the wallpaper.
 *
 * Three backdrops, because the operator picks their own: a dark desktop, a
 * mid-tone photo, and a bright one. A selection cue that only works on one of
 * them is not a selection cue.
 */
// The stylesheet must be loaded, or every `var(--token)` resolves to the same
// inherited value and the hierarchy checks below compare a colour with itself —
// which is exactly how they first passed nothing and reported 12.15:1 twice.
import "./src/styles/signal-room.css";
import { rowSurface } from "./src/components/WorkspaceRail";

let failures = 0;
const check = (name: string, ok: boolean, detail: string) => {
  if (ok) console.log(`%c PASS %c ${name} — ${detail}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name} — ${detail}`);
  }
};

type Rgb = [number, number, number];

/** `src` over `dst`, straight alpha. */
function over(src: [number, number, number, number], dst: Rgb): Rgb {
  const a = src[3];
  return [0, 1, 2].map((i) => (src[i] ?? 0) * a + (dst[i] ?? 0) * (1 - a)) as Rgb;
}

function parse(css: string): [number, number, number, number] {
  const n = css.match(/[\d.]+/g)?.map(Number) ?? [];
  return [n[0] ?? 0, n[1] ?? 0, n[2] ?? 0, n[3] ?? 1];
}

/** Relative luminance, sRGB. */
function lum([r, g, b]: Rgb): number {
  const f = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

const contrast = (a: Rgb, b: Rgb) => {
  const la = lum(a);
  const lb = lum(b);
  const hi = Math.max(la, lb);
  const lo = Math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
};

/** Read a style object's background back out of the real cascade. */
function composited(style: { background: string }, panel: Rgb): Rgb {
  const el = document.createElement("div");
  el.style.background = style.background;
  document.body.append(el);
  const declared = getComputedStyle(el).backgroundColor;
  el.remove();
  return over(parse(declared), panel);
}

// The panel tint over each backdrop, roughly as the app composites it.
const BACKDROPS: { name: string; rgb: Rgb }[] = [
  { name: "dark desktop", rgb: [18, 18, 20] },
  { name: "mid-tone photo", rgb: [96, 104, 112] },
  { name: "bright photo", rgb: [198, 196, 190] },
];

const PANEL: [number, number, number, number] = [20, 20, 22, 0.72];

for (const { name, rgb } of BACKDROPS) {
  const panel = over(PANEL, rgb);

  const active = composited(rowSurface(true, true), panel);
  const onScreen = composited(rowSurface(false, true), panel);
  const plain = composited(rowSurface(false, false), panel);

  // The number that matters: selected vs the row directly above and below it.
  const vsOnScreen = contrast(active, onScreen);
  const vsPlain = contrast(active, plain);

  check(
    `selected stands out from an on-screen row · ${name}`,
    vsOnScreen >= 1.12,
    `contrast ${vsOnScreen.toFixed(3)} (the old 5.5%/3% pair measured ~1.03 here — invisible)`,
  );
  check(
    `selected stands out from an idle row · ${name}`,
    vsPlain >= 1.15,
    `contrast ${vsPlain.toFixed(3)}`,
  );
  check(
    `an on-screen row is still quieter than the selected one · ${name}`,
    lum(onScreen) < lum(active),
    "the two levels must not invert on a bright wallpaper",
  );
}

// The old values, kept as the thing this replaced. If someone reverts to them
// the harness should say so rather than silently passing.
{
  const panel = over(PANEL, [96, 104, 112]);
  const oldActive = over(parse("rgba(232, 230, 225, 0.055)"), panel);
  const oldOnScreen = over(parse("rgba(232, 230, 225, 0.03)"), panel);
  const was = contrast(oldActive, oldOnScreen);
  const now = contrast(
    composited(rowSurface(true, true), panel),
    composited(rowSurface(false, true), panel),
  );

  // Measured, not chosen: the old pair comes out at 1.071 on this backdrop.
  // The assertion is the *relationship* — how much of the separation above
  // parity the change actually bought — because an absolute threshold here
  // would just be a number picked to pass.
  const gained = (now - 1) / (was - 1);
  check(
    "red-then-green: the previous values were an order of magnitude flatter",
    was < 1.1 && gained >= 3,
    `was ${was.toFixed(3)}, now ${now.toFixed(3)} — ${gained.toFixed(1)}x the separation above parity`,
  );
}

check(
  "the selected row carries a heavier left bar",
  rowSurface(true, true).boxShadow.includes("3px") &&
    rowSurface(false, true).boxShadow.includes("2px"),
  "3px at full --text against 2px of --text-faint",
);

check(
  "selection is not carried by colour",
  !JSON.stringify(rowSurface(true, true)).includes("--primary"),
  "red is reserved for what needs acting on; selection is 'where am I'",
);

// --- the rail's text hierarchy ----------------------------------------------
//
// Three levels have to stay ordered, or the rail stops having structure:
// the group heading, the selected session, then everything else. The heading
// was `--text-soft` — identical to an unselected session name — so a folder
// read as just another row.

{
  const token = (name: string): Rgb => {
    const el = document.createElement("div");
    el.style.color = `var(${name})`;
    document.body.append(el);
    const c = parse(getComputedStyle(el).color);
    el.remove();
    return [c[0], c[1], c[2]];
  };

  const panel = over(PANEL, [96, 104, 112]);
  const strong = contrast(token("--text"), panel);
  const soft = contrast(token("--text-soft"), panel);

  check(
    "the group heading outranks an unselected session name",
    strong > soft,
    `heading --text at ${strong.toFixed(2)}:1 vs rows --text-soft at ${soft.toFixed(2)}:1`,
  );
  check(
    "…and both are still legible on their own",
    soft >= 4.5,
    `${soft.toFixed(2)}:1 — the dimmer of the two clears the 4.5:1 floor for body text`,
  );
}

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASSED ",
  `background:${failures ? "#e63b2e" : "#2b7"};color:#000;font-weight:bold`,
);
