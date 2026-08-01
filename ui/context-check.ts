import { compactTokens, contextLimit, sniffWindow } from "./src/lib/context-window";

/**
 * Checks for the context-window arithmetic.
 *
 * Open `http://localhost:5273/context-check.html` and read the console.
 *
 * The failure this guards is subtle and unreportable at runtime: a *wrong*
 * denominator. "18% used" against the wrong window tells someone they have room
 * they do not have, and they only find out when a turn is rejected.
 */

let failures = 0;
const check = (name: string, ok: boolean) => {
  if (ok) console.log(`ok   ${name}`);
  else {
    failures += 1;
    console.error(`FAIL ${name}`);
  }
};

// A name that spells the window out beats everything else.
check("a [1m] model reports a million", contextLimit("claude-opus-5[1m]", 5_000) === 1_000_000);
check("case does not matter", contextLimit("claude-opus-5[1M]", 5_000) === 1_000_000);

// A model name must NOT imply a window. Verified against a real transcript:
// Claude records `"model":"claude-opus-5"` whether it is running in a 200k or
// a 1M window, so assuming 200k reported 80k of 1M as "40% used" — a reason to
// compact when 900k tokens were still free.
check("a model name alone implies nothing", contextLimit("claude-sonnet-5", 5_000) === null);
check("opus-5 implies nothing", contextLimit("claude-opus-5", 83_200) === null);

// What the operator configures is a fact, not an inference.
check("a configured window is used", contextLimit("claude-opus-5", 83_200, 1_000_000) === 1_000_000);
check("200k configured is used", contextLimit("claude-opus-5", 83_200, 200_000) === 200_000);

// …but a conversation that outgrew the configured window disproves it, and the
// measurement must win over the setting.
check(
  "an observation past the configured window wins",
  contextLimit("claude-opus-5", 626_377, 200_000) === 1_000_000,
);

// …but must never narrow a window the observation has already disproved.
check(
  "an observation past 200k wins over the name",
  contextLimit("claude-opus-5", 626_377) === 1_000_000,
);

// An unknown model with a small observation proves nothing, and says so.
check("an unknown model reports nothing", contextLimit("some-other-model", 5_000) === null);
check("no model at all reports nothing", contextLimit(undefined, 5_000) === null);

// Past the largest known window, report the largest known.
check("beyond every known window", contextLimit(undefined, 2_000_000) === 1_000_000);

check("83.2k reads as 83.2k", compactTokens(83_200) === "83.2k");
check("626377 reads as 626k", compactTokens(626_377) === "626k");
check("a million reads as 1.00M", compactTokens(1_000_000) === "1.00M");

// ---------------------------------------------------------------- sniffing
//
// Claude prints the window beside the model, which is the only place it is
// stated outright. Reading it turns the denominator from an inference into an
// observation — and removes the setting for anyone whose Claude says it.

check("reads a million from the banner", sniffWindow("Opus 5 (1M context)") === 1_000_000);
check("reads 200k", sniffWindow("Sonnet 5 (200k context)") === 200_000);
check("case does not matter", sniffWindow("opus 5 (1m CONTEXT)") === 1_000_000);
check("tolerates extra spacing", sniffWindow("Opus 5 (1 M  context)") === 1_000_000);
check("finds it mid-screen", sniffWindow("│ ⏵ Opus 5 (1M context) · 12% │") === 1_000_000);

// The scope is the point. This runs over raw PTY bytes, which include whatever
// the operator and Claude are *writing about* — so only the exact parenthesised
// form beside the word `context` may move the denominator.
check("a bare mention proves nothing", sniffWindow("we should support a 1M window") === null);
check("a number alone proves nothing", sniffWindow("(1M)") === null);
check("the word must follow", sniffWindow("(1M tokens)") === null);

// Chunk boundaries split lines, so implausible sizes are parse artefacts.
check("an absurdly small window is refused", sniffWindow("(5k context)") === null);
check("an absurdly large window is refused", sniffWindow("(90M context)") === null);
check("nothing in the text reports nothing", sniffWindow("just some output\r\n") === null);

// A sniffed window is handed to `contextLimit` as `configured`, so the rule
// that a measurement outgrowing it wins still applies.
check(
  "an observation still beats a sniffed window",
  contextLimit("claude-opus-5", 626_377, sniffWindow("Sonnet 5 (200k context)") ?? undefined) ===
    1_000_000,
);

console.log(failures === 0 ? "\nall context checks passed" : `\n${failures} context check(s) FAILED`);
