import { create } from "zustand";

import {
  decide,
  prefixBytes,
  readPrefixConfig,
  type Effect,
  type KeyLike,
  type Mode,
  type PrefixConfig,
} from "./prefix-mode.ts";

/**
 * herdr's prefix-key model — the **DOM** half: where keys are intercepted, and
 * the little bit of state the mode bar renders. The decisions are all in
 * `prefix-mode.ts`; the effects (store calls) are registered by
 * `use-prefix-mode.ts`, so this file imports neither React nor the workspace
 * store and can be loaded by every xterm host without booting anything.
 *
 * ## Why the terminal path is xterm's custom key handler
 *
 * xterm writes a key to the pty from its *own* keydown handler on its textarea.
 * A `window` listener sees the event afterwards, so `preventDefault` there is
 * too late — `ctrl+b` would already be `\x02` on the wire, which is exactly how
 * a colliding `Mod` chord fires *both* off macOS (CLAUDE.md, keyboard rules).
 * `attachCustomKeyEventHandler` runs **before** xterm encodes anything, and
 * returning `false` makes xterm skip the key. That handler slot holds one
 * function, and `attachClipboard` already owns it — so `terminalKeyGate` is
 * called first from inside that handler rather than installed beside it (a
 * second `attachCustomKeyEventHandler` would silently replace the clipboard
 * rules).
 *
 * ## Where the prefix works
 *
 * - **In every terminal** (shell, Claude, pi, the companion): via the gate.
 * - **Anywhere else that is not a text field** (rail, tab bar, empty pane):
 *   via a bubble-phase `window` listener. Nothing races it there — no terminal
 *   has the key — so bubbling costs nothing and lets a nested handler win.
 * - **Not inside a text input, a textarea, a contenteditable or Monaco**, in
 *   terminal mode. `ctrl+b` is a real key in those (Monaco: nothing by
 *   default, but the operator's own keymap; a rename box: the platform's
 *   cursor-back on macOS emacs bindings). A prefix that eats it there is the
 *   "you press a key and nothing happens" failure this app exists to avoid.
 *   Once a mode is *active* the keys are taken wherever focus is: the operator
 *   asked for a mode, and a mode that ignores the next key because of where the
 *   caret happens to be is a mode that cannot be trusted.
 *
 * ## What leaks, and why nothing does
 *
 * Only `keydown` is decided. When a keydown is consumed its `code` is
 * remembered and the matching `keypress`/`keyup` are refused too — xterm
 * handles keypress separately and would otherwise type the letter. Consumed
 * keydowns are also `preventDefault`ed, which stops the textarea `input`
 * event that xterm would read as typed text.
 */

// ── state the UI renders ─────────────────────────────────────────────────────

export type PrefixDialog =
  | { kind: "rename"; what: "project" | "pane"; id: string; value: string }
  | { kind: "rename"; what: "tab"; id: string; projectId: string; value: string }
  | { kind: "closeProject"; id: string };

type PrefixStore = {
  mode: Mode;
  /** Navigate mode's highlighted space (project) id. The rail may read it. */
  navTarget: string | null;
  /** A short message on the mode bar row (e.g. "go to: coming soon"). */
  toast: string | null;
  helpOpen: boolean;
  /** A prompt the prefix table opened: rename, or the close-project confirmation. */
  dialog: PrefixDialog | null;
  setMode: (mode: Mode) => void;
};

export const usePrefixMode = create<PrefixStore>((set) => ({
  mode: "terminal",
  navTarget: null,
  toast: null,
  helpOpen: false,
  dialog: null,
  setMode: (mode) => set({ mode }),
}));

let toastTimer: number | undefined;
export function prefixToast(message: string): void {
  usePrefixMode.setState({ toast: message });
  if (typeof window === "undefined") return;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => usePrefixMode.setState({ toast: null }), 2600);
}

// ── config ───────────────────────────────────────────────────────────────────

let config: PrefixConfig = readPrefixConfig();
export function prefixConfig(): PrefixConfig {
  return config;
}
if (typeof window !== "undefined") {
  const reload = () => {
    config = readPrefixConfig();
  };
  window.addEventListener("rmux:prefix-changed", reload);
  window.addEventListener("storage", (e) => {
    if (!e.key || e.key === "rmux.prefix") reload();
  });
}

// ── effects ──────────────────────────────────────────────────────────────────

export type Dispatcher = (effect: Effect) => void;
let dispatcher: Dispatcher | null = null;

/** `use-prefix-mode.ts` installs the store calls here. Returns an uninstaller. */
export function setPrefixDispatcher(fn: Dispatcher): () => void {
  dispatcher = fn;
  return () => {
    if (dispatcher === fn) dispatcher = null;
  };
}

// ── terminals that can receive a literal prefix from outside ─────────────────

/** Writes typed input into a pane's terminal — `xterm.input(data, true)`. */
export type InputSink = (data: string) => void;
const targets = new Map<string, InputSink>();
let focusedSession: () => string | null = () => null;

/**
 * Register the terminal that answers for a session — the same one that answers
 * `FOCUS_EVENT` (so the companion shell, which shares its conversation's id,
 * does not). Only used when "send prefix" happens with focus *outside* every
 * terminal; inside one, the key is just given back to xterm.
 */
export function registerPrefixTarget(sessionId: string, sink: InputSink): () => void {
  targets.set(sessionId, sink);
  return () => {
    if (targets.get(sessionId) === sink) targets.delete(sessionId);
  };
}

export function setFocusedSessionSource(fn: () => string | null): void {
  focusedSession = fn;
}

function sendLiteral(token: string): void {
  const id = focusedSession();
  const sink = id ? targets.get(id) : undefined;
  const bytes = prefixBytes(token);
  if (!sink || bytes === null) return;
  // `xterm.input` raises `onData`, so the byte takes exactly the path a typed
  // key takes — the pane's own write to its pty or Claude session, IME gate
  // and all.
  sink(bytes);
}

// ── the two interception points ──────────────────────────────────────────────

/** Events already decided by a terminal gate — the window listener and
 *  `useShortcuts` must not decide them a second time. */
const seen = new WeakSet<Event>();
/** `code`s whose keydown was consumed; their keypress/keyup are refused. */
const swallowed = new Set<string>();

/** Events the prefix layer took. */
const consumed = new WeakSet<Event>();

/**
 * Whether another keyboard listener must leave this event alone: the prefix
 * layer took it, or a mode is active and it is not a ⌘ chord (⌘ chords pass
 * through every mode — see `decide`). `useShortcuts` and the workbench's own
 * listener check this, because on `window` listener order is registration
 * order and `stopPropagation` does not stop a sibling listener.
 */
export function prefixOwns(e: KeyboardEvent): boolean {
  if (consumed.has(e)) return true;
  return usePrefixMode.getState().mode !== "terminal" && !e.metaKey;
}

function swallowKey(e: KeyLike): void {
  swallowed.add(e.code || e.key);
}

function apply(e: KeyboardEvent, inTerminal: boolean): boolean {
  const before = usePrefixMode.getState().mode;
  const verdict = decide(before, e, config);
  // The effect runs *before* the mode changes, so an effect that reads mode
  // state (navigate's Enter reads the highlight) sees it before leaving clears it.
  const commit = () => {
    if (verdict.mode !== before) usePrefixMode.getState().setMode(verdict.mode);
  };

  if (verdict.effect?.kind === "sendPrefix") {
    // Inside a terminal: give the key back to xterm, which encodes it as it
    // would have with no prefix layer at all. Outside: write it ourselves.
    if (!inTerminal) {
      e.preventDefault();
      consumed.add(e);
      sendLiteral(verdict.effect.token);
    }
    commit();
    return false;
  }

  if (verdict.consume) {
    e.preventDefault();
    e.stopPropagation();
    consumed.add(e);
    swallowKey(e);
  }
  if (verdict.effect) dispatcher?.(verdict.effect);
  commit();
  return verdict.consume;
}

/**
 * Called first from every xterm host's custom key handler. Returns what that
 * handler should return: `false` to make xterm ignore the key.
 */
export function terminalKeyGate(e: KeyboardEvent): boolean {
  seen.add(e);
  if (e.type !== "keydown") {
    const k = e.code || e.key;
    if (!swallowed.has(k)) return true;
    if (e.type === "keyup") swallowed.delete(k);
    return false;
  }
  return !apply(e, true);
}

function isTextField(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.closest(".xterm")) return false;
  if (target.closest(".monaco-editor")) return true;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

/** The window-level path, for when no terminal has the keyboard. */
export function installWindowPrefix(): () => void {
  const onDown = (e: KeyboardEvent) => {
    if (seen.has(e)) return;
    seen.add(e);
    const mode = usePrefixMode.getState().mode;
    if (mode === "terminal" && isTextField(e.target)) return;
    apply(e, false);
  };
  const onUp = (e: KeyboardEvent) => {
    if (seen.has(e)) return;
    const k = e.code || e.key;
    if (!swallowed.has(k)) return;
    if (e.type === "keyup") swallowed.delete(k);
    e.preventDefault();
  };
  window.addEventListener("keydown", onDown);
  window.addEventListener("keypress", onUp);
  window.addEventListener("keyup", onUp);
  return () => {
    window.removeEventListener("keydown", onDown);
    window.removeEventListener("keypress", onUp);
    window.removeEventListener("keyup", onUp);
  };
}
