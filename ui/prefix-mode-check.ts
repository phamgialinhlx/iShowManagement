/**
 * The prefix-key model's pure decisions (`src/lib/prefix-mode.ts`).
 *
 *     node --experimental-strip-types ui/prefix-mode-check.ts
 *
 * Every rule pinned here is a key that could otherwise vanish silently: the
 * prefix reaching the shell anyway, a second prefix eaten instead of sent, an
 * IME keystroke taken mid-word, a bare letter accepted as the prefix (and then
 * never typeable again). The real-xterm half — that the gate actually stops
 * `\x02` reaching `onData` — is `ui/prefix-terminal-check.ts`.
 */
import {
  decide,
  defaultConfig,
  normaliseToken,
  prefixBytes,
  prefixConflicts,
  resizeStep,
  tokenLabel,
  tokenOf,
  validatePrefix,
  wrapIndex,
  PREFIX_DEFAULTS,
  type KeyLike,
  type Mode,
} from "./src/lib/prefix-mode.ts";
import type { SplitNode } from "./src/lib/layout-tree.ts";

let failures = 0;
const lines: string[] = [];
function check(what: string, ok: boolean): void {
  if (!ok) failures += 1;
  lines.push(`${ok ? "PASS" : "FAIL"}  ${what}`);
}

const cfg = defaultConfig();
const k = (key: string, mods: Partial<KeyLike> = {}): KeyLike => ({ type: "keydown", key, ...mods });
const ctrlB = k("b", { ctrlKey: true, code: "KeyB" });

// ── tokens ───────────────────────────────────────────────────────────────────
check("ctrl+b spells Ctrl+b", tokenOf(ctrlB) === "Ctrl+b");
check("shift+n spells N (the character carries the shift)", tokenOf(k("N", { shiftKey: true })) === "N");
check("? spells ? whatever produced it", tokenOf(k("?", { shiftKey: true })) === "?");
check("shift+tab keeps Shift (no character to carry it)", tokenOf(k("Tab", { shiftKey: true })) === "Shift+Tab");
check("alt+b on macOS (∫) is read off code", tokenOf(k("∫", { altKey: true, code: "KeyB" })) === "Alt+b");
check("normalise: shift+n → N, ctrl+B → Ctrl+B, '-' stays", normaliseToken("shift+n") === "N" && normaliseToken("control+b") === "Ctrl+b" && normaliseToken("-") === "-");
check("label: Ctrl+b → ctrl+b, N → shift+n", tokenLabel("Ctrl+b") === "ctrl+b" && tokenLabel("N") === "shift+n");

// ── prefix validation ────────────────────────────────────────────────────────
check("a bare printable key is refused as the prefix", !validatePrefix("b").ok && !validatePrefix("?").ok);
check("shift alone is not a modifier", !validatePrefix("B").ok && !validatePrefix("Shift+Tab").ok);
check("ctrl+b, alt+a, a function key are accepted", validatePrefix("Ctrl+b").ok && validatePrefix("Alt+a").ok && validatePrefix("F12").ok);
check("ctrl+enter is refused (it still types \\r)", !validatePrefix("Ctrl+Enter").ok);

// ── transitions ──────────────────────────────────────────────────────────────
{
  const v = decide("terminal", ctrlB, cfg);
  check("terminal + prefix → PREFIX, consumed, no effect", v.mode === "prefix" && v.consume && v.effect === null);
}
{
  const v = decide("terminal", k("v"), cfg);
  check("terminal + v → passes through untouched", v.mode === "terminal" && !v.consume);
}
{
  const v = decide("prefix", ctrlB, cfg);
  check("prefix + prefix → back to terminal, NOT consumed, sendPrefix",
    v.mode === "terminal" && !v.consume && v.effect?.kind === "sendPrefix");
}
{
  const v = decide("prefix", k("Escape"), cfg);
  check("prefix + esc → terminal, consumed, nothing runs", v.mode === "terminal" && v.consume && v.effect === null);
}
{
  const v = decide("prefix", k("Shift", { shiftKey: true }), cfg);
  check("a modifier going down keeps PREFIX (so shift+n is reachable)", v.mode === "prefix" && v.consume);
  const n = decide("prefix", k("N", { shiftKey: true }), cfg);
  check("prefix + shift+n → project.new", n.effect?.kind === "run" && n.effect.action === "project.new" && n.mode === "terminal");
}
{
  const runs = (key: string, mods: Partial<KeyLike> = {}) => {
    const v = decide("prefix", k(key, mods), cfg);
    return v.effect?.kind === "run" ? v.effect.action : v.effect?.kind;
  };
  check("v / - / z / x / c → split right, split down, zoom, close pane, new tab",
    runs("v") === "pane.splitRight" && runs("-") === "pane.splitDown" && runs("z") === "pane.zoom" &&
    runs("x") === "pane.close" && runs("c") === "tab.new");
  check("tab / shift+tab → next / previous pane", runs("Tab") === "pane.next" && runs("Tab", { shiftKey: true }) === "pane.prev");
  check("H swaps left, h focuses left", runs("H", { shiftKey: true }) === "pane.swapLeft" && runs("h") === "pane.left");
  const three = decide("prefix", k("3"), cfg);
  check("prefix + 3 → tab index 2", three.effect?.kind === "selectTab" && three.effect.index === 2);
  const left = decide("prefix", k("ArrowLeft"), cfg);
  check("prefix + ← → focus left", left.effect?.kind === "focus" && left.effect.dir === "left");
  check("q, e and [ are unbound", runs("q") === undefined && runs("e") === undefined && runs("[") === undefined);
  const q = decide("prefix", k("q"), cfg);
  check("an unbound key ends PREFIX and is dropped, not typed", q.mode === "terminal" && q.consume);
}
{
  const nav = decide("prefix", k("w"), cfg);
  check("prefix + w → NAVIGATE", nav.mode === "navigate");
  const step = (mode: Mode, key: string) => decide(mode, k(key), cfg);
  check("navigate: j/↓ move +1, k/↑ move -1, mode persists",
    step("navigate", "j").effect?.kind === "navMove" && step("navigate", "ArrowUp").mode === "navigate" &&
    (step("navigate", "k").effect as { delta: number }).delta === -1);
  check("navigate: an unrelated key is taken and the mode persists",
    step("navigate", "x").mode === "navigate" && step("navigate", "x").consume);
  check("navigate: enter selects and leaves", step("navigate", "Enter").effect?.kind === "navSelect" && step("navigate", "Enter").mode === "terminal");
  check("navigate: esc leaves", step("navigate", "Escape").mode === "terminal");
  const res = decide("prefix", k("r"), cfg);
  check("prefix + r → RESIZE", res.mode === "resize");
  const l = step("resize", "l");
  check("resize: l → resize right, mode persists", l.effect?.kind === "resize" && l.effect.dir === "right" && l.mode === "resize");
  check("resize: enter and esc leave", step("resize", "Enter").mode === "terminal" && step("resize", "Escape").mode === "terminal");
  check("resize: the prefix re-enters PREFIX", decide("resize", ctrlB, cfg).mode === "prefix");
}
{
  const cmdV = decide("prefix", k("v", { metaKey: true }), cfg);
  check("⌘ chords pass through a mode untouched (copy/paste survive)", !cmdV.consume && cmdV.mode === "prefix");
}

// ── IME ──────────────────────────────────────────────────────────────────────
for (const mode of ["terminal", "prefix", "navigate", "resize"] as Mode[]) {
  const a = decide(mode, k("b", { ctrlKey: true, isComposing: true }), cfg);
  const b = decide(mode, k("v", { keyCode: 229 }), cfg);
  const c = decide(mode, k("Process"), cfg);
  check(`composition is never intercepted (${mode})`, !a.consume && !b.consume && !c.consume && a.mode === mode);
}

// ── literal prefix bytes ─────────────────────────────────────────────────────
check("ctrl+b → \\x02, ctrl+a → \\x01, alt+a → ESC a", prefixBytes("Ctrl+b") === "\x02" && prefixBytes("Ctrl+a") === "\x01" && prefixBytes("Alt+a") === "\x1ba");
check("a ⌘ prefix has no bytes", prefixBytes("Meta+b") === null);

// ── conflicts ────────────────────────────────────────────────────────────────
check("the default table has no conflicts", prefixConflicts(PREFIX_DEFAULTS, cfg.prefixes).length === 0);
check("two actions on one key are reported",
  prefixConflicts({ ...PREFIX_DEFAULTS, "pane.zoom": "v" }, cfg.prefixes).some((ids) => ids.includes("pane.zoom") && ids.includes("pane.splitRight")));
check("a binding on a fixed digit is reported", prefixConflicts({ ...PREFIX_DEFAULTS, "pane.zoom": "3" }, cfg.prefixes).some((ids) => ids.includes("pane.zoom")));

// ── resize math ──────────────────────────────────────────────────────────────
{
  const leaf = (id: string): SplitNode => ({ kind: "leaf", id, pane: { kind: "session", id } });
  // [A | [B / C]] — row split s1 at 0.5, col split s2 at 0.5 inside the right half.
  const tree: SplitNode = {
    kind: "split", id: "s1", dir: "row", ratio: 0.5, a: leaf("A"),
    b: { kind: "split", id: "s2", dir: "col", ratio: 0.5, a: leaf("B"), b: leaf("C") },
  };
  const r1 = resizeStep(tree, "A", "right");
  check("l on the left pane moves the row divider +5%", r1?.splitId === "s1" && r1.ratio === 0.55);
  const r2 = resizeStep(tree, "C", "left");
  check("h on a nested pane finds the nearest row ancestor (skipping the col split)", r2?.splitId === "s1" && r2.ratio === 0.45);
  const r3 = resizeStep(tree, "B", "down");
  check("j moves the nearest col divider", r3?.splitId === "s2" && r3.ratio === 0.55);
  check("no split in that axis → nothing", resizeStep(tree, "A", "up") === null);
  const pinned: SplitNode = { ...(tree as Extract<SplitNode, { kind: "split" }>), ratio: 0.9 };
  check("at the clamp → nothing (no no-op write)", resizeStep(pinned, "A", "right") === null);
  const near: SplitNode = { ...(tree as Extract<SplitNode, { kind: "split" }>), ratio: 0.88 };
  check("a step past the clamp stops at 0.9", resizeStep(near, "A", "right")?.ratio === 0.9);
  let ratio = 0.5;
  for (let i = 0; i < 5; i += 1) {
    const t: SplitNode = { ...(tree as Extract<SplitNode, { kind: "split" }>), ratio };
    ratio = resizeStep(t, "A", "right")!.ratio;
  }
  check("five steps land on a clean 0.75", ratio === 0.75);
}
check("wrapIndex cycles both ways", wrapIndex(-1, 3) === 2 && wrapIndex(3, 3) === 0);

lines.push("");
lines.push(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
console.log(lines.join("\n"));
if (failures) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
