import { highlight, initMonaco, languageId } from "./src/lib/monaco";
import "./src/styles/signal-room.css";

/**
 * Does code actually come out colour-coded?
 *
 * This exists because the failure it guards is invisible to every other check.
 * The theme it replaced was syntactically valid, defined nineteen token rules,
 * and compiled — and rendered Python as one flat grey, because `keyword` and
 * `identifier` were both `#e8e6e1`. `tsc` cannot see that, a unit test on the
 * theme object would only re-assert the constants, and a screenshot is the only
 * thing that caught it. So the assertion here is the one that matters: **count
 * the distinct colours Monaco actually emitted.**
 *
 * It needs a real browser — Monaco tokenizes with workers and reads the live
 * theme service — so it runs as a page rather than in Node, like the other
 * `*-check` harnesses. Open http://localhost:5273/highlight-check.html and read
 * the console, or drive it headless: `window.__highlightCheck` resolves to the
 * results.
 */

const PYTHON = `import re

_TAG_RE = re.compile(r"<[^>]+>")  # WPBakery shortcodes

def detect(html):
    """Return (detected_lang, de_count, en_count)."""
    de = en = 0
    for w in _WORD_RE.findall(_text(html).lower()):
        if w in _GERMAN:
            de += 1
    return (("de" if de >= en else "en"), de, en, 3.5)
`;

const TYPESCRIPT = `export function total(items: Item[]): number {
  // one line of prose
  return items.reduce((sum, x) => sum + x.price, 0);
}
`;

type Result = { name: string; ok: boolean; detail: string };

/**
 * Every distinct colour the highlighted markup actually *paints*.
 *
 * Read from `getComputedStyle` after putting the HTML in the document, not from
 * the markup. That distinction is the point of this harness: `colorize` emits
 * class names (`mtk21`), and the stylesheet defining them is injected only once
 * an editor has been constructed. Checking the string would have called
 * perfectly tokenized, entirely grey output a pass — which is the bug.
 */
function colours(html: string): string[] {
  const host = document.createElement("div");
  host.style.cssText = "position:absolute;top:-9999px;left:-9999px";
  host.innerHTML = html;
  document.body.appendChild(host);

  const found = new Set<string>();
  for (const span of host.querySelectorAll("span")) {
    // Only leaves: a wrapper inherits its child's colour and would inflate the
    // count without any of it being a real distinction.
    if (span.querySelector("span")) continue;
    if (!span.textContent?.trim()) continue;
    found.add(getComputedStyle(span).color);
  }

  host.remove();
  return [...found];
}

async function run(): Promise<Result[]> {
  initMonaco();
  const results: Result[] = [];

  // Aliases people actually type in a fence, which is what the transcript gets.
  for (const [hint, expected] of [
    ["python", "python"],
    ["py", "python"],
    ["ts", "typescript"],
    ["sh", "shell"],
    ["", null],
    ["not-a-language", null],
  ] as const) {
    const got = languageId(hint);
    const ok = expected === null ? got === null : got === expected;
    results.push({
      name: `languageId(${JSON.stringify(hint)})`,
      ok,
      detail: `→ ${got ?? "null"}${ok ? "" : ` (wanted ${expected ?? "null"})`}`,
    });
  }

  for (const [language, source, floor] of [
    ["python", PYTHON, 4],
    ["typescript", TYPESCRIPT, 4],
  ] as const) {
    const html = await highlight(source, language);

    if (!html) {
      results.push({ name: `${language}: highlighted`, ok: false, detail: "returned null" });
      continue;
    }

    const distinct = colours(html);
    results.push({
      name: `${language}: distinct colours ≥ ${floor}`,
      // The whole point. One colour means the tokenizer ran and the theme threw
      // the result away — which is exactly what shipped before.
      ok: distinct.length >= floor,
      detail: `${distinct.length}: ${distinct.join(" ")}`,
    });

    // A comment must be visibly *not* the body text, or prose inside code reads
    // as code. `7e7b74` is the corrected faint grey; the old `5f5c56` was under
    // the contrast floor.
    results.push({
      name: `${language}: comment is its own colour`,
      // #7e7b74 / #8a8780 as rgb(), which is what getComputedStyle returns.
      ok: distinct.some((c) => c === "rgb(126, 123, 116)" || c === "rgb(138, 135, 128)"),
      detail: distinct.join(" "),
    });

    // Monaco escapes as it tokenizes; that is what makes injecting this HTML
    // safe for transcript text rmux did not write.
    results.push({
      name: `${language}: source is escaped`,
      ok: !/<(script|img|iframe)/i.test(html),
      detail: "no raw markup in the output",
    });
  }

  return results;
}

const results = await run();
for (const r of results) {
  // eslint-disable-next-line no-console
  console.log(`${r.ok ? "PASS" : "FAIL"}  ${r.name}  —  ${r.detail}`);
}
const failed = results.filter((r) => !r.ok).length;
// eslint-disable-next-line no-console
console.log(failed ? `${failed} FAILED` : `all ${results.length} checks passed`);

document.body.textContent = failed ? `${failed} FAILED — see console` : "all checks passed";

declare global {
  interface Window {
    __highlightCheck?: Result[];
  }
}
window.__highlightCheck = results;
