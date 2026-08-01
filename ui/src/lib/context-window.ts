/**
 * How much of the context window a conversation is using.
 *
 * Claude's transcript reports how many tokens the newest prompt contained, but
 * never the size of the window it went into. So the limit has to be established
 * rather than assumed, and a wrong denominator is worse than none — "18% used"
 * against the wrong window tells someone they have room they do not have.
 *
 * Only three things are trustworthy:
 *
 *  - The operator telling us. A window chosen in session settings is a fact.
 *  - A model name that spells it out, such as `claude-opus-5[1m]`.
 *  - An observation that **exceeds** a window. A conversation holding 626k
 *    tokens (measured on a real session) demonstrably is not in a 200k window,
 *    so the next size up is a fact, not a guess.
 *
 * **A model name alone is not enough**, and assuming otherwise was a real bug:
 * the same model ships in 200k and 1M variants, the transcript records only
 * `claude-opus-5`, and guessing 200k reported 80k of a 1M window as "40% used".
 * That is worse than showing nothing, because it reads as a reason to compact
 * when there are still 900k tokens spare.
 *
 * So anything else returns `null` and the caller shows the token count alone.
 */

/** Context windows Claude models are known to come in. */
const WINDOWS = [200_000, 1_000_000] as const;

export function contextLimit(
  model: string | undefined,
  observed: number,
  /** Chosen in session settings. Beats every inference — it is not a guess. */
  configured?: number,
): number | null {
  // …unless the conversation has already outgrown what was configured, which
  // proves the setting wrong rather than the measurement.
  if (configured && observed <= configured) return configured;

  // Spelled out in the name — the strongest evidence there is.
  if (model && /\[1m\]/i.test(model)) return 1_000_000;


  // Otherwise infer only upward: the smallest known window that could actually
  // hold what we just saw. Below the smallest window this proves nothing, so it
  // deliberately reports nothing.
  if (observed > WINDOWS[0]) {
    // Past the largest known window we can only report the largest we know of.
    return WINDOWS.find((w) => observed <= w) ?? 1_000_000;
  }

  return null;
}

/**
 * Read the window size out of Claude's own output.
 *
 * This is the best source there is, and it was sitting in plain sight: Claude
 * prints its model with the window beside it — `Opus 5 (1M context)` — in the
 * banner and again in `/status`. The transcript never records it, so scanning
 * what the terminal was sent is the only way to *observe* rather than infer.
 *
 * Scoped tightly on purpose. It matches only the parenthesised form directly
 * followed by the word `context`, so a conversation that merely mentions "1M"
 * cannot move the denominator — the meter must never be steered by whatever
 * someone happens to be typing about.
 */
const WINDOW_MARKER = /\((\d+(?:\.\d+)?)\s*([km])\s+context\)/i;

export function sniffWindow(text: string): number | null {
  const match = WINDOW_MARKER.exec(text);
  if (!match) return null;

  const size = Number(match[1]);
  if (!Number.isFinite(size) || size <= 0) return null;

  const tokens = Math.round(size * (match[2]!.toLowerCase() === "m" ? 1_000_000 : 1_000));
  // A sanity floor and ceiling. Claude redraws constantly and this runs over
  // raw PTY bytes, so a line split across two chunks can present a truncated
  // number; a 5k or 90M "window" is a parse artefact, not a model.
  return tokens >= 50_000 && tokens <= 20_000_000 ? tokens : null;
}

/** `626377` → `626k`. Tokens are only ever read approximately. */
export function compactTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 100_000 ? 0 : 1)}k`;
  return String(n);
}
