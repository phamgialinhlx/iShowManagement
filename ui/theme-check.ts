import {
  SIGNAL_ROOM,
  deriveTokens,
  terminalTheme,
  monacoTheme,
  BUILT_INS,
  SLOT_KEYS,
  slotVar,
  contrast,
  isLight,
  parseHex,
  DEFAULT_THEME,
  type Theme,
} from "./src/lib/theme";
import { TERMINAL_THEME } from "./src/lib/terminal-theme";
import "./src/styles/signal-room.css";

/**
 * Does the SIGNAL ROOM theme reproduce the app as it shipped?
 *
 * This is the safety net for "make the current colours the new theme". The whole
 * point of the ANSI theme system is that switching to it changes *nothing* on a
 * first run — so this harness derives the tokens and the terminal palette from
 * the `SIGNAL_ROOM` seed and asserts they match the hand-tuned values in
 * `signal-room.css` / `terminal-theme.ts`.
 *
 * Most tokens must be **exact** (the elevation ramp is +4/+8/+18 off #060606, the
 * borders are #e8e6e1 at fixed alphas). The two dim greys `--text-soft` /
 * `--text-faint` are allowed ~4 levels of drift, because they come from a
 * `color-mix` rather than a stored value — the identical tradeoff the shipped
 * `[data-custom-text]` rule already makes. Those two are checked against the
 * browser's *real* `color-mix` output, not the JS approximation, by applying the
 * derived vars to an element and reading `getComputedStyle`.
 *
 * Runs as a page (`theme-check.html`) so the `color-mix` check is the real
 * engine. `window.__themeCheck` resolves to the results for headless drivers.
 */

type Row = { name: string; got: string; want: string; ok: boolean };
const rows: Row[] = [];

/**
 * Canonicalise a colour string so equal colours compare equal regardless of
 * formatting — `rgba(230, 59, 46, 0.30)` and `rgba(230,59,46,0.3)` are the same
 * colour, and the shipped constants and the derived ones differ only in spacing
 * and a trailing zero.
 */
function norm(s: string | undefined): string {
  const t = (s ?? "").trim().toLowerCase();
  const m = t.match(/rgba?\(([^)]+)\)/);
  if (!m) return t.replace(/\s+/g, "");
  const parts = (m[1] ?? "").split(",").map((p) => String(Number(p.trim())));
  return `rgba(${parts.join(",")})`;
}

function expect(name: string, got: string | undefined, want: string) {
  rows.push({ name, got: got ?? "(missing)", want, ok: norm(got) === norm(want) });
}

// --- exact chrome tokens ----------------------------------------------------
const tok = deriveTokens(SIGNAL_ROOM);
expect("--primary", tok["--primary"], "230 59 46");
expect("--primary-soft", tok["--primary-soft"], "232 230 225");
expect("--busy", tok["--busy"], "242 168 60");
expect("--app-bg", tok["--app-bg"], "#060606");
expect("--app-panel", tok["--app-panel"], "#0a0a0a");
expect("--app-panel-2", tok["--app-panel-2"], "#0e0e0e");
expect("--app-elev", tok["--app-elev"], "#121212");
expect("--border", tok["--border"], "rgba(232, 230, 225, 0.14)");
expect("--border-strong", tok["--border-strong"], "rgba(232, 230, 225, 0.24)");
expect("--hover", tok["--hover"], "rgba(232, 230, 225, 0.055)");
expect("--text", tok["--text"], "#e8e6e1");
expect("--text-bright", tok["--text-bright"], "#ffffff");

// --- terminal palette matches the shipped constants -------------------------
const term = terminalTheme(SIGNAL_ROOM);
for (const key of Object.keys(TERMINAL_THEME) as (keyof typeof TERMINAL_THEME)[]) {
  // `white`/`foreground` intentionally follow Text (#e8e6e1); the shipped
  // `foreground` was a brighter #e8f4f2 that the design rules explicitly
  // corrected, so skip those two rather than assert the pre-fix value.
  if (key === "foreground" || key === "white") continue;
  expect(`term.${key}`, String(term[key]), String(TERMINAL_THEME[key]));
}

// --- the two mixed greys, via the browser's real color-mix ------------------
const probe = document.createElement("div");
for (const [k, v] of Object.entries(tok)) probe.style.setProperty(k, v);
document.body.appendChild(probe);

function resolved(varName: string): { r: number; g: number; b: number } {
  probe.style.color = `var(${varName})`;
  const css = getComputedStyle(probe).color;
  // WebKit answers `rgb(r, g, b)`; Chromium answers a `color-mix` result as
  // `color(srgb 0.59 0.58 0.56)` — fractions, which the old digit regex read as
  // huge integers and failed on.
  const srgb = css.match(/color\(srgb\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)/);
  if (srgb) {
    return {
      r: Math.round(Number(srgb[1]) * 255),
      g: Math.round(Number(srgb[2]) * 255),
      b: Math.round(Number(srgb[3]) * 255),
    };
  }
  const m = css.match(/\d+/g) ?? ["0", "0", "0"];
  return { r: Number(m[0]), g: Number(m[1]), b: Number(m[2]) };
}
function near(varName: string, wantHex: string, tol: number) {
  const got = resolved(varName);
  const n = parseInt(wantHex.replace("#", ""), 16);
  const want = { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
  const d = Math.max(
    Math.abs(got.r - want.r),
    Math.abs(got.g - want.g),
    Math.abs(got.b - want.b),
  );
  rows.push({
    name: `${varName} (≤${tol})`,
    got: `rgb(${got.r},${got.g},${got.b}) Δ${d}`,
    want: wantHex,
    ok: d <= tol,
  });
}
near("--text-soft", "#98958f", 4);
near("--text-faint", "#7e7b74", 5);

// --- every built-in derives without a parse failure -------------------------
for (const t of BUILT_INS) {
  const d = deriveTokens(t);
  const m = monacoTheme(t);
  const bad =
    Object.values(d).some((v) => v.includes("NaN")) ||
    m.rules.some((r) => /[^0-9a-fA-F]/.test(r.foreground)) ||
    m.rules.length === 0;
  rows.push({
    name: `built-in "${t.name}" derives cleanly`,
    got: bad ? "FAIL" : "ok",
    want: "ok",
    ok: !bad,
  });
}

// --- herdr-family: every built-in defines every slot; legibility floors -------
//
// Each built-in except SIGNAL ROOM (which derives its slots on purpose, to stay
// byte-identical to the old app) must state all nineteen herdr slots itself —
// a missing one would silently fall back to a derivation nobody chose. And the
// *published* tokens must clear WCAG on the theme background: `--text` at
// 4.5:1, `--subtext0` at 3:1, `--text-soft` 5.5:1, `--text-faint` 4.5:1.
const AGENT_STATES = ["--st-working", "--st-blocked", "--st-done", "--st-idle", "--st-unknown"];
for (const t of BUILT_INS) {
  const d = deriveTokens(t);
  if (t.name !== SIGNAL_ROOM.name) {
    const missing = SLOT_KEYS.filter((k) => !t.ui?.[k] || !parseHex(t.ui[k] ?? ""));
    rows.push({
      name: `${t.name}: all 19 slots stated`,
      got: missing.length ? `missing ${missing.join(",")}` : "ok",
      want: "ok",
      ok: missing.length === 0,
    });
  }
  const vars = [...SLOT_KEYS.map(slotVar), ...AGENT_STATES];
  const absent = vars.filter((v) => !d[v]);
  rows.push({
    name: `${t.name}: every slot var derived`,
    got: absent.length ? `absent ${absent.join(",")}` : "ok",
    want: "ok",
    ok: absent.length === 0,
  });
  const floor = (v: string, min: number) => {
    const value = d[v] ?? "";
    const c = parseHex(value) ? contrast(value, t.background) : NaN;
    rows.push({
      name: `${t.name}: ${v} ≥ ${min}:1`,
      got: Number.isNaN(c) ? `(mix) ${value}` : `${c.toFixed(2)}:1 ${value}`,
      want: `≥ ${min}`,
      // A slot-less theme's two dim greys are `color-mix` strings, measured
      // through the browser below for SIGNAL ROOM; skip the JS floor for them.
      ok: Number.isNaN(c) ? !t.ui : c >= min,
    });
  };
  floor("--text", 4.5);
  floor("--subtext0", 3);
  floor("--text-soft", 5.5);
  floor("--text-faint", 4.5);
  const raw = t.ui?.text;
  if (raw && raw !== d["--text"]) {
    rows.push({ name: `${t.name}: --text nudged for contrast`, got: `${raw} → ${d["--text"]}`, want: "(info)", ok: true });
  }
  const rawSub = t.ui?.subtext0;
  if (rawSub && rawSub !== d["--subtext0"]) {
    rows.push({ name: `${t.name}: --subtext0 nudged`, got: `${rawSub} → ${d["--subtext0"]}`, want: "(info)", ok: true });
  }
  // Appearance is stated, and agrees with the background's luminance.
  rows.push({
    name: `${t.name}: appearance ${t.appearance}`,
    got: isLight(t) ? "light" : "dark",
    want: t.appearance ?? "?",
    ok: (t.appearance === "light") === isLight({ ...t, appearance: undefined } as Theme),
  });
  // Monaco base follows the side.
  rows.push({
    name: `${t.name}: monaco base`,
    got: monacoTheme(t).base,
    want: isLight(t) ? "vs" : "vs-dark",
    ok: monacoTheme(t).base === (isLight(t) ? "vs" : "vs-dark"),
  });
}

// --- the default is Mocha, with herdr's exact slots ---------------------------
const mocha = deriveTokens(DEFAULT_THEME);
expect("default theme", DEFAULT_THEME.name, "Catppuccin Mocha");
expect("mocha --accent", mocha["--accent"], "#89b4fa");
expect("mocha --panel-bg", mocha["--panel-bg"], "#181825");
expect("mocha --surface0", mocha["--surface0"], "#313244");
expect("mocha --overlay0", mocha["--overlay0"], "#6c7086");
expect("mocha --text", mocha["--text"], "#cdd6f4");
expect("mocha --st-working", mocha["--st-working"], "#f9e2af");
expect("mocha --st-blocked", mocha["--st-blocked"], "#f38ba8");
expect("mocha --st-done", mocha["--st-done"], "#94e2d5");
expect("mocha --st-idle", mocha["--st-idle"], "#a6e3a1");
expect("mocha --app-bg", mocha["--app-bg"], "#1e1e2e");
expect("mocha --radius", mocha["--radius"], "4px");
expect("signal room --radius", tok["--radius"], "0px");
expect("signal room --accent", tok["--accent"], "#e63b2e");
expect("signal room --scroll-thumb", tok["--scroll-thumb"], "#242424");

// --- an old saved theme (no ui/appearance/radius) still derives everything ----
{
  const legacy: Theme = { ...SIGNAL_ROOM, name: "Old Save", appearance: undefined, radius: undefined };
  const d = deriveTokens(legacy);
  const absent = [...SLOT_KEYS.map(slotVar), ...AGENT_STATES].filter((v) => !d[v]);
  rows.push({
    name: "legacy saved theme derives all slots",
    got: absent.length ? absent.join(",") : "ok",
    want: "ok",
    ok: absent.length === 0 && d["--app-panel"] === "#0a0a0a",
  });
}

const pass = rows.every((r) => r.ok);
const out = document.getElementById("out");
if (out) {
  out.textContent =
    `VERDICT: ${pass ? "PASS" : "FAIL"}\n\n` +
    rows
      .map((r) => `${r.ok ? "ok  " : "FAIL"}  ${r.name.padEnd(28)} ${r.got}  (want ${r.want})`)
      .join("\n");
}
(window as unknown as { __themeCheck: unknown }).__themeCheck = { pass, rows };
