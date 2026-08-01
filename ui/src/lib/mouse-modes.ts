/**
 * Turning a program's mouse reporting off locally, so text can be selected.
 *
 * A full-screen program like Claude's TUI enables mouse tracking (DECSET 1002 or
 * 1003 plus an encoding such as 1006). From that moment the terminal stops
 * treating a drag as a selection and starts *sending* it to the program, so the
 * ordinary gesture for copying text silently does nothing. With any-event
 * tracking it is worse than silent: every mouse movement becomes an escape
 * sequence written to the PTY, which on a remote session is a round trip per
 * mouse move — felt as lag while scrolling or dragging.
 *
 * xterm's built-in escape hatch is to hold Option (macOS) or Shift, which is not
 * discoverable and does nothing about the traffic.
 *
 * Mouse tracking is just terminal state, and the terminal here is xterm in this
 * window — not the program on the far side. Writing the reset sequences *into*
 * xterm turns reporting off locally without telling the program anything, so
 * drags select again. Restoring means writing back exactly the modes the program
 * had set, which is why they are tracked from its own output rather than
 * guessed.
 */

/** The DEC private modes that control mouse reporting and its encoding. */
const MOUSE_MODES = [9, 1000, 1001, 1002, 1003, 1004, 1005, 1006, 1015, 1016] as const;

/** Matches `ESC [ ? <params> h|l`, where params may be `1002;1006`. */
const DECSET = /\x1b\[\?([\d;]+)([hl])/g;

/**
 * Tracks which mouse modes a program has switched on.
 *
 * Fed every chunk the program produces. Cheap: a regex over each chunk, and the
 * sequences are rare — but the scan happens on the hot output path, so it does
 * no allocation unless a match is found.
 */
export class MouseModeTracker {
  private readonly active = new Set<number>();

  /** Watch a chunk of program output for mode changes. */
  observe(text: string): void {
    // The overwhelmingly common case: no private-mode sequence at all.
    if (!text.includes("\x1b[?")) return;

    DECSET.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = DECSET.exec(text)) !== null) {
      const set = match[2] === "h";
      for (const raw of match[1]!.split(";")) {
        const mode = Number(raw);
        if (!MOUSE_MODES.includes(mode as (typeof MOUSE_MODES)[number])) continue;
        if (set) this.active.add(mode);
        else this.active.delete(mode);
      }
    }
  }

  /** Whether the program is currently asking for mouse reports. */
  get enabled(): boolean {
    return this.active.size > 0;
  }

  /** Sequences that turn off every mode the program had set. */
  disableSequence(): string {
    if (!this.active.size) return "";
    return [...this.active].map((mode) => `\x1b[?${mode}l`).join("");
  }

  /** Sequences that put back exactly what the program had set. */
  restoreSequence(): string {
    if (!this.active.size) return "";
    return [...this.active].map((mode) => `\x1b[?${mode}h`).join("");
  }
}
