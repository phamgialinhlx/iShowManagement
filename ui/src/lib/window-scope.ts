/**
 * Per-window namespacing for the layout state that lives in `localStorage`.
 *
 * A second workbench window shares this window's origin, so it shares
 * `localStorage`. Left alone, two windows would read the *same* workspace on
 * launch — identical panes — and then clobber each other's writes to the same
 * key, so dragging a session into one would rearrange the other. The whole point
 * of a second window is a *different* layout on a different desktop, so the
 * layout keys are namespaced by the window's own label.
 *
 * The label is carried in the URL (`index.html?window=<label>`, set by
 * `src-tauri/src/new_window.rs`) rather than asked of Tauri, because the scope
 * must be known **synchronously** — `workspace.ts` reads it at module load,
 * before the first render. `settings` is not a scope: the settings window loads
 * the same bundle with `?window=settings` and renders `<Settings/>`, never a
 * workspace, so it is treated as the primary (empty) scope.
 *
 * Only *layout* keys are scoped. Appearance, theme, the "seen" watermarks and
 * saved views stay shared on purpose — every window should look the same, a
 * session looked at in one window is seen everywhere, and a view saved in one
 * window is meant to be recallable in another.
 */
export function windowScope(): string {
  try {
    const w = new URLSearchParams(window.location.search).get("window");
    return w && w !== "settings" ? w : "";
  } catch {
    // No `window` (a Node test context) — the primary scope is the safe answer.
    return "";
  }
}

/** The primary window owns the unscoped keys and the singleton engines (the
 *  Redstone auto-bridge sweep runs here only, or every extra window would fire
 *  the same SSH commands at once). */
export function isPrimaryWindow(): boolean {
  return windowScope() === "";
}

/** A layout key, suffixed with the window scope so extra windows keep their own.
 *  The primary window returns the bare key, so existing installs keep the layout
 *  they already have. */
export function scopedKey(base: string): string {
  const scope = windowScope();
  return scope ? `${base}::${scope}` : base;
}
