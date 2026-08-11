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
 * Which platform this is, asked of more than one source.
 *
 * `navigator.platform` is **deprecated**, and the failure mode if a webview ever
 * stops populating it is the worst kind available here: it returns `""`, every
 * `includes` is false, and the Windows branch below simply ceases to exist —
 * silently, with Ctrl+C going back to interrupting instead of copying and
 * nothing anywhere to say why. `userAgentData.platform` is the supported
 * replacement and is asked first; the user-agent string is the last resort so
 * that all three would have to fail at once. Measured in the shipped app:
 * `platform: "Win32"`, `userAgentData.platform: "Windows"`.
 */
function platformSays(pattern: RegExp): boolean {
  if (typeof navigator === "undefined") return false;
  const data = (navigator as { userAgentData?: { platform?: string } }).userAgentData;
  return pattern.test(data?.platform ?? "") || pattern.test(navigator.platform || "") ||
    pattern.test(navigator.userAgent || "");
}

const onMac = (): boolean => platformSays(/mac/i);
const onWindows = (): boolean => platformSays(/win/i);

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
export function attachClipboard(xterm: Xterm): Disposer {
  const isMac = onMac();
  const isWindows = onWindows();

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

    // **Ctrl+V: get out of the way and let the platform paste.**
    //
    // Same asymmetry as Ctrl+C, one key over, and measured the same way: xterm
    // claims Ctrl+V for the control code `\x16` and calls `preventDefault`, so
    // the browser never raises a `paste` event and nothing arrives. `Cmd+V` is
    // not a control key, is never claimed, and on macOS the Edit menu routes it
    // as well — so paste has always worked there and never on Windows.
    //
    // **Nothing is implemented here, deliberately.** Reading the clipboard and
    // writing it into the terminal would be a second implementation of paste,
    // which is exactly what made every paste arrive twice and is why this file
    // exists. Returning `false` makes xterm skip the key *without* calling
    // `preventDefault` — verified in its source: `_keyDown` returns the moment
    // the custom handler refuses, before any of the encoding — so the browser
    // performs its own paste and xterm's existing `paste` listener receives it.
    // That keeps bracketed-paste mode correct too, which a hand-rolled version
    // would have to get right on its own: without it a multi-line paste runs
    // every line but the last.
    //
    // **Windows only, and the measurement is why.** Three chords were checked
    // against a real xterm:
    //
    //     ctrl+V        claimed  → \x16, so no paste event is ever raised
    //     ctrl+shift+V  free     → the platform already pastes
    //     cmd+V         free     → the platform already pastes
    //
    // So plain Ctrl+V is the only broken one, and releasing anything else would
    // change a platform that is not broken. Linux keeps plain Ctrl+V as
    // readline's quoted-insert — `Ctrl+V Ctrl+M` to type a literal carriage
    // return is a real thing people do — and pastes with Ctrl+Shift+V, which is
    // the convention there and already works. On Windows, Ctrl+V *is* the paste
    // key (Windows Terminal, VS Code), and nothing is lost by giving it up.
    if (isWindows && event.ctrlKey && !event.shiftKey && !event.altKey && key === "v") {
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

  return attachFocusFallback(xterm, isMac);
}

/** Detaches whatever `attachClipboard` installed outside xterm itself. */
export type Disposer = () => void;

/**
 * Copy the terminal's selection even when the terminal does not have focus.
 *
 * `attachCustomKeyEventHandler` only ever runs for keys delivered **to xterm's
 * textarea**. That is the whole shortcut path, and it is enough right up until
 * focus is somewhere else — at which point Ctrl+C reaches `document` instead,
 * xterm never hears it, and the selection sitting on screen is not copied. The
 * operator sees a highlighted block and a key that does nothing.
 *
 * Focus drifting off the terminal is not hypothetical here: it is why
 * `ClaudePanel` restores focus on `mousedown` at all. Any header control, and
 * any click on the padding around the rows, moves it. Right-click › Copy keeps
 * working throughout, because the native menu raises a `copy` event that xterm
 * answers regardless of focus — which is exactly the asymmetry that gets
 * reported as "I can copy with the mouse but not with the keyboard".
 *
 * Deliberately narrow, because this is a listener on `window` competing with a
 * key the terminal needs:
 *
 *  - **Only when xterm has a selection.** No selection is an ordinary interrupt
 *    and must stay one.
 *  - **Only when nothing else is selected.** A real DOM selection — prose in the
 *    transcript, a file preview — belongs to the browser's own copy, and taking
 *    it here would copy the terminal instead of the words under the cursor.
 *  - **Only when the operator is not typing in a field.** A rename box is a text
 *    field and Ctrl+C there means the field's own copy.
 *  - **Never on macOS**, where ⌘C is the copy key and the Edit menu already
 *    routes it. Plain Ctrl+C there is a control code the shell wants.
 */
function attachFocusFallback(xterm: Xterm, isMac: boolean): Disposer {
  if (isMac || typeof window === "undefined") return () => {};

  const onKey = (event: KeyboardEvent) => {
    if (!event.ctrlKey || event.altKey || event.metaKey) return;
    if (event.key.toLowerCase() !== "c") return;
    // Shift is allowed: Ctrl+Shift+C is the other copy binding, and it reaches
    // here for the same reason plain Ctrl+C does.

    // xterm's own handler already dealt with it — this is the fallback for the
    // case where the key never got there.
    const target = event.target as HTMLElement | null;
    if (target?.closest(".xterm")) return;

    const tag = target?.tagName;
    if (target?.isContentEditable || tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      return;
    }

    if (!xterm.hasSelection()) return;
    if ((window.getSelection()?.toString() ?? "").trim()) return;

    event.preventDefault();
    void copySelection(xterm)
      .then(() => xterm.clearSelection())
      .catch(() => {});
  };

  // Bubble, not capture: a text field or a nested handler that wants this key
  // should win, and by the time it reaches `window` nothing else has claimed it.
  window.addEventListener("keydown", onKey);
  return () => window.removeEventListener("keydown", onKey);
}
