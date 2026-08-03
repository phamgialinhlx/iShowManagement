/**
 * Does every control look like a control?
 *
 * ## The bug this exists for
 *
 * `.micro` is this app's 9px caption. It is worn by headings *and* by buttons,
 * so a button carrying nothing else is visually identical to a title — same
 * size, same weight, same colour, often the same position. The right rail's
 * INSTRUMENTS sat where the left rail puts the plain word SESSIONS, and the only
 * way to discover it was a control was to click a word that looked inert.
 *
 * Reported as "clickable label don't have underline or sth to indicate that it
 * is clickable", and then — correctly — as "you need to scan all our UI for
 * that, because I see not just in one place". There were 61 of them.
 *
 * ## Why this reads source rather than the rendered page
 *
 * The rendered check would be better in principle and useless in practice: most
 * of these controls live behind a signed-in account, a connected host, a file
 * being open, or a confirmation already clicked. A DOM sweep would visit a
 * fraction of them and report the rest as passing, which is the failure mode
 * that let 61 accumulate. The source is the one place all of them are visible at
 * once.
 *
 * Vite's `import.meta.glob(..., { query: '?raw' })` hands the real files over,
 * so this cannot drift from what ships the way a copied list would.
 */

const SOURCES = import.meta.glob("./src/**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

/** Classes that make a control legible as one. */
const AFFORDANCES = ["chip", "seg", "link", "btn"];

type Finding = { file: string; line: number; snippet: string };

/**
 * Buttons are matched with their whole opening tag, braces included, because
 * these tags routinely carry multi-line handlers and a line-based scan would
 * split them and miss the className.
 */
const BUTTON = /<button\b(?:[^<>]|\{[^{}]*\})*?>/gs;

export function audit(): Finding[] {
  const findings: Finding[] = [];

  for (const [file, src] of Object.entries(SOURCES)) {
    for (const match of src.matchAll(BUTTON)) {
      const tag = match[0];
      const className = /className="([^"]*)"/.exec(tag)?.[1] ?? "";

      // Already a recognised control shape.
      if (AFFORDANCES.some((c) => new RegExp(`\\b${c}\\b`).test(className))) continue;
      // Inside a segmented group the frame is on the parent, and those children
      // are styled by `.seg > button` rather than by a class of their own.
      if (/aria-pressed/.test(tag) && !/\bmicro\b/.test(className)) continue;
      // A hand-rolled frame still frames it. Not preferred — it duplicates
      // `.chip` — but it is not the defect this guards.
      if (/border/.test(tag)) continue;
      // An icon-only button is a shape, not a word pretending to be a label.
      // Those are matched by having no text class at all.
      if (!/\bmicro\b/.test(className)) continue;

      findings.push({
        file: file.replace("./src/", ""),
        line: src.slice(0, match.index).split("\n").length,
        snippet: className || "(no className)",
      });
    }
  }

  return findings;
}

export function run(log: (line: string) => void): boolean {
  const findings = audit();
  const files = Object.keys(SOURCES).length;

  log(`  scanned ${files} component files`);
  log("");

  if (!findings.length) {
    log("  PASS  every button carries a visible affordance");
    log("");
    log("ALL CHECKS PASSED");
    return true;
  }

  log(`  FAIL  ${findings.length} button(s) wear only a caption class:`);
  for (const f of findings) log(`          ${f.file}:${f.line}  [${f.snippet}]`);
  log("");
  log("  A button styled only with `.micro` is indistinguishable from a heading.");
  log("  Use `.chip` for a labelled action, `.seg` for a segmented group, or");
  log("  `.link` where a border would enclose a full-width value.");
  log("");
  log("CHECKS FAILED");
  return false;
}
