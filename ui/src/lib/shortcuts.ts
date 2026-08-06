/**
 * Keyboard shortcuts — the model, the storage, and the matching.
 *
 * ## The governing constraint: a terminal wants every key
 *
 * rmux is mostly two full-screen programs that read raw keystrokes. Anything
 * this file swallows is a key that never reaches a shell or Claude, and the
 * failure is silent and maddening — you press something, the far side does
 * nothing, and there is no message anywhere saying why. So:
 *
 *  - **A binding without a modifier is refused.** Not discouraged: refused, at
 *    the point of binding, with a reason. A bare `f` would make the letter f
 *    unusable in vim.
 *  - **Shift alone does not count as a modifier**, because `Shift+a` is just
 *    `A` and stealing capitals is the same bug wearing a hat.
 *  - **Nothing fires while a text field has focus.** Not the terminal — xterm's
 *    hidden textarea *is* a text field, and exempting it would give back every
 *    key we just took. The rule is narrower: a shortcut is skipped when the
 *    focused element is an ordinary `input`, `textarea` or `contenteditable`
 *    that is **not** xterm's. Typing a session name must not switch panes;
 *    pressing ⌘2 in a terminal must.
 *
 * ## Why bindings are stored as strings
 *
 * `"Mod+Alt+ArrowLeft"` rather than a parsed record: it is what a person would
 * write, it survives `JSON.parse` without a schema, and an unknown one is
 * inert rather than a crash. `Mod` means ⌘ on macOS and Ctrl elsewhere, so one
 * default set is correct on both without a platform branch at every call site.
 */

export type ActionId =
  | "view.claude"
  | "view.files"
  | "view.transcript"
  | "view.jira"
  | "session.terminal"
  | "pane.left"
  | "pane.right"
  | "pane.up"
  | "pane.down"
  | "progress";

export type Action = { id: ActionId; label: string; hint: string };

/**
 * Every action, in the order the settings list shows them.
 *
 * Grouped by what they do rather than alphabetically — someone rebinding "move
 * around the grid" wants the four arrows together, and an alphabetical list
 * puts `pane.down` three rows from `pane.up`.
 */
export const ACTIONS: readonly Action[] = [
  { id: "view.claude", label: "Claude", hint: "Back to the conversation" },
  { id: "view.files", label: "Files", hint: "The file browser for this session's project" },
  { id: "view.transcript", label: "Transcript", hint: "The conversation as text" },
  { id: "view.jira", label: "Jira", hint: "This session's board" },
  { id: "session.terminal", label: "Terminal", hint: "A shell on this session's machine" },
  { id: "pane.left", label: "Pane left", hint: "Move the focus one tile left" },
  { id: "pane.right", label: "Pane right", hint: "Move the focus one tile right" },
  { id: "pane.up", label: "Pane up", hint: "Move the focus one tile up" },
  { id: "pane.down", label: "Pane down", hint: "Move the focus one tile down" },
  { id: "progress", label: "Progress", hint: "Open or close the Progress page" },
];

/**
 * The defaults, chosen around what is already taken.
 *
 * `Mod+1..4` for views because the numbers are free in both a shell and
 * Claude's TUI, and because "the first tab" is what a number key means
 * everywhere else. `Mod+Alt+Arrow` for the grid rather than plain `Mod+Arrow`:
 * ⌘← and ⌘→ are line-start and line-end in every macOS text field, including
 * the composer this app is full of.
 *
 * Deliberately avoided: ⌘W, ⌘Q, ⌘N, ⌘M — macOS binds those in the app menu, so
 * a shortcut here would either lose or, worse, win and close the window.
 */
export const DEFAULTS: Record<ActionId, string> = {
  "view.claude": "Mod+1",
  "view.files": "Mod+2",
  "view.transcript": "Mod+3",
  "view.jira": "Mod+4",
  "session.terminal": "Mod+T",
  "pane.left": "Mod+Alt+ArrowLeft",
  "pane.right": "Mod+Alt+ArrowRight",
  "pane.up": "Mod+Alt+ArrowUp",
  "pane.down": "Mod+Alt+ArrowDown",
  progress: "Mod+P",
};

const KEY = "rmux.shortcuts";

export type Bindings = Record<ActionId, string>;

export const isMac = (): boolean =>
  typeof navigator !== "undefined" && /mac/i.test(navigator.platform || navigator.userAgent);

/**
 * The canonical form of a key combination.
 *
 * Order is fixed (`Mod`, `Ctrl`, `Alt`, `Shift`, key) so that two spellings of
 * the same chord compare equal — otherwise "Alt+Mod+K" and "Mod+Alt+K" would be
 * two different bindings that fire on the same keystroke, and the conflict
 * detector would swear they were fine.
 */
export function normalise(binding: string): string {
  const parts = binding
    .split("+")
    .map((p) => p.trim())
    .filter(Boolean);
  if (!parts.length) return "";
  const key = parts[parts.length - 1]!;
  const mods = new Set(parts.slice(0, -1).map((m) => m.toLowerCase()));
  const out: string[] = [];
  if (mods.has("mod") || mods.has("cmd") || mods.has("meta")) out.push("Mod");
  if (mods.has("ctrl") || mods.has("control")) out.push("Ctrl");
  if (mods.has("alt") || mods.has("option")) out.push("Alt");
  if (mods.has("shift")) out.push("Shift");
  // Single characters are upper-cased so `k` and `K` are one binding; named
  // keys (`ArrowLeft`, `Enter`) keep their spelling.
  out.push(key.length === 1 ? key.toUpperCase() : key);
  return out.join("+");
}

/** The chord a keyboard event represents, in the same canonical form. */
export function accelOf(e: KeyboardEvent): string {
  const out: string[] = [];
  const mod = isMac() ? e.metaKey : e.ctrlKey;
  if (mod) out.push("Mod");
  // On macOS Ctrl is its own modifier; elsewhere Ctrl *is* Mod and must not be
  // counted twice, or every Mod binding would need a phantom Ctrl to match.
  if (e.ctrlKey && isMac()) out.push("Ctrl");
  if (e.altKey) out.push("Alt");
  if (e.shiftKey) out.push("Shift");
  const key = e.key;
  out.push(key.length === 1 ? key.toUpperCase() : key);
  return out.join("+");
}

/** Whether a chord carries a real modifier. Shift alone is not one. */
export function hasModifier(binding: string): boolean {
  const norm = normalise(binding);
  return /(^|\+)(Mod|Ctrl|Alt)\+/.test(norm);
}

/** Human spelling for a label: `⌘⌥←` on macOS, `Ctrl+Alt+Left` elsewhere. */
export function label(binding: string): string {
  const norm = normalise(binding);
  if (!norm) return "—";
  const parts = norm.split("+");
  const key = parts.pop()!;
  const pretty = key
    .replace(/^Arrow/, "")
    .replace(/^Escape$/, "Esc")
    .replace(/^ $/, "Space");
  if (!isMac()) {
    return [...parts.map((p) => (p === "Mod" ? "Ctrl" : p)), pretty].join("+");
  }
  const glyph: Record<string, string> = { Mod: "⌘", Ctrl: "⌃", Alt: "⌥", Shift: "⇧" };
  const arrow: Record<string, string> = { Left: "←", Right: "→", Up: "↑", Down: "↓" };
  return parts.map((p) => glyph[p] ?? p).join("") + (arrow[pretty] ?? pretty);
}

export function readBindings(): Bindings {
  const out = { ...DEFAULTS };
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return out;
    const stored = JSON.parse(raw) as Partial<Record<ActionId, string>>;
    for (const action of ACTIONS) {
      const value = stored[action.id];
      // An action added in a later version is absent from an older stored map,
      // and must fall back to its default rather than becoming unbound — the
      // same reconciliation the widget rail does.
      if (typeof value === "string") out[action.id] = normalise(value);
    }
  } catch {
    /* a corrupt map is not worth losing every shortcut over */
  }
  return out;
}

export function writeBindings(bindings: Bindings): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(bindings));
    window.dispatchEvent(new CustomEvent("rmux:shortcuts-changed"));
  } catch {
    /* a full localStorage must not break rebinding */
  }
}

/**
 * Actions sharing a chord.
 *
 * Returned rather than prevented, because the moment *during* a rebind when two
 * actions collide is normal — you have set the new one and not yet moved the
 * old. The settings list marks them; it does not refuse to save. A shortcut
 * that silently does one of two things is the thing to avoid, and saying so is
 * enough.
 */
export function conflicts(bindings: Bindings): ActionId[][] {
  const byChord = new Map<string, ActionId[]>();
  for (const action of ACTIONS) {
    const chord = normalise(bindings[action.id] ?? "");
    if (!chord) continue;
    byChord.set(chord, [...(byChord.get(chord) ?? []), action.id]);
  }
  return [...byChord.values()].filter((ids) => ids.length > 1);
}

/**
 * Whether a keystroke should be ignored because the operator is typing.
 *
 * xterm's textarea is explicitly *not* an exemption — it holds focus the whole
 * time a terminal is in use, so treating it as "typing" would disable every
 * shortcut in the app's main view.
 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.closest(".xterm")) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

/** The action a keystroke triggers, or null. */
export function match(e: KeyboardEvent, bindings: Bindings): ActionId | null {
  if (isTypingTarget(e.target)) return null;
  const chord = accelOf(e);
  for (const action of ACTIONS) {
    if (normalise(bindings[action.id] ?? "") === chord) return action.id;
  }
  return null;
}

/**
 * Where the focus goes when moving one tile in a direction.
 *
 * Clamped rather than wrapped. Wrapping means pressing "left" at the left edge
 * lands on the far right, which in a 4×4 is six tiles from where the hand
 * expected — and there is no visual sense in which those are adjacent.
 * Returns the same index at an edge, so the caller can treat "no move" as a
 * no-op rather than a special case.
 */
export function moveFocus(
  index: number,
  direction: "left" | "right" | "up" | "down",
  cols: number,
  rows: number,
): number {
  const total = Math.max(1, cols * rows);
  const at = Math.min(Math.max(0, index), total - 1);
  const row = Math.floor(at / cols);
  const col = at % cols;
  if (direction === "left") return col > 0 ? at - 1 : at;
  if (direction === "right") return col < cols - 1 ? at + 1 : at;
  if (direction === "up") return row > 0 ? at - cols : at;
  return row < rows - 1 ? at + cols : at;
}

/**
 * Ask a session's pane to change view.
 *
 * An event rather than store state, because which sub-view a pane is showing is
 * *ephemeral* — it should not survive a restart, and putting it in the
 * persisted workspace would restore everyone into a transcript they closed
 * days ago. The pane owns the state; this is a request to it.
 */
export const VIEW_EVENT = "rmux:set-view";

export type ViewName = "claude" | "files" | "transcript" | "jira";

export function requestView(sessionId: string, view: ViewName): void {
  window.dispatchEvent(new CustomEvent(VIEW_EVENT, { detail: { sessionId, view } }));
}
