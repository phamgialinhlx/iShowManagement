import { useEffect, useState } from "react";

import { dayKey, daySummary } from "../../lib/activity";
import { readGoals, type Goals as GoalsShape } from "../../lib/goals";

/**
 * Today against the targets you set — without opening the dashboard.
 *
 * The Progress page already answers this, and answering it there only is the
 * problem: a goal you have to go and look at is a goal you check twice a day and
 * then forget. In the rail it sits beside the work all day, which is the only
 * place a target changes what anyone does.
 *
 * ## Bars, not rings
 *
 * The dashboard uses rings because it has the room for two 64px figures. The
 * rail is a 216px column shared with five other instruments, and a ring small
 * enough to fit here is a ring you cannot read a value off. A bar is legible at
 * any width and stacks.
 *
 * ## A target of zero means "not tracking this"
 *
 * The same convention the Progress page uses — nobody holds a target of zero, so
 * the value carries its own off state and there is no second switch beside each
 * field. With neither set, the widget says so and points at where to set them
 * rather than drawing two empty bars, which would read as "you have done
 * nothing" instead of "nothing is being measured".
 */
export function Goals() {
  const [goals, setGoals] = useState<GoalsShape>(readGoals);
  const [today, setToday] = useState(() => daySummary());
  const [minutes, setMinutes] = useState(() => Math.round(daySummary().seconds / 60));

  // Recomputed on the same signals the dashboard listens to, rather than on a
  // timer: these change when *something happens*, and a poll would redraw an
  // unchanged bar every few seconds for the life of the app.
  useEffect(() => {
    const refresh = () => {
      const summary = daySummary(dayKey());
      setToday(summary);
      setMinutes(Math.round(summary.seconds / 60));
      setGoals(readGoals());
    };
    refresh();
    const events = ["rmux:activity-changed", "rmux:goals-changed", "rmux:notes-changed", "storage"];
    for (const e of events) window.addEventListener(e, refresh);
    return () => {
      for (const e of events) window.removeEventListener(e, refresh);
    };
  }, []);

  if (goals.tasks <= 0 && goals.minutes <= 0) {
    return (
      <span className="micro" style={{ color: "var(--text-faint)" }}>
        NO TARGETS — SET THEM IN PROGRESS
      </span>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      {goals.tasks > 0 && (
        <Bar label="TASKS" value={today.tasksDone} target={goals.tasks} format={(n) => String(n)} />
      )}
      {goals.minutes > 0 && (
        <Bar
          label="FOCUS"
          value={minutes}
          target={goals.minutes}
          format={(n) =>
            n >= 60 ? `${Math.floor(n / 60)}h ${String(n % 60).padStart(2, "0")}m` : `${n}m`
          }
        />
      )}
    </div>
  );
}

function Bar({
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
  const met = ratio >= 1;
  // Clamped for the *drawing* only. Past the target the bar is full and the
  // surplus is a number — a bar that kept growing would either overflow the
  // widget or rescale, and rescaling makes yesterday's full bar look half done.
  const filled = Math.max(0, Math.min(1, ratio));
  const over = met ? Math.round(value - target) : 0;

  return (
    <div className="flex flex-col gap-[3px]">
      <div className="flex items-baseline justify-between gap-2">
        <span className="micro">{label}</span>
        <span className="data text-[10.5px] tabular-nums" style={{ color: "var(--text)" }}>
          {format(value)}
          <span style={{ color: "var(--text-faint)" }}> / {format(target)}</span>
        </span>
      </div>

      <div
        className="w-full overflow-hidden"
        style={{ height: 5, background: "color-mix(in srgb, var(--text) 10%, transparent)" }}
        role="progressbar"
        aria-valuenow={Math.round(ratio * 100)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label={`${label} ${format(value)} of ${format(target)}`}
      >
        <div
          style={{
            height: "100%",
            width: `${filled * 100}%`,
            // Met is the **brightest** state. It read as `--text-soft` on the
            // dashboard ring — dimmer than the amber of being halfway — so
            // finishing a goal made the display go quiet, which is backwards.
            background: met ? "var(--text)" : "rgb(var(--busy))",
            transition: "width var(--dur) var(--ease)",
          }}
        />
      </div>

      {met && (
        <span className="micro" style={{ color: "var(--text-faint)" }}>
          MET{over > 0 ? ` · +${format(over)}` : ""}
        </span>
      )}
    </div>
  );
}
