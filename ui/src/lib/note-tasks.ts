/**
 * Checkboxes in a note are tasks.
 *
 * The note is already where people write "- [ ] ask ops about the cert" — this
 * makes that line mean something rather than being decoration. Pure functions
 * over the note's text, deliberately: the note widget renders them, the
 * dashboard counts them across every session, and two implementations of "what
 * is a task" would disagree within a week.
 *
 * **The markdown source stays the source of truth.** Ticking a box rewrites the
 * line in the note's own text rather than keeping a parallel list of task
 * states. A separate structure would drift the moment anyone edited the text by
 * hand — deleting a line would leave an orphaned "done" somewhere, and the
 * count would be right about a task that no longer exists.
 */

/** One checkbox found in a note. */
export type NoteTask = {
  /** Index into the note's lines, so a toggle can rewrite exactly this one. */
  line: number;
  done: boolean;
  /** The text after the checkbox, with the marker stripped. */
  label: string;
};

/**
 * GFM task syntax, as `remark-gfm` accepts it.
 *
 * Leading whitespace is allowed so nested lists count. The bullet may be `-`,
 * `*` or `+`, and the box may hold anything non-blank to mean done — Obsidian
 * and friends use `x`, `X` and `/`, and treating an unfamiliar mark as "not a
 * task" would silently drop it from the count.
 */
const TASK = /^(\s*)([-*+])\s+\[([ xX/])\]\s?(.*)$/;

/** Every checkbox in a note, in document order. */
export function noteTasks(text: string): NoteTask[] {
  const out: NoteTask[] = [];
  text.split("\n").forEach((line, index) => {
    const m = TASK.exec(line);
    if (m) out.push({ line: index, done: m[3] !== " ", label: (m[4] ?? "").trim() });
  });
  return out;
}

/** Done and total, the shape a progress bar wants. */
export function taskProgress(text: string): { done: number; total: number } {
  const tasks = noteTasks(text);
  return { done: tasks.filter((t) => t.done).length, total: tasks.length };
}

/**
 * Flip the checkbox on one line and return the whole new text.
 *
 * Returns the text unchanged when the line is not a task, rather than throwing
 * or inserting one: the caller is a click handler on a rendered checkbox, and a
 * stale index (the note edited in another window between render and click) must
 * not corrupt the note.
 *
 * The indent, bullet and trailing text are all preserved from the match — the
 * line is rewritten, never reformatted. Someone who indents with three spaces
 * gets their three spaces back.
 */
export function toggleTask(text: string, line: number, done?: boolean): string {
  const lines = text.split("\n");
  const target = lines[line];
  if (target === undefined) return text;
  const m = TASK.exec(target);
  if (!m) return text;

  const next = done ?? m[3] === " ";
  lines[line] = `${m[1]}${m[2]} [${next ? "x" : " "}] ${m[4]}`;
  return lines.join("\n");
}

/**
 * Continue a list when Enter is pressed at the end of a list line.
 *
 * The Notion behaviour people expect: finish `- [ ] one`, press Enter, and the
 * next line already starts `- [ ] `. Without it every task costs six keystrokes
 * of punctuation and people stop writing them.
 *
 * Returns `null` when the line is not a list item, so the caller leaves the
 * default newline alone.
 *
 * **An empty item ends the list instead of continuing it.** Pressing Enter on a
 * bare `- [ ] ` should get you out, not give you a second empty box — otherwise
 * the only way to stop is to backspace over punctuation you did not type.
 */
export function continueList(line: string): string | null {
  const task = TASK.exec(line);
  if (task) return (task[4] ?? "").trim() ? `${task[1]}${task[2]} [ ] ` : null;

  const bullet = /^(\s*)([-*+])\s+(.*)$/.exec(line);
  if (bullet) return (bullet[3] ?? "").trim() ? `${bullet[1]}${bullet[2]} ` : null;

  const numbered = /^(\s*)(\d+)([.)])\s+(.*)$/.exec(line);
  if (numbered) {
    if (!(numbered[4] ?? "").trim()) return null;
    return `${numbered[1]}${Number(numbered[2]) + 1}${numbered[3]} `;
  }
  return null;
}
