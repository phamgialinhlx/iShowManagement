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

const term = new Terminal({ fontSize: 12, scrollback: 100, allowTransparency: true });
term.open(document.getElementById("t")!);
term.write("hello world this is selectable text\r\nsecond line of output\r\n");

let handlerRan = false;
attachClipboard(term);
// Wrap so we can tell whether xterm ever calls our handler.
term.attachCustomKeyEventHandler((e) => {
  if (e.type === "keydown" && (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "c") {
    handlerRan = true;
  }
  return true;
});

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

  // 4. Does a Cmd+C keydown reach xterm's custom handler?
  textarea?.focus();
  textarea?.dispatchEvent(new KeyboardEvent("keydown", { key: "c", metaKey: true, bubbles: true, cancelable: true }));
  say(`custom key handler saw Cmd+C: ${handlerRan}`);

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
