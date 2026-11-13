import { invoke, isTauri } from "./bridge";

/**
 * The one way rmux puts text on the clipboard.
 *
 * On the desktop it goes through the native pasteboard (`clipboard_write`),
 * because WKWebView's `navigator.clipboard.writeText` refuses whenever rmux's
 * document is not focused or the click's gesture was spent by an `await` — a
 * dsh pane is a separate native view that keeps focus, so copying a path
 * worked on some presses and not others. In the web build the browser API is
 * right (the native one would fill the *server's* clipboard), with the old
 * `execCommand` path behind it.
 *
 * Throws when nothing worked: a copy that fails silently leaves stale content
 * to be pasted somewhere else with nobody knowing why.
 */
export async function copyText(text: string): Promise<void> {
  if (isTauri()) {
    await invoke("clipboard_write", { text });
    return;
  }
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    // Secure context or permission missing — fall through to the old path.
  }
  const holder = document.createElement("textarea");
  holder.value = text;
  // Off-screen rather than hidden: `display:none` cannot be selected.
  holder.style.position = "fixed";
  holder.style.opacity = "0";
  holder.style.pointerEvents = "none";
  document.body.appendChild(holder);
  holder.select();
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } finally {
    holder.remove();
  }
  if (!copied) throw new Error("could not write to the clipboard");
}
