/**
 * Does the prefix actually stay off the wire, in a **real** xterm?
 *
 * Open http://localhost:5273/prefix-terminal-check.html and read the page (or
 * the console). `window.__prefixCheck` holds `{ failures, lines }` for a driver.
 *
 * `prefix-mode-check.ts` proves the decisions; it cannot prove the one thing
 * that matters, which is *where* the decision runs. A window listener would be
 * too late — xterm has written `\x02` to the pty from its own keydown handler
 * before a bubble listener sees the event (the trap CLAUDE.md describes for
 * Ctrl chords off macOS). So this attaches the terminal exactly as the panes
 * do, through `attachClipboard`, whose custom key handler calls the prefix gate
 * first, and watches `onData`.
 */
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import { attachClipboard } from "./src/lib/terminal-clipboard";
import { setPrefixDispatcher, usePrefixMode } from "./src/lib/prefix-runtime";
import type { Effect } from "./src/lib/prefix-mode";

let failures = 0;
const lines: string[] = [];
function check(what: string, ok: boolean) {
  lines.push(`${ok ? "PASS" : "FAIL"}  ${what}`);
  if (ok) console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

const term = new Terminal({ fontSize: 12, scrollback: 50 });
term.open(document.getElementById("t")!);
attachClipboard(term);

let sent = "";
term.onData((d) => {
  sent += d;
});
const effects: Effect[] = [];
setPrefixDispatcher((e) => effects.push(e));

const ta = () => document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
const show = (s: string) => JSON.stringify(s).replace(/\\u00/g, "\\x");

/** A keydown as a browser delivers it — `keyCode` included, which xterm needs. */
function press(type: "keydown" | "keypress" | "keyup", key: string, init: KeyboardEventInit & { keyCode?: number } = {}) {
  const keyCode = init.keyCode ?? (key.length === 1 ? key.toUpperCase().charCodeAt(0) : 0);
  const ev = new KeyboardEvent(type, {
    key,
    code: key.length === 1 && /[a-z]/i.test(key) ? `Key${key.toUpperCase()}` : key,
    bubbles: true,
    cancelable: true,
    ...init,
    keyCode,
    which: type === "keypress" ? key.charCodeAt(0) : keyCode,
    charCode: type === "keypress" ? key.charCodeAt(0) : 0,
  } as unknown as KeyboardEventInit);
  ta().focus();
  ta().dispatchEvent(ev);
  return ev;
}
const reset = () => {
  sent = "";
  effects.length = 0;
  usePrefixMode.setState({ mode: "terminal" });
};

setTimeout(() => {
  // The probe is live: a control key that must encode does.
  reset();
  press("keydown", "c", { ctrlKey: true });
  check(`the probe sees a real control key (ctrl+c → ${show(sent)})`, sent === "\x03");

  // ctrl+b without a prefix layer would be \x02. With it: nothing, and PREFIX.
  reset();
  const down = press("keydown", "b", { ctrlKey: true });
  check(`ctrl+b does NOT reach onData (got ${show(sent)})`, sent === "");
  check("ctrl+b enters PREFIX", usePrefixMode.getState().mode === "prefix");
  check("the consumed keydown is preventDefault'ed (no input event follows)", down.defaultPrevented);
  press("keyup", "b", { ctrlKey: true });
  check("its keyup leaks nothing", sent === "");

  // Second prefix: the literal byte, through xterm's own encoder.
  sent = "";
  press("keydown", "b", { ctrlKey: true });
  check(`prefix prefix sends the literal \\x02 (got ${show(sent)})`, sent === "\x02");
  check("…and returns to terminal mode", usePrefixMode.getState().mode === "terminal");

  // Prefix then v: the command runs, the letter is not typed.
  reset();
  press("keydown", "b", { ctrlKey: true });
  press("keydown", "v");
  press("keypress", "v");
  press("keyup", "v");
  check(`prefix v types nothing (got ${show(sent)})`, sent === "");
  check("prefix v runs split-right", effects.some((e) => e.kind === "run" && e.action === "pane.splitRight"));

  // Escape cancels and is not sent (it would cancel Claude's prompt).
  reset();
  press("keydown", "b", { ctrlKey: true });
  press("keydown", "Escape", { keyCode: 27 });
  check(`prefix esc sends nothing (got ${show(sent)})`, sent === "" && usePrefixMode.getState().mode === "terminal");

  // Terminal mode untouched: an ordinary letter reaches the shell exactly as it
  // does in a terminal with no prefix layer at all. Compared against a bare
  // baseline xterm fed the same synthetic events, because synthetic keydown +
  // keypress encode twice in xterm regardless (no real default action to
  // cancel) — what matters is that the gate changes nothing.
  reset();
  press("keydown", "v");
  press("keypress", "v");
  const gated = sent;
  const baseline = (() => {
    const host = document.createElement("div");
    host.style.height = "60px";
    document.body.appendChild(host);
    const bare = new Terminal({ fontSize: 12 });
    bare.open(host);
    let out = "";
    bare.onData((d) => (out += d));
    const area = host.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
    area.focus();
    for (const type of ["keydown", "keypress"] as const) {
      area.dispatchEvent(new KeyboardEvent(type, {
        key: "v", code: "KeyV", bubbles: true, cancelable: true,
        keyCode: type === "keypress" ? 118 : 86, which: type === "keypress" ? 118 : 86, charCode: type === "keypress" ? 118 : 0,
      } as unknown as KeyboardEventInit));
    }
    bare.dispose();
    host.remove();
    return out;
  })();
  check(`in terminal mode v reaches onData as in a bare xterm (gated ${show(gated)}, bare ${show(baseline)})`,
    gated.includes("v") && gated === baseline);

  // An IME keystroke is never taken, even mid-mode.
  reset();
  usePrefixMode.setState({ mode: "prefix" });
  press("keydown", "b", { ctrlKey: true, isComposing: true, keyCode: 229 } as KeyboardEventInit);
  check("a composing key is not intercepted (mode unchanged)", usePrefixMode.getState().mode === "prefix");
  reset();

  lines.push("", failures ? `${failures} CHECK(S) FAILED` : "ALL CHECKS PASSED");
  document.getElementById("out")!.textContent = lines.join("\n");
  (window as unknown as { __prefixCheck: unknown }).__prefixCheck = { failures, lines };
}, 300);
