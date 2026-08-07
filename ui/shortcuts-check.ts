/**
 * Keyboard shortcuts: chords, conflicts, focus movement, and what is ignored.
 *
 * Open http://localhost:5273/shortcuts-check.html and read the console.
 *
 * The expensive failure here is silent: a binding that swallows a key means the
 * terminal never sees it, and nothing anywhere says so — you press a key and
 * the far side does nothing. So the rules that protect the terminal are pinned
 * hardest: modifiers are required, xterm's textarea is *not* treated as a text
 * field, and two spellings of one chord must compare equal or the conflict
 * detector reports all-clear on a genuine collision.
 */
import {
  ACTIONS,
  DEFAULTS,
  accelOf,
  conflicts,
  hasModifier,
  isTypingTarget,
  match,
  moveFocus,
  normalise,
  reachesTerminal,
  readBindings,
  writeBindings,
  type Bindings,
} from "./src/lib/shortcuts";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) {
    console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  } else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

const saved = localStorage.getItem("rmux.shortcuts");

// ── chords normalise to one spelling ─────────────────────────────────────────

// Without this, "Alt+Mod+K" and "Mod+Alt+K" are two bindings that fire on the
// same keystroke, and `conflicts` cheerfully reports none.
check("modifier order does not matter", normalise("Alt+Mod+K") === normalise("Mod+Alt+K"));
check("case does not matter for a letter", normalise("Mod+k") === normalise("MOD+K"));
check("a named key keeps its spelling", normalise("Mod+Alt+ArrowLeft") === "Mod+Alt+ArrowLeft");
check("cmd and meta are both Mod", normalise("Cmd+1") === "Mod+1" && normalise("Meta+1") === "Mod+1");
check("an empty binding normalises to empty", normalise("") === "");

// ── a modifier is required ───────────────────────────────────────────────────

check("a bare letter is not bindable", !hasModifier("K"));
// Shift+a is just A. Accepting it would take capitals away from every terminal.
check("shift alone is not a modifier", !hasModifier("Shift+A"));
check("mod counts", hasModifier("Mod+K"));
check("alt counts", hasModifier("Alt+K"));
check("ctrl counts", hasModifier("Ctrl+K"));

// ── events map to chords ─────────────────────────────────────────────────────

const press = (init: KeyboardEventInit) => new KeyboardEvent("keydown", init);
const mac = /mac/i.test(navigator.platform || navigator.userAgent);

if (mac) {
  check("cmd+1 is Mod+1", accelOf(press({ key: "1", metaKey: true })) === "Mod+1");
  check("cmd+alt+left is the grid chord", accelOf(press({ key: "ArrowLeft", metaKey: true, altKey: true })) === "Mod+Alt+ArrowLeft");
  // On macOS Ctrl is its own modifier and must stay distinct from Mod, or
  // Ctrl+C in a shell would collide with a ⌘C binding.
  check("ctrl is distinct from mod on macOS", accelOf(press({ key: "c", ctrlKey: true })) === "Ctrl+C");
} else {
  check("ctrl+1 is Mod+1", accelOf(press({ key: "1", ctrlKey: true })) === "Mod+1");
  // Elsewhere Ctrl *is* Mod; counting it twice would mean no Mod binding ever
  // matched, since every event would carry a phantom Ctrl as well.
  check("ctrl is not counted twice off macOS", accelOf(press({ key: "1", ctrlKey: true })) === "Mod+1");
}

// ── the defaults are sane ────────────────────────────────────────────────────

check("every action has a default", ACTIONS.every((a) => !!DEFAULTS[a.id]));
check("every default carries a modifier", ACTIONS.every((a) => hasModifier(DEFAULTS[a.id])));
check("the defaults do not collide", conflicts({ ...DEFAULTS }).length === 0);
// macOS binds these in the app menu; a shortcut here either loses silently or,
// worse, wins and closes the window.
const reserved = ["Mod+W", "Mod+Q", "Mod+N", "Mod+M"];
check(
  "no default takes a macOS menu key",
  ACTIONS.every((a) => !reserved.includes(normalise(DEFAULTS[a.id]))),
);

// The same guarantee for the other platforms, which had none. On macOS `Mod` is
// ⌘ and free of the terminal; elsewhere it is Ctrl, and eight of the ten macOS
// defaults were measured putting bytes on the wire — `Mod+4` sent `\x1c`, the
// quit character. A colliding binding fires *both*, since `useShortcuts`
// listens on the bubble and xterm has already written to the pty.
//
// `shortcut-terminal-check.ts` pins `reachesTerminal` against a real xterm; this
// asserts the defaults respect it, which needs no browser and so runs here.
const noisy = ACTIONS.filter((a) => reachesTerminal(DEFAULTS[a.id]));
check(
  noisy.length
    ? `no default is also sent to the terminal — these are: ${noisy.map((a) => `${a.id} (${normalise(DEFAULTS[a.id])})`).join(", ")}`
    : "no default is also sent to the terminal",
  noisy.length === 0,
);

// Ctrl+C must keep working as an interrupt, so nothing may bind it. On macOS
// this is `Ctrl+C` and off it `Mod+C` — the same physical keys either way.
check(
  "nothing binds the interrupt",
  ACTIONS.every((a) => {
    const chord = normalise(DEFAULTS[a.id]);
    return chord !== "Ctrl+C" && chord !== "Mod+C";
  }),
);

// ── conflicts are reported, not prevented ────────────────────────────────────

const clashing: Bindings = { ...DEFAULTS, "view.files": DEFAULTS["view.claude"] };
const found = conflicts(clashing);
check("two actions on one chord are reported", found.length === 1 && found[0]!.length === 2);
check(
  "the report names both actions",
  !!found[0]?.includes("view.claude") && !!found[0]?.includes("view.files"),
);
// Same chord, different spelling — the case normalisation exists for this.
check(
  "a differently-spelled duplicate is still a conflict",
  conflicts({ ...DEFAULTS, "view.files": "Alt+Mod+ArrowLeft", "pane.left": "Mod+Alt+ArrowLeft" }).length === 1,
);

// ── typing must win, except in a terminal ────────────────────────────────────

const input = document.createElement("input");
document.body.append(input);
check("a text field is a typing target", isTypingTarget(input));

const area = document.createElement("textarea");
document.body.append(area);
check("a textarea is a typing target", isTypingTarget(area));

// The one that matters: xterm parks a focused textarea for the whole time a
// terminal is on screen. Treating it as "typing" would disable every shortcut
// in the app's main view.
const term = document.createElement("div");
term.className = "xterm";
const xtermArea = document.createElement("textarea");
term.append(xtermArea);
document.body.append(term);
check("xterm's own textarea is NOT a typing target", !isTypingTarget(xtermArea));

const div = document.createElement("div");
document.body.append(div);
check("an ordinary element is not a typing target", !isTypingTarget(div));

// ── matching ─────────────────────────────────────────────────────────────────

const chordFor = (id: string) => normalise(DEFAULTS[id as keyof typeof DEFAULTS]);
const evFor = (chord: string): KeyboardEvent => {
  const parts = chord.split("+");
  const key = parts.pop()!;
  return press({
    key: key.length === 1 ? key.toLowerCase() : key,
    metaKey: mac && parts.includes("Mod"),
    ctrlKey: (!mac && parts.includes("Mod")) || parts.includes("Ctrl"),
    altKey: parts.includes("Alt"),
    shiftKey: parts.includes("Shift"),
  });
};

const claudeEvent = evFor(chordFor("view.claude"));
Object.defineProperty(claudeEvent, "target", { value: div });
check("a bound chord matches its action", match(claudeEvent, { ...DEFAULTS }) === "view.claude");

const typedEvent = evFor(chordFor("view.claude"));
Object.defineProperty(typedEvent, "target", { value: input });
check("the same chord is ignored while typing in a field", match(typedEvent, { ...DEFAULTS }) === null);

const inTerminal = evFor(chordFor("view.claude"));
Object.defineProperty(inTerminal, "target", { value: xtermArea });
check("but it still fires inside a terminal", match(inTerminal, { ...DEFAULTS }) === "view.claude");

const unbound = press({ key: "z", metaKey: mac, ctrlKey: !mac });
Object.defineProperty(unbound, "target", { value: div });
check("an unbound chord matches nothing", match(unbound, { ...DEFAULTS }) === null);

// ── moving around the grid ───────────────────────────────────────────────────

// A 2x2: 0 1 / 2 3.
check("right moves one along", moveFocus(0, "right", 2, 2) === 1);
check("down moves one row", moveFocus(0, "down", 2, 2) === 2);
check("left from the middle comes back", moveFocus(1, "left", 2, 2) === 0);
check("up from the bottom row", moveFocus(3, "up", 2, 2) === 1);

// Clamped, not wrapped: in a 4x4, wrapping "left" at the left edge lands six
// tiles away, and nothing on screen suggests those are neighbours.
check("left at the left edge stays put", moveFocus(0, "left", 2, 2) === 0);
check("right at the right edge stays put", moveFocus(1, "right", 2, 2) === 1);
check("up on the top row stays put", moveFocus(1, "up", 2, 2) === 1);
check("down on the bottom row stays put", moveFocus(2, "down", 2, 2) === 2);
check("a wide 1x4 moves along one row", moveFocus(1, "right", 4, 1) === 2);
check("a 1x4 has nowhere to go down", moveFocus(1, "down", 4, 1) === 1);
check("an out-of-range index is clamped, not NaN", moveFocus(99, "left", 2, 2) === 2);
check("a single tile has no neighbours", moveFocus(0, "right", 1, 1) === 0);

// ── storage ──────────────────────────────────────────────────────────────────

localStorage.removeItem("rmux.shortcuts");
check("an unset store yields the defaults", readBindings()["view.claude"] === DEFAULTS["view.claude"]);

writeBindings({ ...DEFAULTS, "view.files": "Mod+Shift+F" });
check("a rebind is remembered", readBindings()["view.files"] === "Mod+Shift+F");

// An action added in a later version is absent from an older stored map. It
// must fall back to its default rather than becoming silently unbound.
localStorage.setItem("rmux.shortcuts", JSON.stringify({ "view.claude": "Mod+9" }));
const partial = readBindings();
check("a stored value wins", partial["view.claude"] === "Mod+9");
check("an action missing from the store keeps its default", partial["pane.left"] === DEFAULTS["pane.left"]);

localStorage.setItem("rmux.shortcuts", "{ not json");
check("a corrupt store falls back to the defaults", readBindings()["view.claude"] === DEFAULTS["view.claude"]);

// ── restore ──────────────────────────────────────────────────────────────────

input.remove();
area.remove();
term.remove();
div.remove();
if (saved === null) localStorage.removeItem("rmux.shortcuts");
else localStorage.setItem("rmux.shortcuts", saved);

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
  failures ? "background:#e63b2e;color:#fff" : "background:#2b7;color:#000",
);
