import { humanDuration, peakHour, type DayPoint, type HourPoint } from "../lib/dashboard";

/**
 * The Progress page's charts.
 *
 * Hand-drawn SVG rather than a charting library, for the same reason the rail's
 * meters are: a library brings its own opinions about axes, grids, tooltips and
 * rounded corners, and every one of them would have to be fought back to the
 * design system. These are four shapes over at most a fortnight of data.
 *
 * Rules carried from the rail:
 *
 *  - **Bars move, numbers do not.** Widths ease; printed values snap.
 *  - **Amber is in-progress, never red.** Red means "you must act", and a chart
 *    of work already done is not an alarm.
 *  - **A day with nothing recorded is drawn as an empty column**, never skipped.
 *    Closing the gaps would turn a week off into a continuous run.
 */

/** Bars over the last N days, with the axis labelled at both ends. */
export function DayBars({
  points,
  field,
  label,
  format,
}: {
  points: DayPoint[];
  field: "tasksDone" | "commands" | "prompts" | "seconds";
  label: string;
  format?: (n: number) => string;
}) {
  const max = Math.max(1, ...points.map((p) => p[field]));
  const peak = points.reduce((best, p) => (p[field] > best[field] ? p : best), points[0]!);

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between">
        <span className="micro">{label}</span>
        <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
          best {format ? format(peak?.[field] ?? 0) : (peak?.[field] ?? 0)}
        </span>
      </div>

      <div className="flex h-[68px] items-end gap-[3px]">
        {points.map((p) => {
          const value = p[field];
          const height = max > 0 ? (value / max) * 100 : 0;
          return (
            <div
              key={p.day}
              className="flex h-full flex-1 flex-col justify-end"
              title={`${p.day} · ${format ? format(value) : value}`}
            >
              <div
                style={{
                  // A recorded-but-tiny value still gets a visible sliver, or a
                  // day's work reads as a day off.
                  height: value > 0 ? `${Math.max(3, height)}%` : 1,
                  background: value > 0 ? "rgb(var(--busy))" : "var(--border)",
                  transition: "height var(--dur) var(--ease)",
                }}
              />
            </div>
          );
        })}
      </div>

      <div className="flex justify-between">
        <span className="data text-[9px]" style={{ color: "var(--text-faint)" }}>
          {points[0]?.day.slice(5)}
        </span>
        <span className="data text-[9px]" style={{ color: "var(--text-faint)" }}>
          today
        </span>
      </div>
    </div>
  );
}

/**
 * The working day, hour by hour.
 *
 * The other charts answer *how much*; this one answers **when**, which is the
 * only one of the two that can change what somebody does tomorrow. It is a
 * shape rather than a total, so it is drawn over a range of days — a single
 * day is a handful of columns and has no shape to read.
 *
 * Three decisions worth keeping:
 *
 *  - **All 24 hours, always.** Dropping the empty ones would compress the axis
 *    and put lunch next to midnight. The quiet hours *are* the finding.
 *  - **The current hour is marked, never highlighted in red.** It is a position,
 *    not something to act on. A hairline under the column says where you are.
 *  - **Ticks at 00 / 06 / 12 / 18.** Twenty-four labels at 9px is a smear, and
 *    the quarters are what anyone actually reads a clock by.
 */
export function HourClock({
  points,
  label,
  now,
}: {
  points: HourPoint[];
  label: string;
  /** The hour to mark. Passed in so a harness can pin it. */
  now?: number;
}) {
  const max = Math.max(1, ...points.map((p) => p.seconds));
  const peak = peakHour(points);
  const current = now ?? new Date().getHours();
  const total = points.reduce((n, p) => n + p.seconds, 0);

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between">
        <span className="micro">{label}</span>
        <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
          {peak ? `busiest ${peak.label}:00 · ${humanDuration(peak.seconds)}` : "nothing recorded yet"}
        </span>
      </div>

      <div className="flex h-[68px] items-end gap-[2px]">
        {points.map((p) => (
          <div
            key={p.hour}
            className="flex h-full flex-1 flex-col justify-end"
            title={`${p.label}:00 · ${humanDuration(p.seconds)}${
              total ? ` · ${Math.round((p.seconds / total) * 100)}%` : ""
            }`}
          >
            <div
              style={{
                height: p.seconds > 0 ? `${Math.max(3, (p.seconds / max) * 100)}%` : 1,
                background:
                  p.seconds > 0
                    ? `rgb(var(--busy) / ${p.hour === peak?.hour ? 1 : 0.72})`
                    : "var(--border)",
                transition: "height var(--dur) var(--ease)",
              }}
            />
          </div>
        ))}
      </div>

      {/* The axis is its own row of 24 cells so a tick sits under its column
          rather than being positioned by eye. */}
      <div className="flex gap-[2px]">
        {points.map((p) => (
          <div key={p.hour} className="flex flex-1 flex-col items-center">
            <div
              style={{
                height: 2,
                width: "100%",
                background: p.hour === current ? "var(--text-soft)" : "transparent",
              }}
            />
            <span
              className="data text-[9px] leading-none"
              style={{ color: "var(--text-faint)", visibility: p.hour % 6 === 0 ? "visible" : "hidden" }}
            >
              {p.label}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * Where the day's attention went, as one proportional bar.
 *
 * A pie was the obvious choice and is worse: comparing angles is harder than
 * comparing lengths, and at four or five sessions the labels do not fit around
 * one without leader lines. A single stacked bar reads left to right and its
 * legend is a list, which is also the order the table below uses.
 */
export function SessionSplit({
  rows,
}: {
  rows: readonly { id: string; name: string; seconds: number }[];
}) {
  const total = rows.reduce((n, r) => n + r.seconds, 0);
  if (!total) {
    return (
      <p className="data text-[11px]" style={{ color: "var(--text-faint)" }}>
        No attention time recorded for this day.
      </p>
    );
  }

  // Four shades of the same amber rather than four hues: these are one quantity
  // split up, not four different things, and four colours would imply otherwise.
  const shade = (i: number) => 0.85 - Math.min(0.55, i * 0.14);

  return (
    <div className="flex flex-col gap-2">
      <div className="flex h-[10px] w-full overflow-hidden" style={{ background: "var(--border)" }}>
        {rows.map((r, i) => (
          <div
            key={r.id}
            title={`${r.name} · ${humanDuration(r.seconds)}`}
            style={{
              width: `${(r.seconds / total) * 100}%`,
              background: `rgb(var(--busy) / ${shade(i)})`,
              transition: "width var(--dur) var(--ease)",
            }}
          />
        ))}
      </div>
      <ul className="flex flex-col gap-[2px]">
        {rows.map((r, i) => (
          <li key={r.id} className="flex items-baseline gap-2">
            <span
              className="inline-block shrink-0"
              style={{ width: 8, height: 8, background: `rgb(var(--busy) / ${shade(i)})` }}
              aria-hidden="true"
            />
            <span className="data flex-1 truncate text-[11px]" style={{ color: "var(--text-soft)" }}>
              {r.name}
            </span>
            <span className="data text-[11px] tabular-nums" style={{ color: "var(--text)" }}>
              {humanDuration(r.seconds)}
            </span>
            <span className="data w-[38px] text-right text-[10px]" style={{ color: "var(--text-faint)" }}>
              {Math.round((r.seconds / total) * 100)}%
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * Eight weeks of activity, one square per day.
 *
 * The point is the *shape* of a habit, which a bar chart of the same range
 * cannot show at this width — 56 bars is a smear. Intensity is bucketed rather
 * than continuous because the eye cannot read a ratio out of a shade anyway;
 * four steps is what it can distinguish.
 */
export function StreakGrid({ points }: { points: DayPoint[] }) {
  const max = Math.max(1, ...points.map((p) => p.tasksDone + p.commands / 10));
  const level = (p: DayPoint) => {
    if (!p.any) return 0;
    const score = (p.tasksDone + p.commands / 10) / max;
    if (score > 0.66) return 3;
    if (score > 0.33) return 2;
    return 1;
  };
  const fill = ["var(--border)", "rgb(var(--busy) / 0.3)", "rgb(var(--busy) / 0.6)", "rgb(var(--busy))"];

  // Columns are weeks, so the grid reads the way a calendar does. Filling by
  // column keeps each row a weekday.
  return (
    <div className="flex flex-col gap-1">
      <span className="micro">LAST 8 WEEKS</span>
      <div
        className="grid gap-[3px]"
        style={{ gridTemplateRows: "repeat(7, 10px)", gridAutoFlow: "column", gridAutoColumns: "10px" }}
      >
        {points.map((p) => (
          <div
            key={p.day}
            title={`${p.day} · ${p.tasksDone} tasks · ${p.commands} commands`}
            style={{ background: fill[level(p)], width: 10, height: 10 }}
          />
        ))}
      </div>
    </div>
  );
}

/** A goal ring: one number against a target, with over-target stated plainly. */
export function GoalRing({
  label,
  value,
  target,
  format,
}: {
  label: string;
  value: number;
  target: number;
  format: (n: number) => string;
}) {
  const ratio = target > 0 ? value / target : 0;
  const clamped = Math.min(1, ratio);
  const R = 26;
  const C = 2 * Math.PI * R;
  const met = target > 0 && ratio >= 1;

  return (
    <div className="flex items-center gap-3">
      <svg width="64" height="64" viewBox="0 0 64 64" aria-hidden="true">
        <circle cx="32" cy="32" r={R} fill="none" stroke="var(--border)" strokeWidth="5" />
        <circle
          cx="32"
          cy="32"
          r={R}
          fill="none"
          stroke={met ? "var(--text-soft)" : "rgb(var(--busy))"}
          strokeWidth="5"
          strokeDasharray={`${C * clamped} ${C}`}
          // Square caps and a start at twelve o'clock: the design system has no
          // rounded ends anywhere else.
          strokeLinecap="butt"
          transform="rotate(-90 32 32)"
          style={{ transition: "stroke-dasharray var(--dur) var(--ease)" }}
        />
      </svg>
      <div className="flex flex-col">
        {/* Units stay lowercase — see `Stat`; `.display` would render `17m` as
            `17M`. */}
        <span
          className="display text-[20px] tabular-nums"
          style={{ color: "var(--text)", textTransform: "none" }}
        >
          {format(value)}
        </span>
        <span className="micro">{label}</span>
        <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
          {target > 0 ? (
            met ? `target ${format(target)} · met` : `of ${format(target)}`
          ) : (
            "no target set"
          )}
        </span>
      </div>
    </div>
  );
}
