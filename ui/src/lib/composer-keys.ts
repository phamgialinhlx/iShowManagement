/**
 * What a key does in the mobile composer.
 *
 * Enter is a **newline**, always — on a phone the Return key is the only way to
 * break a line, and a composer that sends on Return sends half a thought. Sending
 * is the button, or ⌘/Ctrl+Enter on a hardware keyboard (an iPad with a keyboard
 * case). Nothing fires during IME composition: Vietnamese and CJK input methods
 * use Enter to commit a word, and sending there would ship the unfinished one.
 *
 *     node --experimental-strip-types ui/mobile-check.ts
 */

export type KeyLike = {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  isComposing: boolean;
};

export function composerKey(e: KeyLike): "send" | "default" {
  if (e.isComposing || e.key !== "Enter") return "default";
  return (e.metaKey || e.ctrlKey) && !e.altKey ? "send" : "default";
}

/** Whether there is anything to send — whitespace alone is not a message. */
export function canSend(text: string): boolean {
  return text.trim().length > 0;
}

/** The composer's height: its content, between one line and `maxLines`. */
export function composerHeight(contentPx: number, linePx: number, padPx: number, maxLines: number): number {
  const min = linePx + padPx;
  const max = linePx * maxLines + padPx;
  return Math.max(min, Math.min(max, Math.ceil(contentPx)));
}

/**
 * The quick keys offered while an agent is blocked on a question. Names, not
 * bytes: they go to `claude_keys`, which resolves them through the one key
 * table in Rust (`rmux_claude::keys::named`). This is a row of keystrokes — not
 * a reimplementation of the dialog, which Claude draws and owns.
 */
export const QUICK_KEYS: readonly { name: string; label: string; aria: string }[] = [
  { name: "1", label: "1", aria: "Press 1" },
  { name: "2", label: "2", aria: "Press 2" },
  { name: "3", label: "3", aria: "Press 3" },
  { name: "up", label: "↑", aria: "Press the up arrow" },
  { name: "down", label: "↓", aria: "Press the down arrow" },
  { name: "enter", label: "Enter", aria: "Press Enter" },
  { name: "esc", label: "Esc", aria: "Press Escape" },
];
