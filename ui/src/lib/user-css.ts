/**
 * The operator's own CSS, applied over the design system.
 *
 * The previous app had this and people used it, for the reason custom CSS
 * always earns its keep: no design system fits every eye. Someone wants a
 * larger terminal font, someone else wants the rail wider, someone else has a
 * wallpaper this palette fights with. Those are all one rule each.
 *
 * ## Why this works at all in a WKWebView
 *
 * rmux's window is a native webview, not Chromium — there are no extensions and
 * no user-stylesheet setting to reach for. But a stylesheet does not need any
 * of that: a `<style>` appended last in `<head>` wins on document order against
 * everything of equal specificity, which is the whole mechanism. This is
 * therefore the same feature the old app had, arrived at by a simpler route.
 *
 * ## What it deliberately does not do
 *
 * **It never runs script.** The text goes into a `<style>` element's
 * `textContent`, which the CSS parser reads and nothing else does — an
 * `innerHTML` here would let a pasted snippet carrying a `<script>` execute
 * inside a webview that can reach Tauri IPC, which is the one thing this must
 * not permit. The same reasoning is why `.docx` HTML is sanitised before it
 * reaches the DOM.
 *
 * `!important` still loses to the border-radius rule in `signal-room.css`,
 * because that one is `!important` too and comes first only in source order —
 * later `!important` at equal specificity wins, so rounded corners *are*
 * reachable from here. That is the operator's call to make on their own copy.
 */

const STORAGE_KEY = "rmux.userCss";
const ELEMENT_ID = "rmux-user-css";

export function loadUserCss(): string {
  try {
    return localStorage.getItem(STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

/**
 * Apply it now.
 *
 * Exported so startup can run this before the first paint. Applying it in an
 * effect would show the stock design for a frame and then swap, which reads as
 * the app changing its mind on every launch.
 */
export function applyUserCss(css: string = loadUserCss()): void {
  let element = document.getElementById(ELEMENT_ID) as HTMLStyleElement | null;

  if (!css.trim()) {
    element?.remove();
    return;
  }

  if (!element) {
    element = document.createElement("style");
    element.id = ELEMENT_ID;
    document.head.appendChild(element);
  }

  // `textContent`, never `innerHTML`. See the module comment: this is the line
  // that keeps a pasted stylesheet from being a script injection.
  element.textContent = css;
}

export function saveUserCss(css: string): void {
  try {
    if (css.trim()) localStorage.setItem(STORAGE_KEY, css);
    else localStorage.removeItem(STORAGE_KEY);
  } catch {
    // A full localStorage costs persistence, not the current appearance.
  }
  applyUserCss(css);
}
