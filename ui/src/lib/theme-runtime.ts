/**
 * The live theme: what is active, how it reaches the DOM, and how a change
 * anywhere (this window, another window, an external hand-edit) repaints.
 *
 * `theme.ts` is pure — a model and a derivation with no side effects, so the
 * check harness and Node can exercise it. This module is the side effects: it
 * holds the current theme, writes the derived tokens onto the document, and keeps
 * every window in step with the canonical file (`src-tauri/src/theme.rs`).
 *
 * ## The file is canonical; localStorage is only a paint-cache
 *
 * IPC is async, so the file cannot be read *synchronously* before the first
 * paint. To avoid a flash of the default palette on every launch, the last
 * applied theme is cached in `localStorage` and applied immediately; the
 * canonical file is then read a beat later and reconciles. The cache is never the
 * source of truth — a hand-edit made while the app was closed wins on the next
 * `initTheme`, because the file is always re-read over the cache.
 */

import { listen } from "@tauri-apps/api/event";

import { SIGNAL_ROOM, BUILT_INS, deriveTokens, type Theme } from "./theme";
import { api, isTauri, type ThemeState } from "./api";

/** Paint-cache only. Not authoritative — see the module note. */
const CACHE_KEY = "rmux.theme.cache";
/**
 * A same-document signal. `storage` fires only in *other* documents, so the
 * window that applied a theme still has to tell its own xterms and Monaco to
 * re-read — they cache the palette at construction and do not watch CSS.
 */
export const THEME_EVENT = "rmux-theme";

let current: Theme = SIGNAL_ROOM;
let userThemes: Theme[] = [];
let activeName: string = SIGNAL_ROOM.name;

const listeners = new Set<() => void>();

/** The palette in effect right now — what xterm/Monaco read at construction. */
export function activeTheme(): Theme {
  return current;
}

/** Built-ins first, then the operator's saved themes. The switcher's list. */
export function allThemes(): Theme[] {
  return [...BUILT_INS, ...userThemes];
}

/** The current state for the switcher UI. */
export function themeSnapshot(): { active: string; all: Theme[]; user: Theme[] } {
  return { active: activeName, all: allThemes(), user: userThemes };
}

/** Resolve a name to a theme, falling back to SIGNAL ROOM for an unknown one —
 *  a deleted active theme must not leave the app with no colours. */
export function resolve(name: string, users: Theme[] = userThemes): Theme {
  return [...BUILT_INS, ...users].find((t) => t.name === name) ?? SIGNAL_ROOM;
}

/** Subscribe to state changes (for the Settings switcher). Returns an unsubscribe. */
export function subscribeTheme(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

/**
 * Write the derived tokens onto the document root and tell xterm/Monaco to
 * re-read. Every `var(--…)` in the app and in `signal-room.css` re-skins at once.
 */
export function applyTheme(t: Theme): void {
  current = t;
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  for (const [k, v] of Object.entries(deriveTokens(t))) root.style.setProperty(k, v);
  window.dispatchEvent(new Event(THEME_EVENT));
}

function cache(t: Theme): void {
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(t));
  } catch {
    // A full quota costs a flash on next launch, nothing worse.
  }
}

function cached(): Theme | null {
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    return raw ? (JSON.parse(raw) as Theme) : null;
  } catch {
    return null;
  }
}

/** Fold a fresh state from Rust into the live palette and notify the UI. */
function ingest(state: ThemeState): void {
  userThemes = state.userThemes ?? [];
  activeName = state.active;
  const t = resolve(activeName, userThemes);
  applyTheme(t);
  cache(t);
  for (const l of listeners) l();
}

/**
 * Apply the cached theme *synchronously*, before the first paint. Called from
 * `main.tsx` alongside `applyAppearance`, so a launch shows the last theme with
 * no flash; `initTheme` reconciles against the file immediately after.
 */
export function applyCachedThemeEarly(): void {
  applyTheme(cached() ?? SIGNAL_ROOM);
}

/**
 * Reconcile against the canonical file and start listening for changes — an
 * in-app edit in another window (Rust emits `theme-changed`) or an external
 * hand-edit (the watcher emits the same). Fire-and-forget from startup.
 */
export async function initTheme(): Promise<void> {
  if (!isTauri()) {
    // Plain browser (design work, the check harnesses): the cache or the default
    // is all there is, and there is no file to reconcile against.
    applyTheme(cached() ?? SIGNAL_ROOM);
    return;
  }
  try {
    ingest(await api.themeState());
  } catch {
    applyTheme(SIGNAL_ROOM);
  }
  try {
    await listen<ThemeState>("theme-changed", (e) => ingest(e.payload));
  } catch {
    // No live updates without the listener; edits still land on next launch.
  }
}

/* ------------------------------------------------------- mutations (UI) */

/** Switch the active theme. Rust writes the file and tells every window. */
export async function setActiveTheme(name: string): Promise<void> {
  ingest(await api.themeSetActive(name));
}

/** Create or update a user theme (never a built-in — Rust refuses). */
export async function saveTheme(theme: Theme): Promise<void> {
  ingest(await api.themeSave(theme));
}

export async function deleteTheme(name: string): Promise<void> {
  ingest(await api.themeDelete(name));
}

/** A unique "<base> (copy)" name for forking a built-in before editing it. */
export function copyName(base: string): string {
  const taken = new Set(allThemes().map((t) => t.name));
  let name = `${base} (copy)`;
  let n = 2;
  while (taken.has(name)) name = `${base} (copy ${n++})`;
  return name;
}
