/**
 * Which source line a rendered block of the note came from.
 *
 * The note has two faces — rendered markdown and the raw source — and switching
 * between them threw away the reader's place: click to edit and the editor
 * opened at the top; click away and the rendered note went back to the top.
 * Both are the same missing fact, in opposite directions: *which line was I
 * looking at?*
 *
 * ## Why not the parser's own positions
 *
 * Because there are none. `react-markdown` strips `node.position` — measured in
 * this codebase already, and recorded where the task checkboxes resolve their
 * index: every node reported line 0. The checkboxes solved it by using
 * **document order**, which is parse order, and needs no bookkeeping to stay
 * correct.
 *
 * This is that idea generalised. Rendered blocks appear in source order, so the
 * source is scanned *forward only*, one block at a time. That is what makes
 * repeated text safe: two identical list items match their own occurrences
 * rather than both matching the first.
 *
 * A block that cannot be matched — a table cell, something inside HTML — simply
 * reports `-1`, and the caller falls back to leaving the position alone. Being
 * approximately right about where someone was reading is worth a lot; being
 * confidently wrong is worth less than doing nothing.
 */

/**
 * A source line reduced to the text a reader sees.
 *
 * Only the syntax that markdown *removes* from the output is stripped, because
 * the comparison is against rendered text. Emphasis markers, code ticks, list
 * bullets, task boxes, heading hashes, block quotes, and links reduced to their
 * label.
 */
export function plainOf(line: string): string {
  return (
    line
      // Leading block syntax: quote marks, heading hashes, bullets, numbers, and
      // a task box, in the order they may legally appear.
      .replace(/^\s*>+\s*/, "")
      .replace(/^\s*#{1,6}\s+/, "")
      .replace(/^\s*(?:[-*+]|\d+[.)])\s+/, "")
      .replace(/^\[[ xX/]\]\s*/, "")
      // `[label](target)` → `label`, before the bracket characters go.
      .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
      // Emphasis and code. Repeated rather than global-once so `***both***`
      // collapses rather than leaving a stray marker.
      .replace(/[*_`~]/g, "")
      .replace(/\s+/g, " ")
      .trim()
  );
}

/** The same reduction, for text taken out of the DOM. */
const plainRendered = (s: string): string => s.replace(/\s+/g, " ").trim();

/**
 * Source line (0-based) for each rendered block, in document order.
 *
 * `-1` where a block could not be placed.
 */
export function linesForBlocks(text: string, blocks: readonly string[]): number[] {
  const lines = text.split("\n");
  const reduced = lines.map(plainOf);

  const out: number[] = [];
  // Forward-only: the cursor never rewinds, so identical blocks land on their
  // own occurrences and a mismatch cannot drag every later block backwards.
  let cursor = 0;

  for (const block of blocks) {
    const want = plainRendered(block);
    if (!want) {
      out.push(-1);
      continue;
    }

    let found = -1;
    for (let i = cursor; i < reduced.length; i += 1) {
      const line = reduced[i]!;
      if (!line) continue;
      // A rendered block may join several source lines (a wrapped paragraph),
      // so the *source* line is matched as a prefix of the block rather than
      // the other way round.
      if (want === line || want.startsWith(line) || line.startsWith(want)) {
        found = i;
        break;
      }
    }

    out.push(found);
    if (found >= 0) cursor = found + 1;
  }

  return out;
}

/** Character offset where a 0-based line starts. */
export function offsetOfLine(text: string, line: number): number {
  if (line <= 0) return 0;
  const lines = text.split("\n");
  let at = 0;
  for (let i = 0; i < Math.min(line, lines.length); i += 1) at += lines[i]!.length + 1;
  return Math.min(at, text.length);
}

/** 0-based line containing a character offset. */
export function lineOfOffset(text: string, offset: number): number {
  const upto = text.slice(0, Math.max(0, Math.min(offset, text.length)));
  return upto.split("\n").length - 1;
}
