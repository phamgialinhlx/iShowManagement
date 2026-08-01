import { motion } from "motion/react";

import { compactTokens } from "../lib/context-window";

/**
 * How full the context window is — as an instrument, not a sentence.
 *
 * A percentage buried in a row of grey text answers "how much" only if you go
 * looking. The question people actually have is "am I about to run out", and
 * that is a *shape* question: a bar you read without focusing beats a number
 * you have to find and then divide in your head.
 *
 * Two rules from the design system decide how it is drawn.
 *
 * **Rule 0 — red only where the operator must act.** A filling context is not
 * an alert; it is a normal thing that happens on every long conversation. So it
 * runs monochrome while there is room, turns amber past three quarters (worth
 * noticing, nothing to do), and only goes red once compaction is genuinely
 * imminent — at which point there *is* something to do, and red means it.
 *
 * **Rule 2 — meters breathe by scaling the bar, never the number.** The width
 * animates; the printed figure snaps. Animating a value invents readings
 * between the two the transcript actually recorded.
 *
 * The bar is drawn only when the window size is known. Everywhere else this
 * shows the token count alone, because a bar against a guessed denominator is a
 * confident lie about how much room is left — and that lie reads as a reason to
 * compact when there may be 900k tokens spare.
 */

/** Past this, worth noticing. */
const NOTICE = 75;
/** Past this, compaction is imminent — the operator has something to do. */
const ACT = 92;

export const contextTone = (percent: number): string =>
  percent >= ACT ? "rgb(var(--primary))" : percent >= NOTICE ? "rgb(var(--busy))" : "var(--text-soft)";

export function ContextMeter({
  tokens,
  limit,
  /** `rail` prints a labelled row above the bar; `strip` is the inline header form. */
  variant = "rail",
}: {
  tokens: number;
  limit: number | null;
  variant?: "rail" | "strip";
}) {
  const percent = limit ? (tokens / limit) * 100 : null;
  const clamped = percent === null ? 0 : Math.max(0, Math.min(100, percent));
  const tone = percent === null ? "var(--text-soft)" : contextTone(percent);

  const title =
    limit === null
      ? `${tokens.toLocaleString()} context tokens — window size unknown, so no share is shown`
      : `${tokens.toLocaleString()} of ~${limit.toLocaleString()} context tokens`;

  if (variant === "strip") {
    return (
      <span className="flex shrink-0 items-center gap-1.5" title={title}>
        <span
          className="relative block overflow-hidden"
          style={{ width: 54, height: 4, background: "rgba(232,230,225,0.10)" }}
        >
          {percent !== null && (
            <motion.span
              className="absolute inset-y-0 left-0 block w-full"
              style={{ background: tone, transformOrigin: "left" }}
              initial={false}
              animate={{ scaleX: clamped / 100 }}
              transition={{ duration: 0.3, ease: [0.2, 0.9, 0.3, 1] }}
            />
          )}
        </span>
        <span className="micro" style={{ color: tone }}>
          {percent === null ? `${compactTokens(tokens)} CTX` : `${Math.round(percent)}%`}
        </span>
      </span>
    );
  }

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2">
        <span className="micro">CONTEXT</span>
        <span className="data text-[11px]" style={{ color: tone }}>
          {/* Both figures, because they answer different questions: the share
              says whether to compact, the count says how much compacting would
              actually recover. */}
          {percent === null
            ? compactTokens(tokens)
            : `${Math.round(percent)}% · ${compactTokens(tokens)}`}
        </span>
      </div>
      <div style={{ height: 4, background: "rgba(232,230,225,0.10)" }} title={title}>
        {percent !== null && (
          <motion.div
            style={{ height: "100%", background: tone, transformOrigin: "left" }}
            initial={false}
            animate={{ scaleX: clamped / 100 }}
            transition={{ duration: 0.3, ease: [0.2, 0.9, 0.3, 1] }}
          />
        )}
      </div>
      {limit === null && (
        <span className="micro" style={{ color: "var(--text-faint)" }}>
          WINDOW UNKNOWN — SET IT IN SESSION SETTINGS
        </span>
      )}
      {percent !== null && percent >= ACT && (
        <span className="micro" style={{ color: "rgb(var(--primary))" }}>
          RUN /COMPACT
        </span>
      )}
    </div>
  );
}
