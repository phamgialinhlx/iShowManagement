import { useCallback, useEffect, useMemo, useState } from "react";

import { dayKey, daySummary, record } from "../lib/activity";
import { DayBars, GoalRing, HourClock, SessionSplit, StreakGrid } from "../components/ProgressCharts";
import { JiraToday } from "../components/JiraToday";
import {
  buildDashboard,
  clock,
  history,
  humanDuration,
  milestones,
  noteOf,
  personalBest,
  type SessionTask,
} from "../lib/dashboard";
import { readGoals, writeGoals, type Goals } from "../lib/goals";
import { noteTasks, toggleTask } from "../lib/note-tasks";
import { useWorkspace } from "../lib/workspace";

/**
 * Progress — the day, the week, and whether it is going anywhere.
 *
 * The rail answers "which machine needs me *now*". This answers the other
 * question, over a horizon a rail cannot show: what got done across every
 * session, how that compares with the fortnight behind it, and whether the
 * targets you set are being met.
 *
 * ## It replaces the workbench rather than floating over it
 *
 * The first version was `fixed inset-0` over the deck, and it was unreadable:
 * every panel in rmux is translucent by design, so the terminal and the rail
 * showed straight through the charts. Layering a translucent sheet over a busy
 * one cannot be fixed by tinting it — that only makes the app opaque, which is
 * the thing the whole appearance system exists to avoid. So this *is* the body
 * of the window while it is open. The title bar and footer stay, because they
 * carry the way back.
 *
 * ## Everything here is measured
 *
 * Tasks are the checkboxes in the notes people already write. Prompts and
 * commands are counted as they are sent. Time is attention time, one session at
 * a time (`lib/attention.ts`). Nothing is estimated, and the footer says what
 * date the counters started — a total that looks historical but is really "since
 * Tuesday" is worse than an absent one.
 */
export function Dashboard({ onClose }: { onClose: () => void }) {
  const sessions = useWorkspace((s) => s.sessions);
  const activate = useWorkspace((s) => s.activate);

  const [day, setDay] = useState(dayKey());
  const [version, bump] = useState(0);
  const [goals, setGoals] = useState<Goals>(readGoals);
  const [editingGoals, setEditingGoals] = useState(false);

  // Recomputed when a note, a counter or a goal changes, here or in another
  // window. Polling would be the obvious thing and is wrong: nothing changes
  // except in response to an action that already fires an event.
  useEffect(() => {
    const refresh = () => {
      setGoals(readGoals());
      bump((n) => n + 1);
    };
    for (const e of ["rmux:notes-changed", "rmux:activity-changed", "rmux:goals-changed", "storage"]) {
      window.addEventListener(e, refresh);
    }
    return () => {
      for (const e of ["rmux:notes-changed", "rmux:activity-changed", "rmux:goals-changed", "storage"]) {
        window.removeEventListener(e, refresh);
      }
    };
  }, []);

  const named = useMemo(() => sessions.map((s) => ({ id: s.id, name: s.name })), [sessions]);
  /* eslint-disable react-hooks/exhaustive-deps -- `version` is the invalidation key */
  const data = useMemo(() => buildDashboard(named, day), [named, day, version]);
  const fortnight = useMemo(() => history(14), [version]);
  const eightWeeks = useMemo(() => history(56), [version]);
  // Two ranges, because they answer different questions: today is "how has
  // *this* day been shaped", the fortnight is "when do I work". A single range
  // would have to pick one and be wrong about the other.
  const clockToday = useMemo(() => clock([day]), [day, version]);
  const clockFortnight = useMemo(() => clock(fortnight.map((p) => p.day)), [fortnight, version]);
  const bestTasks = useMemo(() => personalBest("tasksDone"), [version]);
  const allTimeTasks = useMemo(
    () => named.reduce((n, s) => n + noteTasks(noteOf(s.id)).filter((t) => t.done).length, 0),
    [named, version],
  );
  /* eslint-enable react-hooks/exhaustive-deps */

  /** Ticking here writes to the session's own note — one source of truth. */
  const toggle = useCallback((task: SessionTask) => {
    const text = noteOf(task.sessionId);
    const next = toggleTask(text, task.line);
    if (next === text) return;
    try {
      localStorage.setItem(`rmux.note.${task.sessionId}`, next);
      record(task.sessionId, "tasksDone", task.done ? -1 : 1);
      window.dispatchEvent(new CustomEvent("rmux:notes-changed"));
    } catch {
      /* a full localStorage must not break ticking */
    }
  }, []);

  const isToday = day === dayKey();
  const minutes = Math.round(data.totals.seconds / 60);

  /**
   * Tasks finished **on this day**, which is what a daily target means.
   *
   * The ring used to read `data.tasks.done` — the number of ticked boxes on the
   * board, all-time, across every note. Against a target of 10 that showed
   * `12 · met` on a day nothing had been ticked at all, and it could never go
   * down. Reported as the graph being wrong, which it was.
   *
   * `activity.ts` records a completion at the moment it happens for exactly this
   * reason: a note stores only the *current* state of its checkboxes, so "tasks
   * finished today" is unknowable from the notes alone. It is the same series
   * the 14-day chart below already plots.
   */
  const tasksToday = useMemo(() => daySummary(day).tasksDone, [day, version]);

  return (
    <div className="progress-page flex min-h-0 flex-1 flex-col">
      <header
        className="flex shrink-0 items-center gap-3 border-b px-4 py-2"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="kicker">PROGRESS</span>
        <span className="micro" style={{ color: "var(--text-faint)" }}>
          {isToday ? "TODAY" : day}
        </span>
        {!isToday && (
          <button type="button" className="chip" onClick={() => setDay(dayKey())}>
            BACK TO TODAY
          </button>
        )}
        <button type="button" className="chip ml-auto" onClick={onClose}>
          CLOSE
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        <div className="mx-auto flex w-full max-w-[1180px] flex-col gap-5">
          {/* ── goals ─────────────────────────────────────────────────────── */}
          <section className="flex flex-wrap items-center gap-8">
            <GoalRing
              label="TASKS"
              value={tasksToday}
              target={goals.tasks}
              format={(n) => String(Math.round(n))}
            />
            <GoalRing
              label="FOCUS"
              value={minutes}
              target={goals.minutes}
              format={(n) => (n >= 60 ? `${Math.floor(n / 60)}h ${String(Math.round(n) % 60).padStart(2, "0")}m` : `${Math.round(n)}m`)}
            />

            <div className="flex flex-col gap-1">
              <span
                className="display text-[20px] tabular-nums"
                style={{ color: "var(--text)", textTransform: "none" }}
              >
                {data.streak || "—"}
                {data.streak ? <span className="text-[13px]"> days</span> : null}
              </span>
              <span className="micro">STREAK</span>
              <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
                best day {bestTasks || "—"} tasks
              </span>
            </div>

            <div className="ml-auto">
              {editingGoals ? (
                <GoalEditor
                  goals={goals}
                  onDone={(next) => {
                    setGoals(writeGoals(next));
                    setEditingGoals(false);
                  }}
                  onCancel={() => setEditingGoals(false)}
                />
              ) : (
                <button type="button" className="chip" onClick={() => setEditingGoals(true)}>
                  SET GOALS
                </button>
              )}
            </div>
          </section>

          {/* ── the day in numbers ────────────────────────────────────────── */}
          <section className="flex flex-wrap gap-6">
            <Stat label="TASKS" value={`${data.tasks.done}/${data.tasks.total}`} />
            <Stat label="TIME" value={humanDuration(data.totals.seconds)} />
            <Stat label="PROMPTS" value={String(data.totals.prompts || "—")} />
            <Stat label="COMMANDS" value={String(data.totals.commands || "—")} />
            <Stat label="SESSIONS" value={String(data.rows.length || "—")} />
          </section>

          {/* ── charts ────────────────────────────────────────────────────── */}
          <section className="grid gap-5" style={{ gridTemplateColumns: "repeat(auto-fit, minmax(320px, 1fr))" }}>
            <Card>
              <DayBars points={fortnight} field="tasksDone" label="TASKS DONE · 14 DAYS" />
            </Card>
            <Card>
              <DayBars
                points={fortnight}
                field="seconds"
                label="FOCUS TIME · 14 DAYS"
                format={(n) => humanDuration(n)}
              />
            </Card>
            <Card>
              <DayBars points={fortnight} field="commands" label="COMMANDS · 14 DAYS" />
            </Card>
            <Card>
              <StreakGrid points={eightWeeks} />
            </Card>
            <Card>
              <HourClock points={clockToday} label={isToday ? "HOURS · TODAY" : `HOURS · ${day}`} />
            </Card>
            <Card>
              <HourClock points={clockFortnight} label="HOURS · TYPICAL DAY (14 DAYS)" />
            </Card>
          </section>

          <section className="grid gap-5" style={{ gridTemplateColumns: "repeat(auto-fit, minmax(320px, 1fr))" }}>
            <Card>
              <span className="micro">WHERE THE DAY WENT</span>
              <div className="mt-2">
                <SessionSplit
                  rows={data.rows.map((r) => ({ id: r.id, name: r.name, seconds: r.activity.seconds }))}
                />
              </div>
            </Card>

            <Card>
              <span className="micro">TASKS · ALL SESSIONS</span>
              {data.tasks.items.length === 0 ? (
                <p className="data mt-2 text-[11px]" style={{ color: "var(--text-faint)" }}>
                  Write <code>- [ ] something</code> in any session&rsquo;s note and it appears here.
                </p>
              ) : (
                <ul className="mt-2 flex max-h-[260px] flex-col overflow-y-auto">
                  {data.tasks.items.map((task) => (
                    <li
                      key={`${task.sessionId}:${task.line}`}
                      className="flex items-center gap-2 border-b py-[5px]"
                      style={{ borderColor: "var(--border)" }}
                    >
                      <input
                        type="checkbox"
                        className="note-check"
                        checked={task.done}
                        onChange={() => toggle(task)}
                        aria-label={task.label}
                      />
                      <span
                        className="data flex-1 truncate text-[12px]"
                        style={{
                          color: task.done ? "var(--text-soft)" : "var(--text)",
                          textDecoration: task.done ? "line-through" : "none",
                        }}
                      >
                        {task.label || <em style={{ color: "var(--text-faint)" }}>untitled</em>}
                      </span>
                      {/* The session name is a link: the reason to read a task
                          list is usually to go and do one of them. */}
                      <button
                        type="button"
                        className="micro link shrink-0"
                        style={{ color: "var(--text-faint)" }}
                        onClick={() => {
                          activate(task.sessionId);
                          onClose();
                        }}
                        title={`Go to ${task.sessionName}`}
                      >
                        {task.sessionName}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </Card>
          </section>

          {/* ── jira ──────────────────────────────────────────────────────── */}
          <JiraToday
            sessions={named}
            onOpen={(id) => {
              activate(id);
              onClose();
            }}
          />

          {/* ── per session ───────────────────────────────────────────────── */}
          <section className="flex flex-col gap-2">
            <span className="micro">BY SESSION</span>
            {data.rows.length === 0 ? (
              <p className="data inset px-3 py-3 text-[11px]" style={{ color: "var(--text-faint)", border: "1px solid var(--border)" }}>
                Nothing recorded {isToday ? "yet today" : "on this day"}.
              </p>
            ) : (
              <table className="data w-full text-[12px]">
                <thead>
                  <tr className="micro" style={{ color: "var(--text-faint)" }}>
                    <th className="py-1 text-left font-normal">SESSION</th>
                    <th className="py-1 text-right font-normal">TIME</th>
                    <th className="py-1 text-right font-normal">PROMPTS</th>
                    <th className="py-1 text-right font-normal">COMMANDS</th>
                    <th className="py-1 text-right font-normal">TASKS</th>
                  </tr>
                </thead>
                <tbody>
                  {data.rows.map((row) => (
                    <tr key={row.id} className="border-t" style={{ borderColor: "var(--border)" }}>
                      <td className="py-[5px]">
                        <button
                          type="button"
                          className="link truncate text-left"
                          onClick={() => {
                            activate(row.id);
                            onClose();
                          }}
                        >
                          {row.name}
                        </button>
                      </td>
                      <td className="py-[5px] text-right tabular-nums">{humanDuration(row.activity.seconds)}</td>
                      <td className="py-[5px] text-right tabular-nums">{row.activity.prompts || "—"}</td>
                      <td className="py-[5px] text-right tabular-nums">{row.activity.commands || "—"}</td>
                      <td className="py-[5px] text-right tabular-nums">
                        {row.tasks.total ? `${row.tasks.done}/${row.tasks.total}` : "—"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>

          {/* ── milestones ────────────────────────────────────────────────── */}
          <section className="flex flex-col gap-2">
            <span className="micro">ACHIEVEMENTS</span>
            <div className="grid gap-2" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))" }}>
              {milestones(data, allTimeTasks).map((m) => (
                <div key={m.id} className="inset flex flex-col gap-1 px-2 py-2" style={{ border: "1px solid var(--border)" }}>
                  <div className="flex items-baseline justify-between gap-2">
                    <span className="micro" style={{ color: m.reached ? "var(--text)" : "var(--text-faint)" }}>
                      {m.label}
                    </span>
                    {m.reached && (
                      <span className="micro" style={{ color: "rgb(var(--busy))" }}>
                        DONE
                      </span>
                    )}
                  </div>
                  <div className="h-[3px]" style={{ background: "var(--border)" }}>
                    <div
                      className="h-full"
                      style={{
                        width: `${Math.round(m.progress * 100)}%`,
                        background: m.reached ? "var(--text-soft)" : "rgb(var(--busy))",
                        transition: "width var(--dur) var(--ease)",
                      }}
                    />
                  </div>
                  <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
                    {m.hint}
                  </span>
                </div>
              ))}
            </div>
          </section>

          {/* The counters cannot know anything from before they existed. Saying
              so is the difference between a total and a total-shaped guess. */}
          <p className="data pb-2 text-[10px]" style={{ color: "var(--text-faint)" }}>
            Prompts, commands and time are counted from {data.countingSince ?? "your first session"}{" "}
            onward. Tasks are the checkboxes in each session&rsquo;s note, read live; the day a task
            was finished is recorded as you tick it.
          </p>
        </div>
      </div>
    </div>
  );
}

/**
 * Setting a target.
 *
 * Two numbers, applied on Save rather than as you type — a ring that re-scales
 * on every keystroke while you are still choosing the number is the app moving
 * under your hands. Blank or zero means "not tracking this", so there is no
 * separate switch to get out of step with the value.
 */
function GoalEditor({
  goals,
  onDone,
  onCancel,
}: {
  goals: Goals;
  onDone: (next: Goals) => void;
  onCancel: () => void;
}) {
  const [tasks, setTasks] = useState(String(goals.tasks || ""));
  const [minutes, setMinutes] = useState(String(goals.minutes || ""));

  return (
    <form
      className="inset flex items-end gap-3 px-3 py-2"
      style={{ border: "1px solid var(--border)" }}
      onSubmit={(e) => {
        e.preventDefault();
        onDone({ tasks: Number(tasks) || 0, minutes: Number(minutes) || 0 });
      }}
    >
      <label className="flex flex-col gap-1">
        <span className="micro">TASKS / DAY</span>
        <input
          className="field data w-[80px] px-2 py-1 text-[12px]"
          value={tasks}
          onChange={(e) => setTasks(e.target.value.replace(/[^\d]/g, ""))}
          placeholder="—"
          inputMode="numeric"
        />
      </label>
      <label className="flex flex-col gap-1">
        <span className="micro">FOCUS MIN / DAY</span>
        <input
          className="field data w-[96px] px-2 py-1 text-[12px]"
          value={minutes}
          onChange={(e) => setMinutes(e.target.value.replace(/[^\d]/g, ""))}
          placeholder="—"
          inputMode="numeric"
        />
      </label>
      <button type="submit" className="chip">
        SAVE
      </button>
      <button type="button" className="chip" onClick={onCancel}>
        CANCEL
      </button>
    </form>
  );
}

function Card({ children }: { children: React.ReactNode }) {
  return (
    <div className="inset px-3 py-3" style={{ border: "1px solid var(--border)" }}>
      {children}
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col">
      {/*
        `text-transform: none` against `.display`, which uppercases everything.
        That turned `17m` into `17M` — a duration reading as millions, which is
        the one thing a figure on a dashboard must never do. Units are lowercase
        or they are a different quantity.
      */}
      <span
        className="display text-[26px] tabular-nums"
        style={{ color: "var(--text)", textTransform: "none" }}
      >
        {value}
      </span>
      <span className="micro">{label}</span>
    </div>
  );
}
