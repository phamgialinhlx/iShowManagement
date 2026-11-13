import type { Direction, SplitNode } from "./layout-tree.ts";

/**
 * herdr's prefix-key keyboard model — the **pure** half.
 *
 * Press the prefix (default `ctrl+b`), and the *next* key is a command rather
 * than input: `v` splits right, `z` zooms, `w` enters navigate mode. This is
 * the layer that lets rmux bind bare letters without stealing them from the
 * shell — the letter is only a command for one keystroke after a chord the
 * operator pressed on purpose. The rebindable `Mod` chords in `shortcuts.ts`
 * stay exactly as they were; this is an additional layer, not a replacement.
 *
 * ## Why this file decides and does nothing
 *
 * Every rule that can silently eat a key lives here, where
 * `ui/prefix-mode-check.ts` can pin it under plain Node: the prefix is
 * detected, a second prefix is handed back to the terminal as itself, Escape
 * cancels, an IME composition is **never** intercepted, and a bare printable
 * key is refused as a prefix. `prefix-runtime.ts` owns the DOM (xterm's custom
 * key handler, the window listener) and the effects (the store calls).
 *
 * Type-only import from `layout-tree.ts` plus its pure functions, like the
 * store's other pure modules, so the check never boots Zustand.
 */

// ── keys ─────────────────────────────────────────────────────────────────────

/** The fields of a `KeyboardEvent` the decision reads — a plain object in tests. */
export type KeyLike = {
  type?: string;
  key: string;
  code?: string;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  metaKey?: boolean;
  isComposing?: boolean;
  keyCode?: number;
};

const MODIFIER_KEYS = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph", "CapsLock", "OS"]);

/** A key press that is only a modifier going down — part of a chord, not one. */
export function isModifierOnly(e: KeyLike): boolean {
  return MODIFIER_KEYS.has(e.key);
}

/**
 * The token a key press is spelled as in the prefix table.
 *
 * herdr's spelling, and the one a person writes: `v`, `N` (shift+n), `?`, `-`,
 * `Tab`, `Shift+Tab`, `ArrowLeft`, `Ctrl+b`.
 *
 * - **A printable key is its character.** Shift is *not* spelled out for it,
 *   because the character already says it: `?` is shift+/ on one layout and
 *   something else on the next, and `N` is shift+n everywhere. Spelling
 *   `Shift+/` would make `?` unbindable on half the keyboards in use.
 * - **A named key keeps Shift**, because there is no character to carry it —
 *   `Shift+Tab` is not `Tab`.
 * - Ctrl, Alt and Meta are always spelled, in that fixed order, so two
 *   spellings of one chord compare equal.
 * - **Alt on macOS types a different character** (`⌥b` is `∫`), so with Alt
 *   held a letter is read off `code` instead — otherwise `alt+b` could be
 *   captured but never matched again.
 */
export function tokenOf(e: KeyLike): string {
  let key = e.key;
  if (e.altKey && e.code && /^Key[A-Z]$/.test(e.code)) {
    const letter = e.code.slice(3);
    key = e.shiftKey ? letter : letter.toLowerCase();
  } else if (e.altKey && e.code && /^Digit[0-9]$/.test(e.code)) {
    key = e.code.slice(5);
  }
  const printable = key.length === 1;
  const mods: string[] = [];
  if (e.metaKey) mods.push("Meta");
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey && !printable) mods.push("Shift");
  // Ctrl+Shift+b reports `B`; keep the case, it is the shift.
  return [...mods, key === " " ? "Space" : key].join("+");
}

/** Canonical spelling of a stored token (`ctrl+B` → `Ctrl+B`, `shift+n` → `N`). */
export function normaliseToken(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return "";
  // A lone `+` or `-` is a key, not a separator.
  if (trimmed.length === 1) return trimmed;
  const parts = trimmed.split("+");
  let key = parts.pop() ?? "";
  if (key === "" && trimmed.endsWith("+")) key = "+";
  const mods = new Set(parts.map((p) => p.trim().toLowerCase()).filter(Boolean));
  const out: string[] = [];
  if (mods.has("meta") || mods.has("cmd") || mods.has("super")) out.push("Meta");
  if (mods.has("ctrl") || mods.has("control")) out.push("Ctrl");
  if (mods.has("alt") || mods.has("option")) out.push("Alt");
  const lower = key.toLowerCase();
  const named: Record<string, string> = {
    tab: "Tab", enter: "Enter", esc: "Escape", escape: "Escape", space: "Space",
    left: "ArrowLeft", right: "ArrowRight", up: "ArrowUp", down: "ArrowDown",
    arrowleft: "ArrowLeft", arrowright: "ArrowRight", arrowup: "ArrowUp", arrowdown: "ArrowDown",
    backspace: "Backspace", home: "Home", end: "End", pageup: "PageUp", pagedown: "PageDown",
  };
  if (key.length === 1) {
    out.push(mods.has("shift") && /[a-z]/i.test(key) ? key.toUpperCase() : key);
  } else {
    if (mods.has("shift")) out.push("Shift");
    out.push(named[lower] ?? (/^f([1-9]|1[0-2])$/.test(lower) ? lower.toUpperCase() : key));
  }
  return out.join("+");
}

/** herdr's display spelling: `ctrl+b`, `shift+n`, `?`, `shift+tab`, `←`. */
export function tokenLabel(token: string): string {
  const norm = normaliseToken(token);
  if (!norm) return "—";
  const parts = norm.length === 1 ? [norm] : norm.split("+");
  let key = parts.pop() ?? "";
  if (key === "" ) key = "+";
  const mods = parts.map((m) => m.toLowerCase());
  const arrows: Record<string, string> = { ArrowLeft: "←", ArrowRight: "→", ArrowUp: "↑", ArrowDown: "↓" };
  let pretty = arrows[key] ?? (key.length === 1 ? key : key.toLowerCase());
  if (key.length === 1 && /[A-Z]/.test(key)) {
    mods.push("shift");
    pretty = key.toLowerCase();
  }
  if (key === "Escape") pretty = "esc";
  return [...mods, pretty].join("+");
}

/**
 * Whether a token may be a prefix.
 *
 * **A bare printable key is refused**, with the reason: the prefix is a key the
 * terminal never sees on first press, so a prefix of `b` would make the letter
 * b untypeable in every shell, editor and Claude prompt. Shift alone is not a
 * modifier for the same reason it is not one in `shortcuts.ts` — `B` is just a
 * capital. A function key is allowed bare: it types nothing.
 */
export function validatePrefix(token: string): { ok: true } | { ok: false; reason: string } {
  const norm = normaliseToken(token);
  if (!norm) return { ok: false, reason: "No key." };
  const parts = norm.length === 1 ? [norm] : norm.split("+");
  const key = parts[parts.length - 1] ?? "";
  const mods = new Set(parts.slice(0, -1));
  if (mods.has("Ctrl") || mods.has("Alt") || mods.has("Meta")) {
    if (["Escape", "Enter", "Tab"].includes(key)) {
      return { ok: false, reason: `${tokenLabel(norm)} still types into the terminal — pick a letter chord.` };
    }
    return { ok: true };
  }
  if (/^F([1-9]|1[0-2])$/.test(key)) return { ok: true };
  return {
    ok: false,
    reason: `${tokenLabel(norm)} needs ctrl, alt or ⌘ — a bare key as the prefix could never be typed into a terminal again`,
  };
}

/**
 * The bytes a prefix chord would put on the wire, for "send prefix" when the
 * keyboard is *not* in a terminal (the rail has focus). Inside a terminal the
 * key is simply handed back to xterm, which encodes it itself — this is only the
 * fallback, so it covers the shapes a prefix realistically takes and returns
 * `null` for the rest rather than guessing.
 */
export function prefixBytes(token: string): string | null {
  const norm = normaliseToken(token);
  const parts = norm.split("+");
  const key = parts.pop() ?? "";
  const mods = new Set(parts);
  if (mods.has("Meta")) return null;
  let base: string | null = key.length === 1 ? key : key === "Space" ? " " : null;
  if (base === null) return null;
  if (mods.has("Ctrl")) {
    const c = base.toUpperCase().charCodeAt(0);
    if (c >= 0x40 && c <= 0x5f) base = String.fromCharCode(c & 0x1f);
    else if (base === " " || base === "2") base = "\x00";
    else if (base === "?") base = "\x7f";
    else return null;
  }
  return mods.has("Alt") ? `\x1b${base}` : base;
}

// ── the table ────────────────────────────────────────────────────────────────

export type PrefixActionId =
  | "help"
  | "settings"
  | "sidebar"
  | "jumpLatest"
  | "project.new"
  | "project.rename"
  | "project.close"
  | "mode.navigate"
  | "goto"
  | "tab.new"
  | "tab.rename"
  | "tab.close"
  | "tab.prev"
  | "tab.next"
  | "pane.left"
  | "pane.down"
  | "pane.up"
  | "pane.right"
  | "pane.swapLeft"
  | "pane.swapDown"
  | "pane.swapUp"
  | "pane.swapRight"
  | "pane.next"
  | "pane.prev"
  | "pane.splitRight"
  | "pane.splitDown"
  | "pane.close"
  | "pane.zoom"
  | "mode.resize"
  | "pane.rename"
  | "tile.openBeside"
  | "tile.close";

export type PrefixAction = { id: PrefixActionId; label: string; group: "Workspace" | "Tabs" | "Panes" | "Modes" };

/** herdr's table, in its order. `q`, `e` and `[` are deliberately absent — see the report in `prefix-runtime.ts`. */
export const PREFIX_ACTIONS: readonly PrefixAction[] = [
  { id: "help", label: "Keybinds", group: "Workspace" },
  { id: "settings", label: "Settings", group: "Workspace" },
  { id: "sidebar", label: "Toggle sidebar", group: "Workspace" },
  { id: "jumpLatest", label: "Jump to the latest notification", group: "Workspace" },
  { id: "project.new", label: "New project", group: "Workspace" },
  { id: "project.rename", label: "Rename project", group: "Workspace" },
  { id: "project.close", label: "Close project (asks; ends its sessions)", group: "Workspace" },
  { id: "goto", label: "Go to… (quick switcher)", group: "Workspace" },
  { id: "tab.new", label: "New tab", group: "Tabs" },
  { id: "tab.rename", label: "Rename tab", group: "Tabs" },
  { id: "tab.close", label: "Close tab (asks)", group: "Tabs" },
  { id: "tab.prev", label: "Previous tab", group: "Tabs" },
  { id: "tab.next", label: "Next tab", group: "Tabs" },
  { id: "pane.left", label: "Focus pane left", group: "Panes" },
  { id: "pane.down", label: "Focus pane down", group: "Panes" },
  { id: "pane.up", label: "Focus pane up", group: "Panes" },
  { id: "pane.right", label: "Focus pane right", group: "Panes" },
  { id: "pane.swapLeft", label: "Swap with pane left", group: "Panes" },
  { id: "pane.swapDown", label: "Swap with pane down", group: "Panes" },
  { id: "pane.swapUp", label: "Swap with pane up", group: "Panes" },
  { id: "pane.swapRight", label: "Swap with pane right", group: "Panes" },
  { id: "pane.next", label: "Next pane", group: "Panes" },
  { id: "pane.prev", label: "Previous pane", group: "Panes" },
  { id: "pane.splitRight", label: "Split right", group: "Panes" },
  { id: "pane.splitDown", label: "Split down", group: "Panes" },
  { id: "pane.close", label: "Close pane (asks)", group: "Panes" },
  { id: "pane.zoom", label: "Zoom pane", group: "Panes" },
  { id: "pane.rename", label: "Rename pane (its session)", group: "Panes" },
  // The project grid — rmux's, not herdr's. Shift on both so the lower-case
  // letters keep herdr's meaning (`v` splits, `q` stays unbound).
  { id: "tile.openBeside", label: "Open the next project beside", group: "Workspace" },
  { id: "tile.close", label: "Close this tile (sessions keep running)", group: "Workspace" },
  { id: "mode.navigate", label: "Navigate mode", group: "Modes" },
  { id: "mode.resize", label: "Resize mode", group: "Modes" },
];

export const PREFIX_DEFAULTS: Record<PrefixActionId, string> = {
  help: "?",
  settings: "s",
  sidebar: "b",
  jumpLatest: "o",
  "project.new": "N",
  "project.rename": "W",
  "project.close": "D",
  "mode.navigate": "w",
  goto: "g",
  "tab.new": "c",
  "tab.rename": "T",
  "tab.close": "X",
  "tab.prev": "p",
  "tab.next": "n",
  "pane.left": "h",
  "pane.down": "j",
  "pane.up": "k",
  "pane.right": "l",
  "pane.swapLeft": "H",
  "pane.swapDown": "J",
  "pane.swapUp": "K",
  "pane.swapRight": "L",
  "pane.next": "Tab",
  "pane.prev": "Shift+Tab",
  "pane.splitRight": "v",
  "pane.splitDown": "-",
  "pane.close": "x",
  "pane.zoom": "z",
  "mode.resize": "r",
  "pane.rename": "P",
  "tile.openBeside": "V",
  "tile.close": "Q",
};

/**
 * Fixed aliases, not rebindable: arrows always focus (herdr's `h/j/k/l and
 * arrows`), digits always pick a tab. Listed in the help overlay so they are
 * discoverable; kept out of the rebind list because nine "tab N" rows would bury
 * the bindings anyone actually changes.
 */
export const ARROW_FOCUS: Record<string, Direction> = {
  ArrowLeft: "left",
  ArrowDown: "down",
  ArrowUp: "up",
  ArrowRight: "right",
};

export type PrefixConfig = {
  /** One or more prefix chords. More than one is allowed — herdr's `prefix` list. */
  prefixes: string[];
  bindings: Record<PrefixActionId, string>;
};

export const DEFAULT_PREFIXES = ["Ctrl+b"];

export function defaultConfig(): PrefixConfig {
  return { prefixes: [...DEFAULT_PREFIXES], bindings: { ...PREFIX_DEFAULTS } };
}

/** Prefix-table actions sharing one key — marked in settings, not prevented. */
export function prefixConflicts(bindings: Record<PrefixActionId, string>, prefixes: string[] = []): PrefixActionId[][] {
  const by = new Map<string, PrefixActionId[]>();
  for (const a of PREFIX_ACTIONS) {
    const t = normaliseToken(bindings[a.id] ?? "");
    if (!t) continue;
    by.set(t, [...(by.get(t) ?? []), a.id]);
  }
  const out = [...by.values()].filter((ids) => ids.length > 1);
  // A binding equal to a prefix can never run: the second prefix press sends
  // the prefix itself. Reported as a one-action "conflict" so it is marked.
  const pre = new Set(prefixes.map(normaliseToken));
  for (const a of PREFIX_ACTIONS) {
    if (pre.has(normaliseToken(bindings[a.id] ?? ""))) out.push([a.id]);
  }
  // Digits and arrows are fixed aliases; a binding on one is shadowed.
  for (const a of PREFIX_ACTIONS) {
    const t = normaliseToken(bindings[a.id] ?? "");
    if (/^[1-9]$/.test(t) || t in ARROW_FOCUS) out.push([a.id]);
  }
  return out;
}

const KEY = "rmux.prefix";

export function readPrefixConfig(): PrefixConfig {
  const out = defaultConfig();
  try {
    if (typeof localStorage === "undefined") return out;
    const raw = localStorage.getItem(KEY);
    if (!raw) return out;
    const stored = JSON.parse(raw) as Partial<{ prefixes: unknown; bindings: Record<string, unknown> }>;
    if (Array.isArray(stored.prefixes)) {
      const valid = stored.prefixes
        .filter((p): p is string => typeof p === "string")
        .map(normaliseToken)
        .filter((p) => validatePrefix(p).ok);
      // An empty list would leave no way in at all; fall back rather than strand.
      if (valid.length) out.prefixes = [...new Set(valid)];
    }
    for (const a of PREFIX_ACTIONS) {
      const v = stored.bindings?.[a.id];
      // Absent → default, the same reconciliation `readBindings` does for an
      // action added in a later version.
      if (typeof v === "string") out.bindings[a.id] = normaliseToken(v);
    }
  } catch {
    /* corrupt → defaults */
  }
  return out;
}

export function writePrefixConfig(config: PrefixConfig): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(config));
    window.dispatchEvent(new CustomEvent("rmux:prefix-changed"));
  } catch {
    /* full storage must not break rebinding */
  }
}

// ── the state machine ────────────────────────────────────────────────────────

export type Mode = "terminal" | "prefix" | "navigate" | "resize";

export type Effect =
  | { kind: "run"; action: PrefixActionId }
  | { kind: "sendPrefix"; token: string }
  | { kind: "selectTab"; index: number }
  | { kind: "focus"; dir: Direction }
  | { kind: "navMove"; delta: number }
  | { kind: "navSelect" }
  | { kind: "resize"; dir: Direction };

export type Verdict = {
  /** The mode after this key. */
  mode: Mode;
  /**
   * Whether the key is taken from whatever would otherwise receive it. `false`
   * means "not ours" — the terminal (or browser) handles it as normal. The one
   * case with an effect *and* `consume: false` is `sendPrefix`: inside a
   * terminal the key is simply given back to xterm, which encodes it exactly as
   * it would have without the prefix layer.
   */
  consume: boolean;
  effect: Effect | null;
};

const PASS = (mode: Mode): Verdict => ({ mode, consume: false, effect: null });
const TAKE = (mode: Mode, effect: Effect | null = null): Verdict => ({ mode, consume: true, effect });

/** Whether an event is part of an input-method composition. Never intercepted. */
export function isComposition(e: KeyLike): boolean {
  // 229 is what every browser reports for a key the IME is holding — the
  // composition flag is not always set on the first one.
  return !!e.isComposing || e.keyCode === 229 || e.key === "Process";
}

export function isPrefix(e: KeyLike, prefixes: string[]): boolean {
  const t = tokenOf(e);
  return prefixes.some((p) => normaliseToken(p) === t);
}

/**
 * Decide what one `keydown` does.
 *
 * Only keydown: keyup and keypress are never *decided*. The runtime swallows
 * the keyup/keypress of a key whose keydown it consumed, so nothing leaks — but
 * the decision is made once, here.
 */
export function decide(mode: Mode, e: KeyLike, config: PrefixConfig): Verdict {
  // **An IME composition is never ours**, in any mode. Marked text is produced
  // above the DOM (see `ime-replace.ts`); taking one of its keys would split a
  // Vietnamese word across a command.
  if (isComposition(e)) return PASS(mode);

  // A modifier going down is a chord in progress. In a mode it must not count
  // as "the next key", or shift+n could never be typed after the prefix.
  if (isModifierOnly(e)) return mode === "terminal" ? PASS(mode) : TAKE(mode);

  const token = tokenOf(e);
  const prefixHit = config.prefixes.some((p) => normaliseToken(p) === token);

  if (mode === "terminal") return prefixHit ? TAKE("prefix") : PASS("terminal");

  // ⌘ chords pass through every mode untouched and leave it as it is: ⌘C, ⌘V
  // and the app menu are the platform's, and a mode must not break copy/paste.
  if (e.metaKey && !prefixHit) return PASS(mode);

  if (mode === "prefix") {
    if (e.key === "Escape") return TAKE("terminal");
    // herdr: "<prefix> <prefix> sends the prefix". Handed back, not consumed.
    if (prefixHit) return { mode: "terminal", consume: false, effect: { kind: "sendPrefix", token } };
    if (/^[1-9]$/.test(token)) return TAKE("terminal", { kind: "selectTab", index: Number(token) - 1 });
    const arrow = ARROW_FOCUS[token];
    if (arrow) return TAKE("terminal", { kind: "focus", dir: arrow });
    const action = PREFIX_ACTIONS.find((a) => normaliseToken(config.bindings[a.id] ?? "") === token);
    // An unbound key ends the prefix and is dropped — tmux's behaviour. Passing
    // it on would type a letter the operator meant as a command.
    if (!action) return TAKE("terminal");
    if (action.id === "mode.navigate") return TAKE("navigate", { kind: "run", action: action.id });
    if (action.id === "mode.resize") return TAKE("resize", { kind: "run", action: action.id });
    return TAKE("terminal", { kind: "run", action: action.id });
  }

  // Navigate and resize are persistent: everything is taken, and only Escape,
  // Enter or the prefix leaves.
  if (prefixHit) return TAKE("prefix");
  if (e.key === "Escape") return TAKE("terminal");

  if (mode === "navigate") {
    if (e.key === "Enter") return TAKE("terminal", { kind: "navSelect" });
    if (token === "ArrowUp" || token === "k") return TAKE("navigate", { kind: "navMove", delta: -1 });
    if (token === "ArrowDown" || token === "j") return TAKE("navigate", { kind: "navMove", delta: 1 });
    if (token === "ArrowLeft" || token === "h") return TAKE("navigate", { kind: "focus", dir: "left" });
    if (token === "ArrowRight" || token === "l") return TAKE("navigate", { kind: "focus", dir: "right" });
    return TAKE("navigate");
  }

  // resize
  if (e.key === "Enter") return TAKE("terminal");
  const dir: Direction | undefined =
    ARROW_FOCUS[token] ?? ({ h: "left", j: "down", k: "up", l: "right" } as Record<string, Direction>)[token];
  if (dir) return TAKE("resize", { kind: "resize", dir });
  return TAKE("resize");
}

// ── resize math ──────────────────────────────────────────────────────────────

export const RESIZE_STEP = 0.05;

/**
 * Which divider a resize key moves, and to where.
 *
 * The **nearest ancestor split in that axis** — `row` for left/right, `col` for
 * up/down — moved by `step` **in the direction of the key** (tmux's
 * `resize-pane -L/-R/-U/-D`, and herdr's): `l` pushes the divider right, which
 * grows a pane on its left and shrinks one on its right. Moving the divider
 * rather than "growing the focused pane" means `l` then `h` always returns to
 * where you started, whichever side of the split you are on.
 *
 * `null` when there is no split in that axis or the ratio is already at its
 * clamp — the caller does nothing, as the focus keys do at an edge.
 */
export function resizeStep(
  root: SplitNode,
  leafId: string,
  dir: Direction,
  step = RESIZE_STEP,
  bounds: { min: number; max: number } = { min: 0.1, max: 0.9 },
): { splitId: string; ratio: number } | null {
  const axis = dir === "left" || dir === "right" ? "row" : "col";
  const path: SplitNode[] = [];
  const find = (n: SplitNode): boolean => {
    if (n.kind === "leaf") return n.id === leafId;
    path.push(n);
    if (find(n.a) || find(n.b)) return true;
    path.pop();
    return false;
  };
  if (!find(root)) return null;
  for (let i = path.length - 1; i >= 0; i -= 1) {
    const s = path[i]!;
    if (s.kind !== "split" || s.dir !== axis) continue;
    const delta = dir === "right" || dir === "down" ? step : -step;
    // Rounded so five presses of 5% land on a clean number rather than 0.7500000001.
    const next = Math.round(Math.min(bounds.max, Math.max(bounds.min, s.ratio + delta)) * 1000) / 1000;
    if (Math.abs(next - s.ratio) < 1e-9) return null;
    return { splitId: s.id, ratio: next };
  }
  return null;
}

/** Wrap an index into `0..n-1` — navigate mode cycles through the spaces. */
export function wrapIndex(i: number, n: number): number {
  if (n <= 0) return 0;
  return ((i % n) + n) % n;
}
