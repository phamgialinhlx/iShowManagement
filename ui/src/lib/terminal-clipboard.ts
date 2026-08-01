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
 * Only select-all. Copy and paste are xterm's own — see the note at the top of
 * this file; handling them here is what made paste arrive twice.
 */
export function attachClipboard(xterm: Xterm): void {
  xterm.attachCustomKeyEventHandler((event) => {
    if (event.type !== "keydown") return true;

    const isMac = navigator.platform.toUpperCase().includes("MAC");
    // Plain Ctrl+C must stay SIGINT on every platform, so the modifier for
    // terminal-local shortcuts is Cmd on macOS and Ctrl+Shift elsewhere.
    const combo = isMac ? event.metaKey && !event.ctrlKey : event.ctrlKey && event.shiftKey;
    if (!combo) return true;

    if (event.key.toLowerCase() === "a") {
      xterm.selectAll();
      return false;
    }

    return true;
  });
}
