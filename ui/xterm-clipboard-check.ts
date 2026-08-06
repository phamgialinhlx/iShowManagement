/**
 * What a REAL xterm 6 actually does with copy, selection and mouse tracking.
 *
 * The earlier clipboard test used a stubbed terminal, so it proved the key
 * handling this repo wrote and nothing about whether xterm ever sees the key, or
 * whether a selection can be made while a program has mouse reporting on. Those
 * are the two things that decide whether Cmd-C works in the Claude tab.
 */
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { attachClipboard, copyAll, copySelection, copyViewport } from "./src/lib/terminal-clipboard";
import { MouseModeTracker } from "./src/lib/mouse-modes";

const lines: string[] = [];
const say = (s: string) => lines.push(s);

/**
 * A keydown xterm will actually act on.
 *
 * **`keyCode` is not optional here, however deprecated it is.** xterm maps a key
 * to a control code through the legacy `keyCode`, so a synthetic event without
 * one is `keyCode: 0` and evaluates to nothing — Ctrl+C then produces no `\x03`
 * and the probe reports "SIGINT was not sent" for a terminal that is working
 * perfectly. That is a false alarm on the one assertion that must not be got
 * wrong, so the event is built the way the browser builds it.
 */
const keyEvent = (extra: Record<string, unknown>) =>
  new KeyboardEvent("keydown", {
    key: "c",
    code: "KeyC",
    keyCode: 67,
    which: 67,
    bubbles: true,
    cancelable: true,
    ...extra,
  } as unknown as KeyboardEventInit);

const term = new Terminal({ fontSize: 12, scrollback: 100, allowTransparency: true });
term.open(document.getElementById("t")!);
term.write("hello world this is selectable text\r\nsecond line of output\r\n");

// **`attachCustomKeyEventHandler` sets one handler; it does not add one.**
//
// This used to call it a second time "to wrap" the handler `attachClipboard`
// installs, with a comment saying so. It does not wrap it — xterm keeps a single
// `_customKeyEventHandler` field, so the second call *replaced* rmux's handler
// and every key assertion below was measuring the probe rather than the code
// under test. Worth recording as a finding in its own right: anything that
// attaches a handler after `attachClipboard` silently disables the terminal's
// copy and select-all shortcuts.
//
// So there is exactly one handler now, and it is the real one. Whether xterm
// reaches it is proven by behaviour further down — if Ctrl+C copies a selection
// instead of sending an interrupt, the handler plainly ran.
attachClipboard(term);

setTimeout(async () => {
  // 1. Can xterm produce a selection at all, and can we read it?
  term.selectAll();
  const selection = term.getSelection();
  say(`selection via API: ${JSON.stringify(selection.slice(0, 30))} (len ${selection.length})`);
  say(`hasSelection(): ${term.hasSelection()}`);

  // 2. Is there a DOM selection the browser could copy natively? This is the
  //    crux: if xterm's selection is invisible to the DOM, the OS "Copy" has
  //    nothing to act on and only an explicit handler can work.
  const domSel = window.getSelection()?.toString() ?? "";
  say(`DOM selection: ${JSON.stringify(domSel.slice(0, 30))} (len ${domSel.length})`);

  // 3. Does xterm register a `copy` listener that fills the clipboard?
  const textarea = document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement | null;
  say(`helper textarea present: ${!!textarea}`);
  let copyEventData = "";
  if (textarea) {
    const ev = new ClipboardEvent("copy", { bubbles: true, cancelable: true, clipboardData: new DataTransfer() });
    textarea.dispatchEvent(ev);
    copyEventData = ev.clipboardData?.getData("text/plain") ?? "";
  }
  say(`xterm fills a copy event: ${JSON.stringify(copyEventData.slice(0, 30))} (len ${copyEventData.length})`);

  // 4. Does a Cmd+C keydown survive to the platform, rather than being claimed?
  //
  // This is the asymmetry behind the reported bug. Cmd-C is not a terminal
  // control key, so xterm does not consume it and the native `copy` event above
  // does the work. Ctrl+C *is* one: xterm claims it for SIGINT and calls
  // preventDefault, so no `copy` event is ever raised — which is why right-click
  // › Copy worked and Ctrl+C did nothing.
  // Measured with **nothing selected**, which is the state the bug report
  // describes and the only one where the asymmetry is visible: with a selection
  // rmux's own handler takes Ctrl+C, so xterm never gets to claim it.
  term.clearSelection();
  textarea?.focus();
  const cmdC = keyEvent({ metaKey: true });
  textarea?.dispatchEvent(cmdC);
  say(`cmd+c left for the platform (not consumed): ${!cmdC.defaultPrevented}`);

  const ctrlC = keyEvent({ ctrlKey: true });
  textarea?.dispatchEvent(ctrlC);
  say(`ctrl+c claimed by the terminal: ${ctrlC.defaultPrevented}`);

  // --- the buttons the Claude tab exposes, which need no selection ---------
  let copied = "";
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: async (t: string) => { copied = t; } },
  });

  term.clearSelection();
  copied = "";
  const hadSelection = await copySelection(term);
  say(`copySelection with nothing selected: returned ${hadSelection}, wrote ${copied.length} chars`);

  copied = "";
  await copyViewport(term);
  const viewportOk = copied.includes("hello world") && copied.includes("second line");
  say(`copyViewport without any selection: ${viewportOk ? "got the screen" : JSON.stringify(copied.slice(0,40))}`);

  copied = "";
  await copyAll(term);
  say(`copyAll: ${copied.includes("hello world") ? "got the scrollback" : JSON.stringify(copied.slice(0,40))}`);

  term.selectAll();
  copied = "";
  const had2 = await copySelection(term);
  say(`copySelection with a selection: returned ${had2}, wrote ${copied.length} chars`);

  // --- Ctrl+C: copies a selection, and still interrupts without one ---------
  //
  // The reported bug was that highlight + right-click › Copy worked while
  // Ctrl+C did nothing. The cause is that xterm claims Ctrl+C as SIGINT and
  // calls preventDefault, so the browser never raises a `copy` event for it —
  // whereas Cmd-C is not a control key and reaches the native path untouched.
  //
  // Only meaningful where the binding exists; elsewhere Ctrl+Shift+C is the
  // copy key and plain Ctrl+C must remain SIGINT, which is what the second
  // half asserts on every platform.
  const isWindows = navigator.platform.toUpperCase().includes("WIN");
  let sent = "";
  const tap = term.onData((d) => { sent += d; });

  const pressCtrlC = () => {
    const ta = document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement | null;
    ta?.focus();
    ta?.dispatchEvent(keyEvent({ ctrlKey: true }));
  };

  // With a selection: it must copy, and must NOT send an interrupt.
  term.selectAll();
  copied = "";
  sent = "";
  pressCtrlC();
  await new Promise((r) => setTimeout(r, 120));
  say(
    `ctrl+c with a selection: copied ${copied.length} chars, sent ${JSON.stringify(sent)}` +
      (isWindows ? "" : " (binding is windows-only; no copy expected here)"),
  );
  if (isWindows) {
    say(`  copied the selection: ${copied.length > 0}`);
    say(`  suppressed the interrupt: ${!sent.includes("\x03")}`);
    // And the selection is dropped, so the *next* Ctrl+C interrupts rather than
    // copying the same stale text — a terminal you cannot interrupt is a much
    // worse bug than one that needs the key pressed twice.
    say(`  cleared the selection afterwards: ${!term.hasSelection()}`);
  }

  // With nothing selected: it must be an ordinary interrupt, everywhere.
  term.clearSelection();
  copied = "";
  sent = "";
  pressCtrlC();
  await new Promise((r) => setTimeout(r, 120));
  say(`ctrl+c with no selection sends SIGINT: ${sent.includes("\x03")} (${JSON.stringify(sent)})`);
  tap.dispose();

  // --- select mode: does disabling reporting locally actually stop it? ------
  const tracker = new MouseModeTracker();
  let reports = 0;
  term.onData((d) => {
    // A mouse report in SGR encoding: ESC [ < b ; x ; y M|m
    if (/\x1b\[</.test(d)) reports += 1;
  });

  const screen = document.querySelector(".xterm-screen") as HTMLElement | null;
  const drag = () => {
    const opts = { bubbles: true, cancelable: true, clientX: 40, clientY: 40, button: 0 };
    screen?.dispatchEvent(new MouseEvent("mousedown", opts));
    screen?.dispatchEvent(new MouseEvent("mousemove", { ...opts, clientX: 90 }));
    screen?.dispatchEvent(new MouseEvent("mouseup", { ...opts, clientX: 90 }));
  };

  // Turn tracking on exactly as Claude's TUI does, and record it.
  const enable = "\x1b[?1002h\x1b[?1006h";
  tracker.observe(enable);
  term.write(enable);
  await new Promise((r) => setTimeout(r, 80));

  reports = 0;
  drag();
  await new Promise((r) => setTimeout(r, 60));
  const whileTracking = reports;
  say(`mouse reports sent while tracking is on: ${whileTracking}`);

  // Now the thing select mode does.
  term.write(tracker.disableSequence());
  await new Promise((r) => setTimeout(r, 80));
  reports = 0;
  drag();
  await new Promise((r) => setTimeout(r, 60));
  say(`mouse reports after select mode turns it off: ${reports}`);
  say(`select mode works: ${whileTracking > 0 && reports === 0}`);

  // And restoring puts it back, or the mouse would stay dead in Claude.
  term.write(tracker.restoreSequence());
  await new Promise((r) => setTimeout(r, 80));
  reports = 0;
  drag();
  await new Promise((r) => setTimeout(r, 60));
  say(`mouse reports after restoring: ${reports}`);

  const summary = `[xterm-probe]\n${lines.join("\n")}`;
  document.getElementById("out")!.textContent = summary;
  console.log(summary);
}, 400);
