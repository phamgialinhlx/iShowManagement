/**
 * Vietnamese, Telex, and every other IME that edits what it already typed.
 *
 * ## What macOS actually does — measured, in the app
 *
 * `ime-check.ts` simulated `compositionstart` → `compositionupdate` →
 * `compositionend` and passed 4/4 against a real xterm, while typing Vietnamese
 * in rmux stayed broken. The simulation was the problem. Recorded from the
 * running app with the Vietnamese keyboard active, by a temporary recorder
 * posting every event to the dev server (removed once it had answered), typing
 * `ee` → `ê`:
 *
 *     beforeinput data="e"  inputType=insertText             textarea=" e"
 *     input       data="e"  inputType=insertText             textarea=" e"
 *     keydown     key="e"   keyCode=229  isComposing=false
 *     beforeinput data="ê"  inputType=insertReplacementText  textarea=" e"
 *     input       data="ê"  inputType=insertReplacementText  textarea=" ê"
 *
 * **There is not a single composition event, and `isComposing` is false
 * throughout.** macOS Telex does not use marked text here: it inserts the plain
 * letter, then *replaces* it with the accented one via `insertReplacementText`.
 *
 * xterm understands composition events and plain `insertText`. It has nothing
 * for `insertReplacementText`, so the base letter is sent and the replacement is
 * dropped — which is precisely the report: accents never arrive, and the letters
 * that exist *only* as replacements (ê, ơ, ư, đ, â, ô) cannot be typed at all.
 * Typing `ee ee ee` produced `e e e` on the wire.
 *
 * ## Why a terminal has to do the work itself
 *
 * A textarea can replace a range in place. A terminal cannot — the letter has
 * already been sent, Claude has already drawn it, and there is no "unsend". The
 * only way to express a replacement over a byte stream is to erase what was sent
 * and send the new text: one DEL per replaced character, then the replacement.
 * That is what a person pressing backspace would do, and it is what every
 * readline-style prompt on the far side expects.
 *
 * `getTargetRanges()` on the `beforeinput` event says exactly how much is being
 * replaced, so the count is read rather than assumed. It is only available on
 * `beforeinput` — by `input` the range is gone, which is why the interception
 * happens there.
 *
 * ## Why the default is prevented
 *
 * Left alone, xterm would also see the `input` event and send its own idea of
 * what changed, so the replacement would arrive twice. Preventing the default
 * makes this the only path — the same reasoning as `terminal-clipboard.ts`,
 * where a second copy/paste implementation made every paste arrive twice.
 */

/** ASCII DEL. What a terminal expects for "rub out the last character". */
const DEL = "\x7f";

/**
 * How many characters is this replacement standing in for?
 *
 * Usually one — `e` becoming `ê`. But Telex composes across a whole syllable, so
 * `dd` → `đ` and longer rewrites happen too, and a hard-coded 1 would leave
 * stray letters behind. The count comes from the event.
 */
export function replacedLength(ranges: readonly StaticRange[]): number {
  const range = ranges[0];
  if (!range) return 0;
  // Same text node in a textarea's shadow tree, so offsets are directly
  // comparable. Guarded anyway: a negative count would send DELs forever.
  return Math.max(0, range.endOffset - range.startOffset);
}

/**
 * The bytes that turn what was already sent into `data`.
 *
 * Exported so `ime-replace-check.ts` can assert it without a browser: this is
 * the whole behaviour, and it is pure.
 */
export function replacementKeys(replaced: number, data: string): string {
  return DEL.repeat(replaced) + data;
}

/**
 * Route `insertReplacementText` into a terminal.
 *
 * @param textarea xterm's own textarea — the element the IME writes into.
 * @param send     what to put on the wire. Normalised to NFC by the caller.
 * @returns a detach function.
 */
export type Ime = {
  /**
   * Should this `onData` chunk go on the wire?
   *
   * **False while a composition is running.** xterm flushes the marked text as
   * it changes — measured on the wire, `Tiếng` left as `"Ti"` and then `"ê"`,
   * two separate sends, mid-word. Once those bytes are gone the tone key that
   * arrives next has nothing left to modify, so `s` landed literally and the
   * word came out `Tiêngs`. `Việt`, whose composition happened to commit in one
   * piece, was correct in the same sentence — which is why this looked
   * intermittent rather than structural.
   *
   * A terminal cannot take bytes back, so the only correct thing is to not send
   * them early. Everything xterm emits during a composition is a partial view
   * of a word still being typed; the committed form arrives at
   * `compositionend`, and that is what gets sent.
   */
  shouldSend: (data: string) => boolean;
  dispose: () => void;
};

export function attachImeReplace(textarea: HTMLTextAreaElement, send: (data: string) => void): Ime {
  const onBeforeInput = (event: Event) => {
    const e = event as InputEvent;
    if (e.inputType !== "insertReplacementText") return;
    // **Never touch a live composition.** macOS Vietnamese has two modes and
    // fires completely different events in each — one run recorded no
    // composition events at all, the next used them throughout. xterm already
    // handles the composition path, and it tracks the marked text by *offset*
    // into the textarea (`_compositionPosition`). Interfering there desynchs
    // those offsets and the finalise slices one character short: `Xin chào tôi
    // đang gõ` arrived as `in hào ôi ang õ`, every word missing its first
    // letter. So this handles only the non-composition path and leaves the
    // other one strictly alone.
    if (e.isComposing) return;
    // `data` is null for a deletion-shaped replacement; there is nothing to send
    // but the erase still has to happen, or the letter stays on screen.
    const data = e.data ?? "";

    const ranges = typeof e.getTargetRanges === "function" ? e.getTargetRanges() : [];
    const keys = replacementKeys(replacedLength(ranges), data);
    if (!keys) return;

    // Ours alone — otherwise xterm sends its own version from the `input` event
    // that follows and the replacement lands twice.
    e.preventDefault();
    send(keys);

    // **The textarea is deliberately not cleared.** Clearing it was tried and
    // it is what ate the first letter of every word: xterm measures the marked
    // text as offsets into this value, so resetting it moves the ground under
    // its own bookkeeping. Leaving a stale buffer is the lesser problem — xterm
    // owns that element and clears it on its own path.
  };

  /**
   * Start every composition from an empty buffer.
   *
   * **This is the first-letter bug.** xterm keeps a non-breaking space parked in
   * its textarea, and it never clears the buffer between words — measured, the
   * value grew to `"jt mẹ màycon chó này ncyuwaxxin chào tôi đang gõ\u00a0"`
   * across one sentence. At `compositionstart` xterm records
   * `start = value.length`, which *counts that trailing NBSP*, but the IME
   * inserts its marked text **before** it. So the finalising slice begins one
   * character late and the first letter of every word is thrown away:
   * `tiếng việt` arrived as `iếng iệt`, `Xin chào tôi đang gõ` as
   * `in hào ôi ang õ`.
   *
   * Clearing here, in the capture phase, means xterm records `start = 0` against
   * a buffer whose contents are exactly the word being typed. Nothing is lost by
   * emptying it: anything already in there has long since been sent, and the
   * terminal — not this textarea — is the record of what was typed.
   *
   * It must be `compositionstart` specifically. Clearing during the composition
   * was tried and it moves the ground under the same offsets this is fixing.
   */
  let composing = false;

  const onCompositionStart = () => {
    composing = true;
    textarea.value = "";
  };

  /**
   * Reopen the gate and send nothing.
   *
   * **Do not send the committed word from here.** It was tried and it doubled
   * everything — `Tiếng Việt` arrived as `TiêngsêngsViệtViệt` — because xterm
   * emits the committed form through `onData` itself once the composition ends.
   * Its *partial* states are the problem, not its final one, so the gate closes
   * for the duration of the composition and reopens here, letting xterm's own
   * single committed send straight through.
   *
   * The flag clears in the capture phase, before xterm's handler runs, which is
   * exactly what lets that send pass.
   */
  const onCompositionEnd = () => {
    composing = false;
  };

  textarea.addEventListener("compositionstart", onCompositionStart, true);
  textarea.addEventListener("compositionend", onCompositionEnd, true);
  textarea.addEventListener("beforeinput", onBeforeInput, true);

  return {
    shouldSend: () => !composing,
    dispose: () => {
      textarea.removeEventListener("compositionstart", onCompositionStart, true);
      textarea.removeEventListener("compositionend", onCompositionEnd, true);
      textarea.removeEventListener("beforeinput", onBeforeInput, true);
    },
  };
}
