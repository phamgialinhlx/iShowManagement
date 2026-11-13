/**
 * A transcript, shaped into chat bubbles for the mobile conversation view.
 *
 * Pure: the entries come from the one transcript reader (`transcript-read.ts`),
 * and this only decides how they are grouped on a phone.
 *
 * - **user / assistant** become bubbles (right / left);
 * - **a run of consecutive tool entries** becomes *one* collapsed row — a turn
 *   that edits six files is six tool records, and six full-width blocks of shell
 *   output between two sentences is how a phone conversation becomes unreadable;
 * - **system** (slash-command plumbing, caveats, reminders — demoted by the
 *   Rust parser) is hidden, as `TranscriptView` hides it, and counted so the
 *   view can say so rather than silently dropping it.
 *
 *     node --experimental-strip-types ui/mobile-check.ts
 */

export type Speaker = "user" | "assistant" | "tool" | "system";
export type EntryLike = { speaker: Speaker; text: string; tool?: string; timestamp?: string };

export type ToolCall = { key: string; tool?: string; text: string; timestamp?: string; summary: string };

export type Bubble =
  | { kind: "message"; key: string; role: "user" | "assistant"; text: string; timestamp?: string }
  | { kind: "tools"; key: string; calls: ToolCall[] };

/** The first non-empty line, trimmed and cut — what a collapsed tool row shows. */
export function summarise(text: string, max = 80): string {
  const line = text.split("\n").find((l) => l.trim().length > 0)?.trim() ?? "";
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/**
 * Stable keys that survive both an append (a new turn) and a "load more" (older
 * turns arriving in front): built from the entry itself plus how many identical
 * ones came before it, never from its index in a list whose front moves.
 */
function keyer() {
  const seen = new Map<string, number>();
  return (e: EntryLike) => {
    const base = `${e.speaker}|${e.timestamp ?? ""}|${e.text.length}|${e.text.slice(0, 24)}`;
    const n = seen.get(base) ?? 0;
    seen.set(base, n + 1);
    return `${base}#${n}`;
  };
}

export function shapeBubbles(entries: readonly EntryLike[]): { bubbles: Bubble[]; hiddenSystem: number } {
  const key = keyer();
  const bubbles: Bubble[] = [];
  let hiddenSystem = 0;
  for (const e of entries) {
    const k = key(e);
    if (e.speaker === "system") {
      hiddenSystem += 1;
      continue;
    }
    if (e.speaker === "tool") {
      const call: ToolCall = { key: k, tool: e.tool, text: e.text, timestamp: e.timestamp, summary: summarise(e.text) };
      const last = bubbles.at(-1);
      if (last?.kind === "tools") last.calls.push(call);
      else bubbles.push({ kind: "tools", key: `tools:${k}`, calls: [call] });
      continue;
    }
    // An empty message is a record with nothing to read (a tool-only turn's
    // text half); a blank bubble reads as a rendering fault.
    if (!e.text.trim()) continue;
    bubbles.push({ kind: "message", key: k, role: e.speaker, text: e.text, timestamp: e.timestamp });
  }
  return { bubbles, hiddenSystem };
}

/** "Read 3, Edit 2" — the label of a collapsed tool row. */
export function toolsLabel(calls: readonly ToolCall[]): string {
  const counts = new Map<string, number>();
  for (const c of calls) {
    const name = c.tool || "tool";
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return [...counts].map(([name, n]) => (n > 1 ? `${name} ${n}` : name)).join(", ");
}
