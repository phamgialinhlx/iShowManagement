/**
 * How much of the context window a conversation is using.
 *
 * Claude's transcript reports how many tokens the newest prompt contained, but
 * never the size of the window it went into. So the limit has to be established
 * rather than assumed, and a wrong denominator is worse than none — "18% used"
 * against the wrong window tells someone they have room they do not have.
 *
 * Two things are trustworthy:
 *
 *  - A model name that spells the window out, such as `claude-opus-5[1m]`.
 *  - An observation that **exceeds** a window. A conversation holding 626k
 *    tokens (measured on a real session) demonstrably is not in a 200k window,
 *    so the next size up is a fact, not a guess.
 *
 * Anything else returns `null`, and the caller shows the token count alone.
 */

/** Context windows Claude models are known to come in. */
const WINDOWS = [200_000, 1_000_000] as const;

export function contextLimit(model: string | undefined, observed: number): number | null {
  // Spelled out in the name — the only direct evidence there is.
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

/** `626377` → `626k`. Tokens are only ever read approximately. */
export function compactTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 100_000 ? 0 : 1)}k`;
  return String(n);
}
