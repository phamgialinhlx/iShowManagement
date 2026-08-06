/**
 * What does macOS *actually* send when Vietnamese is typed?
 *
 * Open http://localhost:5273/ime-capture.html **in Safari**, type one Vietnamese
 * word, press COPY LOG.
 *
 * ## Why this exists rather than another simulated test
 *
 * `ime-check.ts` fires the composition events macOS was *assumed* to fire — one
 * composition per character, `compositionstart` → `compositionupdate` →
 * `compositionend` — and passes 4/4 against a real xterm. The report is that
 * Vietnamese still loses letters, and that some letters cannot be typed at all
 * with the IME active. So the assumption is what is wrong, and a test built on
 * it will keep passing while the bug keeps happening. That is the same mistake
 * that made `blackbar-check.ts` measure an unfitted terminal.
 *
 * Vietnamese is not one-composition-per-character. Telex holds a whole
 * *syllable* as marked text — `tieengs` mutates `t` → `ti` → `tie` → `tiê` →
 * `tiế` → `tiếng` — and only commits at a word boundary. Every intermediate
 * state is a replacement of the previous one, not an addition to it, so
 * anything that treats an update as new text will emit `titietiếtiếng`, and
 * anything that ignores updates but mishandles the commit will emit nothing.
 *
 * There is no way to drive a real IME from a script: marked text is produced by
 * the input method, above the DOM. So this records the ground truth instead —
 * every event, in order, with its `data`, its `isComposing`, the textarea's
 * value at that moment, and, crucially, **what xterm decided to send**.
 *
 * The last column is the answer. If `onData` shows letters that were never
 * typed, or is missing letters that were, the client is at fault and this log
 * says exactly which event it mishandled.
 */
import { Terminal as Xterm } from "@xterm/xterm";

import "@xterm/xterm/css/xterm.css";

const logEl = document.getElementById("log")!;
const sentEl = document.getElementById("sent")!;
const lines: string[] = [];

const show = (s: string) => {
  lines.push(s);
  logEl.textContent = lines.join("\n");
  logEl.scrollTop = logEl.scrollHeight;
};

/** Printable, so a combining mark or a stray control character is not invisible. */
const q = (s: string | null | undefined) =>
  s === null || s === undefined
    ? "—"
    : JSON.stringify(s) +
      (s ? `  [${[...s].map((c) => `U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}`).join(" ")}]` : "");

const term = new Xterm({
  fontFamily: '"IBM Plex Mono", ui-monospace, Menlo, monospace',
  fontSize: 13,
  lineHeight: 1.3,
  cursorBlink: true,
  theme: { background: "#000000", foreground: "#e8e6e1" },
});
term.open(document.getElementById("term")!);
term.writeln("Type a Vietnamese word here. Everything is recorded below.");
term.write("\r\n$ ");

// What xterm decided to send. This is the line that matters: it is exactly what
// rmux would have put on the wire to Claude.
const sent: string[] = [];
term.onData((d) => {
  sent.push(d);
  show(`  ${"→ onData".padEnd(20)} ${q(d)}`);
  sentEl.textContent = `onData so far: ${JSON.stringify(sent.join(""))}`;
  term.write(d);
});

const textarea = document.querySelector("#term textarea") as HTMLTextAreaElement;
if (!textarea) throw new Error("xterm has no textarea — there is no IME path to record");

const record = (name: string, e: Event) => {
  const data = (e as CompositionEvent & InputEvent).data;
  const composing = (e as InputEvent).isComposing;
  show(
    `${name.padEnd(22)} data=${q(data)}  isComposing=${composing === undefined ? "—" : composing}` +
      `  textarea=${q(textarea.value)}`,
  );
};

for (const name of ["compositionstart", "compositionupdate", "compositionend"]) {
  textarea.addEventListener(name, (e) => record(name, e), true);
}
textarea.addEventListener("beforeinput", (e) => record("beforeinput", e), true);
textarea.addEventListener("input", (e) => record("input", e), true);
// `keyCode 229` is how a browser says "this keystroke belongs to the IME". If
// letters cannot be typed at all, this is where it will show.
textarea.addEventListener(
  "keydown",
  (e) => {
    const k = e as KeyboardEvent;
    show(
      `keydown                key=${q(k.key)} code=${k.code} keyCode=${k.keyCode} ` +
        `isComposing=${k.isComposing}${k.defaultPrevented ? " (defaultPrevented)" : ""}`,
    );
  },
  true,
);

document.getElementById("copy")!.addEventListener("click", async () => {
  const report = [
    `userAgent: ${navigator.userAgent}`,
    `onData total: ${JSON.stringify(sent.join(""))}`,
    "",
    ...lines,
  ].join("\n");
  await navigator.clipboard.writeText(report);
  sentEl.textContent = "copied to clipboard";
});

document.getElementById("clear")!.addEventListener("click", () => {
  lines.length = 0;
  sent.length = 0;
  logEl.textContent = "";
  sentEl.textContent = "";
});

show("ready — click the terminal and type");
