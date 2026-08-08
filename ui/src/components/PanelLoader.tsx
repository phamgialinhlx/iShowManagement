import { useEffect, useState } from "react";

/**
 * What a pane shows while it is coming up.
 *
 * A terminal that has not connected yet is a black rectangle, and a black
 * rectangle is indistinguishable from a crash. That ambiguity is the whole
 * problem this solves: the operator cannot tell "working on it" from "broken",
 * so they wait, then restart something that was fine.
 *
 * Three rules from the design system shape it:
 *
 *  - **No blinking.** Liveness is shown by a sweep that moves, never by opacity
 *    flicker, and never by a spinner.
 *  - **Not red.** Waiting is not a fault, and red means "you must act". This is
 *    chalk on the panel material.
 *  - **Say what is happening, not "loading".** The phase text is the point; a
 *    generic spinner tells you nothing you did not already know.
 *
 * Elapsed seconds appear once a wait stops feeling instant, because a slow step
 * with a visible clock reads as *slow*, while the same step with no clock reads
 * as *hung*.
 */

/** After this, the wait is worth quantifying. */
const SHOW_CLOCK_AFTER = 3;

/**
 * How much room the caller has.
 *
 * One component with three shapes rather than three components, because the
 * reason seven surfaces each invented their own bare string is that this one
 * was easy to miss and awkward to reuse for a list. A variant is easier to
 * reach for than a new file.
 */
export type LoaderVariant = "panel" | "rows" | "inline";

export function PanelLoader({
  phase,
  detail,
  variant = "panel",
  /** How many skeleton rows to draw, for `rows`. */
  rows = 5,
  /** Shown once the wait runs long — usually why it is taking a while. */
  hint,
  hintAfter = 6,
}: {
  phase: string;
  detail?: string;
  variant?: LoaderVariant;
  rows?: number;
  hint?: string;
  hintAfter?: number;
}) {
  const [seconds, setSeconds] = useState(0);

  useEffect(() => {
    const timer = setInterval(() => setSeconds((s) => s + 1), 1000);
    return () => clearInterval(timer);
  }, []);

  // A widget with room for one line. Still says *what*, because "loading" alone
  // is the thing this component exists not to say.
  if (variant === "inline") {
    return (
      <span className="micro" style={{ color: "var(--text-faint)" }}>
        {phase}
        {seconds >= SHOW_CLOCK_AFTER ? ` · ${seconds}s` : ""}
      </span>
    );
  }

  // A list is coming, so draw where its rows will be. The pane then does not
  // jump when the answer lands — and a skeleton in the shape of the answer
  // reads as "this is filling in" rather than "something is happening".
  if (variant === "rows") {
    return (
      <div className="flex flex-col gap-2 px-3 py-3">
        <div className="flex items-baseline justify-between gap-2">
          <span className="micro" style={{ color: "var(--text-soft)" }}>
            {phase}
          </span>
          {seconds >= SHOW_CLOCK_AFTER && (
            <span className="data text-[10px] tabular-nums" style={{ color: "var(--text-faint)" }}>
              {seconds}s
            </span>
          )}
        </div>
        {detail && (
          <span className="micro truncate" style={{ color: "var(--text-faint)" }} title={detail}>
            {detail}
          </span>
        )}
        <ul className="flex flex-col gap-[6px]">
          {/* Uneven widths, because a column of identical bars reads as a
              rendering fault rather than as content arriving. */}
          {Array.from({ length: rows }, (_, i) => [68, 84, 55, 76, 62][i % 5]!).map((w, i) => (
            <li key={i} className="git-row flex items-center gap-2">
              <span className="shrink-0" style={{ width: 6, height: 6, background: "var(--text-faint)" }} />
              <span style={{ height: 7, width: `${w}%`, background: "var(--text-faint)" }} />
            </li>
          ))}
        </ul>
      </div>
    );
  }

  return (
    <div className="grid h-full place-items-center px-6">
      <div className="flex w-full max-w-[320px] flex-col items-center gap-3">
        <div className="flex w-full items-baseline justify-between">
          <span className="micro" style={{ color: "var(--text)" }}>
            {phase}
          </span>
          {seconds >= SHOW_CLOCK_AFTER && (
            // Tabular figures so the number does not jitter as it counts.
            <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
              {seconds}s
            </span>
          )}
        </div>

        {/* The sweep. Indeterminate on purpose — there is no progress to report,
            and a fake percentage would be an invented measurement. */}
        <div
          className="w-full overflow-hidden"
          style={{ height: 2, background: "color-mix(in srgb, var(--text) 10%, transparent)" }}
          role="progressbar"
          aria-label={phase}
        >
          <div className="sweep" style={{ height: "100%", width: "38%" }} />
        </div>

        {detail && (
          <span className="data w-full truncate text-center text-[10.5px]" style={{ color: "var(--text-faint)" }}>
            {detail}
          </span>
        )}

        {hint && seconds >= hintAfter && (
          <p
            className="data text-center text-[10.5px] leading-relaxed"
            style={{ color: "var(--text-soft)" }}
          >
            {hint}
          </p>
        )}
      </div>
    </div>
  );
}
