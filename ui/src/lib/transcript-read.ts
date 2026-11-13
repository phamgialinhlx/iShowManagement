/**
 * What reading a conversation back *is*, shared by the desktop transcript view
 * (`TranscriptView`) and the mobile chat (`mobile/ChatView`).
 *
 * One definition of which command reads which agent's transcript, how much of the
 * tail is read, and when a re-read counts as "something changed" — so the two
 * views cannot drift into disagreeing about what the conversation says.
 */
import { api, type TargetRef, type Transcript } from "./api";

export type TranscriptProvider = "claude" | "pi" | "codex";

/**
 * How much of the tail to read at a time.
 *
 * 512KB sounded generous and was not: most of a Claude transcript is plumbing
 * — slash-command wrappers, caveat banners, system reminders — which these views
 * demote and hide. On a real 220MB transcript that budget surfaced a single
 * visible message, which reads as "the transcript is broken" rather than "the
 * window is small". 2MB is a few hundred KB of actual conversation.
 */
export const TAIL_STEP = 2 * 1024 * 1024;

/** Read the tail of a session's transcript through its agent's own command.
 *  The returned shape is identical for all three, so only the fetch forks. */
export function readTranscript(
  provider: TranscriptProvider,
  target: TargetRef,
  folder: string,
  resume: string | undefined,
  tail: number,
): Promise<Transcript> {
  if (provider === "pi") return api.piTranscript(target, folder, resume, tail);
  // Codex's rollout, tail only; with no id, the newest one for this folder.
  if (provider === "codex") return api.codexTranscript(target, folder, resume, tail);
  return api.claudeTranscript(target, folder, resume, tail);
}

/**
 * Did the poll actually bring anything new?
 *
 * Compared by what the reader would notice — how much was read, how many turns,
 * and the last turn's text — rather than deep-equalled. A transcript tail is
 * append-only in practice, so the final entry changing is the signal, and this
 * runs on a list that can be thousands of items long.
 */
export function sameTranscript(a: Transcript | null, b: Transcript): boolean {
  if (!a) return false;
  if (a.readBytes !== b.readBytes || a.totalBytes !== b.totalBytes) return false;
  if (a.entries.length !== b.entries.length) return false;
  const last = a.entries.at(-1);
  const next = b.entries.at(-1);
  return last?.text === next?.text && last?.timestamp === next?.timestamp;
}

/**
 * Is the operator selecting text inside this view right now?
 *
 * Guards every automatic DOM change. `getSelection` is checked against the
 * container rather than globally, so a selection elsewhere does not freeze this
 * view — and `isCollapsed` is what separates a real selection from a bare caret,
 * which every click leaves behind.
 */
export function hasSelectionWithin(container: HTMLElement | null): boolean {
  if (!container) return false;
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return false;
  const range = selection.getRangeAt(0);
  return container.contains(range.commonAncestorContainer);
}

export const humanBytes = (bytes: number) => {
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)}${units[unit]}`;
};
