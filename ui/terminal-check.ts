/**
 * Checks the terminal clipboard bridge in a real browser.
 *
 * `attachClipboard` reads `navigator.platform` and touches the clipboard API, so
 * it cannot run in Node. The xterm instance is stubbed deliberately: what is
 * under test is the key handling this repo wrote, not xterm's own behaviour.
 */
import { attachClipboard } from "./src/lib/terminal-clipboard";
import { MouseModeTracker } from "./src/lib/mouse-modes";

const results: string[] = [];
let failures = 0;
const check = (name: string, ok: boolean, detail?: unknown) => {
  results.push(`${ok ? "PASS" : "FAIL"}  ${name}${ok ? "" : ` — ${JSON.stringify(detail)}`}`);
  if (!ok) failures += 1;
};

type Handler = (e: KeyboardEvent) => boolean;

function harness(selection: string) {
  let handler: Handler = () => true;
  const sent: string[] = [];
  let selectAllCalled = false;
  const xterm = {
    attachCustomKeyEventHandler: (h: Handler) => { handler = h; },
    getSelection: () => selection,
    selectAll: () => { selectAllCalled = true; },
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  attachClipboard(xterm as any);
  return { handler: () => handler, sent, wasSelectAll: () => selectAllCalled };
}

const key = (init: Partial<KeyboardEvent> & { key: string; type?: string }) =>
  ({ type: init.type ?? "keydown", ...init } as KeyboardEvent);

async function run() {
  const isMac = navigator.platform.toUpperCase().includes("MAC");
  const mod = isMac ? { metaKey: true } : { ctrlKey: true, shiftKey: true };

  // Copy and paste are deliberately NOT handled here — xterm and the webview
  // already implement both, and a second implementation made every paste arrive
  // twice. These two checks pin that they stay unhandled.
  const copy = harness("something selected");
  check(
    "copy is left to xterm (a second handler double-pastes its sibling)",
    copy.handler()(key({ key: "c", ...mod })) === true,
  );

  const paste = harness("");
  check("paste is left to xterm", paste.handler()(key({ key: "v", ...mod })) === true);
  check("paste does not send anything itself", paste.sent.length === 0, paste.sent);

  // Plain Ctrl+C must stay SIGINT on every platform, or nothing can be
  // interrupted.
  const sigint = harness("something selected");
  check(
    "plain ctrl+c is left alone (SIGINT)",
    sigint.handler()(key({ key: "c", ctrlKey: true })) === true,
  );

  // Select-all is the one thing xterm does not provide.
  const all = harness("");
  check("select-all is handled", all.handler()(key({ key: "a", ...mod })) === false);
  check("select-all calls into xterm", all.wasSelectAll());

  // Ordinary typing is untouched.
  const typing = harness("");
  check("plain letters pass through", typing.handler()(key({ key: "c" })) === true);
  check("keyup is ignored", typing.handler()(key({ key: "a", type: "keyup", ...mod })) === true);

  // --- mouse modes: what select mode turns off and puts back ---------------
  const t = new MouseModeTracker();
  check("nothing tracked before a program asks", !t.enabled);

  // What Claude's TUI actually emits: any-event tracking plus SGR encoding.
  t.observe("some output \x1b[?1002h\x1b[?1006h more output");
  check("tracks the modes a program enables", t.enabled);
  check(
    "disables exactly what was enabled",
    t.disableSequence() === "\x1b[?1002l\x1b[?1006l",
    t.disableSequence(),
  );
  check(
    "restores exactly what was enabled",
    t.restoreSequence() === "\x1b[?1002h\x1b[?1006h",
    t.restoreSequence(),
  );

  // Combined parameters in one sequence.
  const combined = new MouseModeTracker();
  combined.observe("\x1b[?1003;1006h");
  check("handles combined parameters", combined.disableSequence() === "\x1b[?1003l\x1b[?1006l", combined.disableSequence());

  // A program turning tracking off must be noticed, or select mode would put
  // back reporting the program no longer wants.
  const off = new MouseModeTracker();
  off.observe("\x1b[?1002h");
  off.observe("\x1b[?1002l");
  check("a program disabling tracking is noticed", !off.enabled);

  // Unrelated private modes must not be touched — 1049 is the alternate screen,
  // and resetting it would wipe the display.
  const other = new MouseModeTracker();
  other.observe("\x1b[?1049h\x1b[?25l");
  check("ignores non-mouse private modes", !other.enabled, other.disableSequence());

  // The hot path: ordinary output must cost nothing and change nothing.
  const plain = new MouseModeTracker();
  plain.observe("just some regular terminal output with no escapes");
  check("plain output tracks nothing", !plain.enabled);

  const summary = `[terminal-check] ${failures === 0 ? "ALL PASS" : `${failures} FAILED`}\n${results.join("\n")}`;
  document.getElementById("out")!.textContent = summary;
  console.log(summary);
}

void run();
