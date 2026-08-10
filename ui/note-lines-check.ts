/**
 * Does a rendered note block point back at the right source line?
 *
 * Open http://localhost:5273/note-lines-check.html and read the console.
 *
 * This is what keeps the reader's place when the note switches between its two
 * faces. Rendered markdown and raw source are different scrollers over different
 * content, so a pixel offset cannot survive the switch — a *line* can. Getting
 * the line wrong is worse than not moving at all: it puts the caret somewhere
 * plausible and somewhere else.
 *
 * `react-markdown` strips `node.position`, so there is no parser answer to
 * check against; the mapping is done by matching text in document order. The
 * risky part is repeated lines, which is what most of this file is about.
 */
import { linesForBlocks, lineOfOffset, offsetOfLine, plainOf } from "./src/lib/note-lines";

let failures = 0;
const check = (name: string, ok: boolean, detail = "") => {
  if (ok) console.log(`%c PASS %c ${name}${detail ? ` — ${detail}` : ""}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name}${detail ? ` — ${detail}` : ""}`);
  }
};

// What markdown removes on the way to the screen.
check("a bullet is stripped", plainOf("- deploy the thing") === "deploy the thing");
check("a task box is stripped", plainOf("- [ ] deploy the thing") === "deploy the thing");
check("a ticked box too", plainOf("- [x] deploy the thing") === "deploy the thing");
check("a heading is stripped", plainOf("### Today") === "Today");
check("emphasis is stripped", plainOf("**bold** and `code`") === "bold and code");
check("a link keeps its label", plainOf("see [the docs](https://x.test/y)") === "see the docs");
check("a quote is stripped", plainOf("> quoted") === "quoted");
check("an ordered item is stripped", plainOf("2. second") === "second");

// The ordinary case.
{
  const text = "# Today\n\n- [ ] ship it\n- [x] write it up\n\nA closing thought.";
  const lines = linesForBlocks(text, ["Today", "ship it", "write it up", "A closing thought."]);
  check("blocks map to their source lines", JSON.stringify(lines) === "[0,2,3,5]", JSON.stringify(lines));
}

// **The one that matters.** Identical lines must map to their own occurrences,
// not all to the first — which is what a plain indexOf would do.
{
  const text = "- retry\n- retry\n- retry";
  const lines = linesForBlocks(text, ["retry", "retry", "retry"]);
  check("repeated lines map forward, not all to the first",
    JSON.stringify(lines) === "[0,1,2]", JSON.stringify(lines));
}

// A wrapped paragraph renders as one block spanning several source lines.
{
  const text = "para one continues\nacross two lines\n\n- after";
  const lines = linesForBlocks(text, ["para one continues across two lines", "after"]);
  check("a wrapped paragraph takes its first line",
    JSON.stringify(lines) === "[0,3]", JSON.stringify(lines));
}

// Something unplaceable reports -1 rather than guessing.
{
  const lines = linesForBlocks("- only this", ["only this", "nothing like this in the source"]);
  check("an unmatched block reports -1", JSON.stringify(lines) === "[0,-1]", JSON.stringify(lines));
}

// Offsets round-trip, since the caret is placed with them.
{
  const text = "alpha\nbeta\ngamma";
  check("offsetOfLine finds a line start", offsetOfLine(text, 2) === 11, String(offsetOfLine(text, 2)));
  check("lineOfOffset is its inverse", lineOfOffset(text, 11) === 2, String(lineOfOffset(text, 11)));
  check("a caret inside a line still reports that line", lineOfOffset(text, 13) === 2);
  check("line 0 starts at 0", offsetOfLine(text, 0) === 0);
  check("past the end is clamped", offsetOfLine(text, 99) === text.length);
}

console.log(
  failures === 0
    ? "%c ALL PASS %c a rendered block points back at the line it came from"
    : `%c ${failures} FAILED %c`,
  failures === 0 ? "background:#2b7;color:#000" : "background:#e63b2e;color:#fff",
  "",
);
