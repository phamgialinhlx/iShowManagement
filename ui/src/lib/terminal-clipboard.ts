import type { Terminal as Xterm } from "@xterm/xterm";

/**
 * Clipboard helpers for an xterm view.
 *
 * **Copy and paste are deliberately NOT handled here.** xterm already implements
 * both against the platform's own clipboard events, and the webview's menu
 * routes Cmd-C/Cmd-V into them. Adding a second implementation on the keydown
 * made every paste arrive twice — measured in the terminal tab — because the
 * menu path and the key path both fired.
 *
 * What is left is the gap xterm does not fill: select-all, and copying without a
 * selection at all. The latter matters because a full-screen program like
 * Claude's TUI turns on mouse reporting, and from then on a drag is *sent to the
 * program* instead of selecting. xterm's escape hatch is to hold Option (macOS)
 * or Shift, which is not discoverable — so callers also get `copyViewport` and
 * `copyAll`, which need no selection.
 */

/**
 * Write to the clipboard, falling back when the async API is unavailable.
 *
 * Throws if nothing worked. A copy that fails silently is worse than one that
 * errors: the operator pastes stale content somewhere else and never learns why.
 */
async function write(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    // `navigator.clipboard` needs a secure context and a permission that a
    // webview does not always grant. The old path still works everywhere, and a
    // copy that silently fails is worse than an ugly implementation.
  }

  let copied = false;
  const holder = document.createElement("textarea");
  holder.value = text;
  // Off-screen rather than hidden: `display:none` cannot be selected, so the
  // copy would do nothing.
  holder.style.position = "fixed";
  holder.style.opacity = "0";
  holder.style.pointerEvents = "none";
  document.body.appendChild(holder);
  holder.select();
  try {
    copied = document.execCommand("copy");
  } finally {
    holder.remove();
  }

  if (!copied) throw new Error("the clipboard rejected the write");
}

/** Read the terminal's buffer as lines of text. */
function bufferText(xterm: Xterm, fromScrollback: boolean): string {
  const buffer = xterm.buffer.active;
  const start = fromScrollback ? 0 : buffer.viewportY;
  const end = fromScrollback ? buffer.length : buffer.viewportY + xterm.rows;

  const lines: string[] = [];
  for (let y = start; y < end; y += 1) {
    // `true` keeps wrapped lines joined, so a long line copies as one line
    // rather than as however many columns the pane happens to be.
    lines.push(buffer.getLine(y)?.translateToString(true) ?? "");
  }

  // Trailing blank lines are an artefact of the grid's fixed height, not content.
  while (lines.length && !lines[lines.length - 1]!.trim()) lines.pop();
  return lines.join("\n");
}

/** Copy the visible screen. Needs no selection. */
export async function copyViewport(xterm: Xterm): Promise<void> {
  await write(bufferText(xterm, false));
}

/** Copy everything, scrollback included. */
export async function copyAll(xterm: Xterm): Promise<void> {
  await write(bufferText(xterm, true));
}

/** Copy the current selection, if there is one. Returns whether there was. */
export async function copySelection(xterm: Xterm): Promise<boolean> {
  const selection = xterm.getSelection();
  if (!selection) return false;
  await write(selection);
  return true;
}

/**
 * Wire the shortcuts xterm does not provide.
 *
 * **Copy on macOS is still xterm's own**, and must stay that way — the webview's
 * Edit menu routes Cmd-C into the platform's `copy` event, and adding a second
 * implementation on the keydown is what made paste arrive twice. Nothing below
 * touches the macOS path.
 *
 * ## Why Ctrl+C needed handling, when Cmd-C never did
 *
 * Windows has no application Edit menu to route the key, and — the part that
 * actually decides it — **xterm treats Ctrl+C as SIGINT and calls
 * `preventDefault()`**, so the browser never generates a `copy` event at all.
 * Cmd-C is not a terminal control key, so it is never intercepted and the native
 * path runs untouched. That asymmetry is the whole bug: right-click › Copy
 * works, because the context menu *does* raise a `copy` event, while the key
 * that everyone reaches for first silently does nothing.
 *
 * It bites hardest in the Claude pane. Its TUI turns on mouse reporting, so
 * selecting takes a Shift-drag most people never discover; having gone to that
 * trouble, Ctrl+C then throwing the selection away is a poor reward.
 */
export function attachClipboard(xterm: Xterm): void {
  const platform = navigator.platform.toUpperCase();
  const isMac = platform.includes("MAC");
  const isWindows = platform.includes("WIN");

  xterm.attachCustomKeyEventHandler((event) => {
    if (event.type !== "keydown") return true;

    const key = event.key.toLowerCase();

    // **Ctrl+C copies a selection and interrupts when there is none** — the
    // Windows Terminal and VS Code rule, which is what a Windows operator
    // expects and what they reported missing.
    //
    // Interrupting must not become unreachable: a stale selection from ten
    // minutes ago cannot be allowed to swallow the key that stops a runaway
    // process. So the selection is **cleared once the copy lands**, and the
    // very next Ctrl+C is an ordinary SIGINT. Worst case is pressing it twice,
    // which is recoverable; a terminal you cannot interrupt is not.
    if (isWindows && event.ctrlKey && !event.shiftKey && !event.altKey && key === "c") {
      if (!xterm.hasSelection()) return true;

      void copySelection(xterm)
        .then(() => xterm.clearSelection())
        // Leave the selection alone if the clipboard refused, so the operator
        // can try again or use the header button rather than silently losing it.
        .catch(() => {});
      return false;
    }

    // Plain Ctrl+C stays SIGINT everywhere else, so terminal-local shortcuts
    // take Cmd on macOS and Ctrl+Shift elsewhere.
    const combo = isMac ? event.metaKey && !event.ctrlKey : event.ctrlKey && event.shiftKey;
    if (!combo) return true;

    if (key === "a") {
      xterm.selectAll();
      return false;
    }

    // Ctrl+Shift+C is the long-standing terminal convention, and on Linux it is
    // the *only* copy binding — there is no Edit menu there either. Deliberately
    // not extended to Cmd-C on macOS: that already works through the menu, and
    // a second implementation of it is the bug this file was written about.
    if (!isMac && key === "c") {
      void copySelection(xterm).catch(() => {});
      return false;
    }

    return true;
  });
}
