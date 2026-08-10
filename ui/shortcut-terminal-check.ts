/**
 * Which default shortcuts does a real terminal also act on?
 *
 * Open http://localhost:5273/shortcut-terminal-check.html and read the console.
 *
 * ## Why this exists, and why `shortcuts-check` could not catch it
 *
 * `useShortcuts` listens in the **bubble** phase on purpose: xterm sees the key
 * first, and rmux only intervenes on chords the terminal does not use. That
 * reasoning is sound on macOS, where `Mod` is ⌘ and xterm sends nothing at all
 * for a ⌘ chord — so "the terminal does its thing" is a no-op and only the
 * shortcut runs.
 *
 * Off macOS `Mod` is **Ctrl**, and Ctrl chords are exactly the ones a terminal
 * claims. `preventDefault` on the bubble is far too late: xterm wrote to the pty
 * from its own keydown handler, several frames earlier. So a colliding default
 * does not merely lose — it fires *both*. `Mod+P` recalls the previous command
 * in every readline shell *and* opens the Progress page.
 *
 * `shortcuts-check.ts` cannot see this. It asserts the defaults avoid ⌘W/⌘Q/⌘N/⌘M
 * — the macOS menu keys — and has no equivalent for the platform where `Mod` is
 * Ctrl. Both halves of that failure are invisible to a stub: only a real xterm
 * knows which chords it encodes.
 *
 * So this asks the terminal instead of assuming. Anything that puts bytes on the
 * wire is a chord a default must not use.
 */
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import { DEFAULTS, hasModifier, isMac, normalise, reachesTerminal } from "./src/lib/shortcuts";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) {
    console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  } else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

/**
 * The legacy `keyCode` for a chord's key.
 *
 * **Not optional, however deprecated.** xterm maps a key to a control code
 * through `keyCode`; a synthetic event without one is `keyCode: 0` and encodes
 * to nothing, so every chord would look free and this probe would cheerfully
 * pass on a terminal it had never actually asked. That exact false negative was
 * found in `xterm-clipboard-check.ts`, where a missing `keyCode` reported a
 * terminal as unable to send SIGINT when it was fine.
 */
const KEY_CODES: Record<string, number> = {
  ArrowLeft: 37,
  ArrowUp: 38,
  ArrowRight: 39,
  ArrowDown: 40,
  Enter: 13,
  Escape: 27,
  Tab: 9,
  " ": 32,
};

function keyCodeFor(key: string): number {
  if (KEY_CODES[key] !== undefined) return KEY_CODES[key]!;
  if (key.length === 1) return key.toUpperCase().charCodeAt(0);
  return 0;
}

/** The keydown this platform would deliver for a canonical chord. */
function eventFor(chord: string): KeyboardEvent {
  const parts = normalise(chord).split("+");
  const key = parts.pop()!;
  const mods = new Set(parts);
  const code = keyCodeFor(key);
  return new KeyboardEvent("keydown", {
    key: key.length === 1 ? key.toLowerCase() : key,
    code: key.length === 1 ? `Key${key.toUpperCase()}` : key,
    keyCode: code,
    which: code,
    // `Mod` is ⌘ on macOS and Ctrl everywhere else — the same mapping `accelOf`
    // reads back, so what is dispatched here is what the app would match.
    metaKey: isMac() && mods.has("Mod"),
    ctrlKey: (!isMac() && mods.has("Mod")) || mods.has("Ctrl"),
    altKey: mods.has("Alt"),
    shiftKey: mods.has("Shift"),
    bubbles: true,
    cancelable: true,
  } as unknown as KeyboardEventInit);
}

const term = new Terminal({ fontSize: 12, scrollback: 50 });
term.open(document.getElementById("t")!);

let sent = "";
term.onData((d) => {
  sent += d;
});

const textarea = () => document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement | null;

/** What the terminal puts on the wire for this chord, if anything. */
function bytesFor(chord: string): string {
  const ta = textarea();
  sent = "";
  ta?.focus();
  ta?.dispatchEvent(eventFor(chord));
  return sent;
}

const show = (s: string) =>
  JSON.stringify(s).replace(/\\u001b/g, "\\e").replace(/\\u00/g, "\\x");

setTimeout(() => {
  // ── the probe can see a collision at all ───────────────────────────────────
  //
  // Asserted first and deliberately: every result below is a claim that some
  // chord produced *nothing*, and "nothing" is also what a broken probe
  // reports. A control key that must encode proves the measurement is live.
  check(`the probe registers a known control key (Ctrl+C → ${show(bytesFor("Ctrl+C"))})`, bytesFor("Ctrl+C") === "\x03");


  // ── candidate grid chords, measured on both modifier maps ─────────────────
  //
  // The module note says every *arrow* chord is taken, which is what pushed the
  // non-mac grid onto H/J/K/L. That was measured with `Mod` as Ctrl. On macOS
  // `Mod` is ⌘, and xterm encodes nothing for ⌘ — so the same chord can be
  // taken on one platform and free on the other. Printed rather than asserted,
  // because this is the evidence a default is chosen *from*.
  {
    const arrows = ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"];
    const shapes = ["Mod+Shift+", "Mod+Alt+", "Ctrl+Shift+", "Shift+"];
    console.log(`%c chord survey %c on ${isMac() ? "macOS (Mod = Cmd)" : "this platform (Mod = Ctrl)"}`,
      "background:#458;color:#fff", "");
    for (const shape of shapes) {
      const line = arrows
        .map((a) => `${a.replace("Arrow", "")}=${bytesFor(shape + a) ? show(bytesFor(shape + a)) : "free"}`)
        .join("  ");
      console.log(`  ${shape.padEnd(12)} ${line}`);
    }

    // **Does ⌘ suppress this key, or does the key type anyway?**
    //
    // The two are not the same, and the difference decides how the collision
    // model has to be written. ⌘ suppresses an *arrow* (measured free above)
    // but not Enter — Enter is a character, and holding ⌘ still types it.
    const typed = ["Enter", "Tab", "Escape", "Backspace", " "];
    const named = (k: string) => (k === " " ? "Space" : k);
    console.log(
      "  Mod+        " +
        typed
          .map((k) => `${named(k)}=${bytesFor("Mod+" + k) ? show(bytesFor("Mod+" + k)) : "free"}`)
          .join("  "),
    );
    console.log(
      "  Mod+Shift+  " +
        typed
          .map((k) => `${named(k)}=${bytesFor("Mod+Shift+" + k) ? show(bytesFor("Mod+Shift+" + k)) : "free"}`)
          .join("  "),
    );
  }

  // ── every default, measured ────────────────────────────────────────────────
  const collisions: string[] = [];
  for (const [action, binding] of Object.entries(DEFAULTS)) {
    const bytes = bytesFor(binding);
    const chord = normalise(binding);
    if (bytes) {
      collisions.push(`${action} (${chord}) → ${show(bytes)}`);
      console.warn(`COLLISION  ${action}  ${chord}  sends ${show(bytes)} to the shell`);
    } else {
      console.log(`%c free %c ${action}  ${chord}`, "background:#333;color:#8f8", "");
    }
  }

  check(
    collisions.length
      ? `a default must not also reach the shell — ${collisions.length} do: ${collisions.join("; ")}`
      : "no default reaches the shell",
    collisions.length === 0,
  );

  // ── the shape of a safe chord, on this platform ────────────────────────────
  //
  // Recorded so the *reason* the defaults are what they are stays measured
  // rather than remembered. Ctrl+Shift+<letter> is the Windows/Linux terminal
  // convention precisely because it encodes to nothing.
  if (!isMac()) {
    check(`Ctrl+Shift+P is free (${show(bytesFor("Mod+Shift+P"))})`, bytesFor("Mod+Shift+P") === "");
    check(`Ctrl+Shift+T is free (${show(bytesFor("Mod+Shift+T"))})`, bytesFor("Mod+Shift+T") === "");
    check(`plain Ctrl+P is NOT free (${show(bytesFor("Ctrl+P"))})`, bytesFor("Ctrl+P") !== "");
  }

  // ── the survey the defaults were chosen from ───────────────────────────────
  //
  // Printed rather than asserted: this is the evidence behind the choice, and
  // re-reading it is how the next person picks a *new* binding without
  // rediscovering all of this by hand. Arrow keys are the awkward case — xterm
  // encodes every modifier combination of them, so there is no free arrow chord
  // and the grid has to move on letters.
  const survey = [
    "Mod+1", "Mod+2", "Mod+3", "Mod+4",
    "Mod+Shift+1", "Mod+Shift+2", "Mod+Shift+3", "Mod+Shift+4",
    "Mod+T", "Mod+P", "Mod+Shift+T", "Mod+Shift+P",
    "Mod+Shift+H", "Mod+Shift+J", "Mod+Shift+K", "Mod+Shift+L",
    "Mod+Alt+ArrowLeft", "Mod+Shift+ArrowLeft", "Alt+Shift+ArrowLeft", "Alt+ArrowLeft",
  ];
  console.log("--- chord survey (empty = free for a shortcut) ---");
  for (const chord of survey) {
    const bytes = bytesFor(chord);
    console.log(`  ${bytes ? "TAKEN" : "free "}  ${normalise(chord).padEnd(22)} ${show(bytes)}`);
  }

  // ── the predicate agrees with the terminal ─────────────────────────────────
  //
  // `reachesTerminal` is what warns the operator mid-rebind, and it is a
  // *model* of xterm's encoder rather than the encoder itself — so it can drift
  // from the real thing on any xterm upgrade, silently, in the direction that
  // matters (a false all-clear). Checked here against the only authority there
  // is. A disagreement names the chord, so the fix is one line rather than a
  // hunt.
  //
  // Being conservative is allowed: predicting "taken" for something free costs
  // a chord. Predicting "free" for something taken is the bug.
  const letters = ["A", "C", "K", "P", "T", "Z"];
  const digits = ["1", "2", "3", "4", "8", "9", "0"];
  const named = ["ArrowLeft", "ArrowDown", "Home", "End", "Enter", "Tab"];
  const shapes = ["Mod+", "Mod+Shift+", "Alt+", "Mod+Alt+", "Ctrl+", "Ctrl+Shift+", "Shift+"];
  // Only chords that can actually be *bound*. A modifier-less one is refused at
  // the point of binding, so measuring `Shift+A` proves nothing about the
  // predicate and its "sends a" merely drowns the real disagreements — which is
  // what the first run of this did.
  const grid: string[] = [];
  for (const shape of shapes) {
    for (const key of [...letters, ...digits, ...named]) {
      const chord = `${shape}${key}`;
      if (hasModifier(chord)) grid.push(chord);
    }
  }

  const optimistic: string[] = [];
  const pessimistic: string[] = [];
  for (const chord of grid) {
    // `Shift+X` alone has no modifier and is never bindable, but measuring it
    // keeps the grid honest about what "free" means.
    const taken = bytesFor(chord) !== "";
    const predicted = reachesTerminal(chord);
    if (taken && !predicted) optimistic.push(`${normalise(chord)} sends ${show(bytesFor(chord))}`);
    else if (!taken && predicted) pessimistic.push(normalise(chord));
  }

  check(
    optimistic.length
      ? `the predicate never says free when the terminal takes it — wrong about: ${optimistic.join(", ")}`
      : `the predicate never says free when the terminal takes it (${grid.length} chords)`,
    optimistic.length === 0,
  );
  if (pessimistic.length) {
    console.log(
      `  (conservative on ${pessimistic.length}: ${pessimistic.join(", ")} — allowed, costs only a chord)`,
    );
  }

  console.log(
    failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
    failures ? "background:#e63b2e;color:#fff" : "background:#2b7;color:#000",
  );
}, 400);
