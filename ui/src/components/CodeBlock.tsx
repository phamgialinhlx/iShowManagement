import { useEffect, useState } from "react";

import { highlight } from "../lib/monaco";

/**
 * A fenced code block, coloured by the same tokenizer the editor uses.
 *
 * ## It renders immediately, then colours
 *
 * Highlighting is async — Monaco tokenizes off the critical path and a
 * transcript can hold hundreds of blocks. So the plain text is rendered first
 * and replaced when the colours arrive, rather than showing an empty box or a
 * spinner over four lines of shell. Nothing moves when it lands: same font,
 * same metrics, only the colour changes.
 *
 * ## Unknown languages stay plain, deliberately
 *
 * A fence with no language, or one Monaco does not know, is left as text.
 * Guessing produces confidently wrong colours — a shell transcript tokenized as
 * JavaScript is worse than no colour at all, because it *looks* parsed.
 *
 * The HTML comes from `monaco.editor.colorize`, which escapes the source as it
 * tokenizes. That is what makes injecting it safe here: this text is whatever
 * Claude printed, including file contents from a machine rmux does not control.
 */
export function CodeBlock({ code, language }: { code: string; language: string | null }) {
  const [html, setHtml] = useState<string | null>(null);

  useEffect(() => {
    if (!language) {
      setHtml(null);
      return;
    }

    let cancelled = false;
    // Reset first: without this, scrolling a virtualised list would leave the
    // previous block's colours over the new block's text for a frame.
    setHtml(null);
    void highlight(code, language).then((result) => {
      if (!cancelled) setHtml(result);
    });

    return () => {
      cancelled = true;
    };
  }, [code, language]);

  const style = {
    color: "var(--text)",
    margin: 0,
    // `pre-wrap`, not `pre`: a transcript is read in a narrow pane, and a
    // horizontal scrollbar per code block makes it unreadable. Long lines wrap
    // and keep their indentation.
    whiteSpace: "pre-wrap" as const,
    tabSize: 4,
  };

  return (
    <pre className="inset data overflow-x-auto p-2 text-[11.5px] leading-[1.5]" style={style}>
      {html === null ? (
        code
      ) : (
        <code dangerouslySetInnerHTML={{ __html: html }} />
      )}
    </pre>
  );
}
