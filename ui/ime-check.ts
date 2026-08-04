/**
 * Does Vietnamese survive the client half?
 *
 * Open http://localhost:5273/ime-check.html and read the console.
 *
 * The report is "typing Vietnamese in the Claude pane does not work on macOS",
 * and there are two completely different places that can break:
 *
 * 1. **The client.** macOS composes Vietnamese through an input method — typing
 *    `a` then `s` in Telex produces one `á`, delivered as `compositionstart` →
 *    `compositionupdate`* → `compositionend`, not as keystrokes. If xterm sends
 *    the intermediate states, the far side receives `aá` or `aas`; if it sends
 *    nothing, the character never arrives at all.
 * 2. **The far side.** A shell with no `LANG` runs in the POSIX locale, where
 *    the bytes are not a character. rmux sets no locale anywhere.
 *
 * These need opposite fixes, so guessing between them is worth nothing. This
 * harness settles the first one against a **real** `@xterm/xterm`, constructed
 * with the Claude pane's exact options, by firing the composition events macOS
 * actually fires and recording what `onData` emits.
 *
 * A stub could not answer this: the whole question is what xterm's own
 * CompositionHelper does with these events.
 */
import { Terminal as Xterm } from "@xterm/xterm";

let failures = 0;
const check = (name: string, ok: boolean, detail: string) => {
  if (ok) console.log(`%c PASS %c ${name} — ${detail}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name} — ${detail}`);
  }
};

const host = document.createElement("div");
host.style.cssText = "width:800px;height:300px";
document.body.append(host);

// The Claude pane's options, verbatim — an IME bug can hide in any of them.
const xterm = new Xterm({
  allowTransparency: true,
  fontFamily: '"IBM Plex Mono", ui-monospace, Menlo, monospace',
  fontSize: 12,
  lineHeight: 1.3,
  cursorBlink: true,
  scrollback: 5000,
});
xterm.open(host);

const sent: string[] = [];
xterm.onData((d) => sent.push(d));

const textarea = host.querySelector("textarea");
if (!textarea) throw new Error("xterm has no textarea — the IME path does not exist");

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * Type one composed character the way macOS delivers it.
 *
 * `steps` are the intermediate marked-text states the IME shows while you are
 * still typing; `final` is what it commits. Telex `as` → steps `["a"]`, final
 * `"á"`.
 */
async function compose(steps: string[], final: string) {
  textarea!.focus();
  // xterm clears its own textarea after committing a composition. The harness
  // has to do the same, or the second character is diffed against the first
  // one's leftovers and nothing is emitted — which looks exactly like the bug
  // under investigation and is not it.
  textarea!.value = "";
  await sleep(0);
  textarea!.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));

  for (const s of steps) {
    textarea!.value = s;
    textarea!.dispatchEvent(new CompositionEvent("compositionupdate", { data: s, bubbles: true }));
    textarea!.dispatchEvent(new InputEvent("input", { data: s, isComposing: true, bubbles: true }));
    await sleep(5);
  }

  textarea!.value = final;
  textarea!.dispatchEvent(new CompositionEvent("compositionend", { data: final, bubbles: true }));
  textarea!.dispatchEvent(new InputEvent("input", { data: final, isComposing: false, bubbles: true }));
  // xterm's CompositionHelper defers the send to a timer so the textarea has
  // settled; give it more than one frame.
  await sleep(0);
}

await (async () => {
  // Telex: a + s -> á
  sent.length = 0;
  await compose(["a"], "á");
  await sleep(60);
  check(
    "one composed vowel arrives once",
    sent.join("") === "á",
    `onData emitted ${JSON.stringify(sent.join(""))} — "aá" would mean the marked text was sent too`,
  );

  // The hard one: a word where nearly every character composes.
  sent.length = 0;
  for (const [steps, final] of [
    [["T"], "T"],
    [["i"], "i"],
    [["e"], "ế"],
    [["n"], "n"],
    [["g"], "g"],
  ] as [string[], string][]) {
    await compose(steps, final);
  }
  await sleep(60);
  check(
    "a whole Vietnamese word survives",
    sent.join("") === "Tiếng",
    `onData emitted ${JSON.stringify(sent.join(""))}`,
  );

  // A character outside the BMP-adjacent range, to prove the encoder is not
  // truncating to one byte somewhere.
  sent.length = 0;
  await compose(["đ"], "đ");
  await sleep(60);
  check(
    "a multi-byte character is not truncated",
    sent.join("") === "đ",
    `onData emitted ${JSON.stringify(sent.join(""))} (đ is 2 bytes in UTF-8)`,
  );

  // Plain ASCII must still be unaffected — the control case.
  sent.length = 0;
  textarea!.dispatchEvent(new InputEvent("input", { data: "x", isComposing: false, bubbles: true }));
  await sleep(30);
  check(
    "the non-IME path is untouched",
    !sent.join("").includes("á"),
    `ordinary typing emitted ${JSON.stringify(sent.join(""))}`,
  );

  console.log(
    failures ? `%c ${failures} FAILED — the CLIENT is at fault ` : "%c CLIENT IS CLEAN ",
    `background:${failures ? "#e63b2e" : "#2b7"};color:#000;font-weight:bold`,
  );
  if (!failures) {
    console.log(
      "%c → then the fault is on the far side: no LANG is set anywhere in rmux, so a shell " +
        "spawned by the agent daemon runs in the POSIX locale.",
      "color:#e8c15a",
    );
  }
})();
