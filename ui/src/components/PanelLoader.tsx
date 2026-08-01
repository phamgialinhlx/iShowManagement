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

export function PanelLoader({
  phase,
  detail,
  /** Shown once the wait runs long — usually why it is taking a while. */
  hint,
  hintAfter = 6,
}: {
  phase: string;
  detail?: string;
  hint?: string;
  hintAfter?: number;
}) {
  const [seconds, setSeconds] = useState(0);

  useEffect(() => {
    const timer = setInterval(() => setSeconds((s) => s + 1), 1000);
    return () => clearInterval(timer);
  }, []);

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
          style={{ height: 2, background: "rgba(232,230,225,0.10)" }}
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
