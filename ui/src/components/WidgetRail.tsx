import { useCallback, useEffect, useState } from "react";
import { motion, Reorder, useDragControls } from "motion/react";

import { HostStatus } from "./widgets/HostStatus";
import { PanelLoader } from "./PanelLoader";
import { TopProcesses } from "./widgets/TopProcesses";
import { TokenSpend } from "./widgets/TokenSpend";
import { Clock } from "./widgets/Clock";
import { Note } from "./widgets/Note";
import { SentPrompts } from "./widgets/SentPrompts";
import { Uplink } from "./widgets/Uplink";
import { JiraProgress } from "./widgets/JiraProgress";

import {
  api,
  isTauri,
  type ClaudeStatus,
  type MetricsSample,
  type TokenUsage,
  type TargetRef,
} from "../lib/api";
import { contextLimit } from "../lib/context-window";
import { ContextMeter } from "./ContextMeter";
import { basename } from "../lib/workspace-model";
import { useRailWidth } from "../lib/rail-width";
import { RailGrip } from "./RailGrip";
import { useWorkspace } from "../lib/workspace";
import { bench } from "../lib/debug-log";

/**
 * What the instruments need from the active session, flattened.
 *
 * v3 splits the fused `Session`: `target` comes from the Server, `folder` from
 * the Project. `Workbench` resolves them and hands this down, so the widgets do
 * not each reach into the store's tree.
 */
export type Active = {
  id: string;
  target: TargetRef;
  folder: string;
  resume?: string;
  contextWindow?: number;
  jiraProject?: string;
};

/**
 * The instrument rail.
 *
 * Glanceable state for the session you are in: what the host is doing, what
 * Claude has spent, where you are connected. Everything here answers a question
 * you would otherwise interrupt your work to go and look up.
 *
 * Design rules that shape every widget below:
 *
 *  - **Red is not decoration.** Nothing in here is red, because nothing in here
 *    is something you must act on — a host at 90% CPU is usually a host doing its
 *    job. Load is amber. Red belongs to the session rail, where it means "this
 *    one is waiting for you".
 *  - **Bars move, numbers do not.** A meter animates its bar toward the new
 *    reading; the printed figure snaps. Easing a number shows a value that was
 *    never measured.
 *  - **No blinking.** Liveness comes from data changing.
 */

const RAIL_DEFAULT = 244;
/** Collapsed removes the rail from the layout entirely (see the note at its use).
 *  Re-opening is the footer toggle, so nothing needs to stay on screen. */
const RAIL_COLLAPSED = 0;
/** Samples kept for the history chart. Its x-axis is "the last 30", so an
 *  unbounded buffer would quietly change what the chart means. */
const HISTORY = 30;


const compact = (n: number) => {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 100_000 ? 0 : 1)}k`;
  return String(n);
};

/**
 * A panel with a corner-bracketed header — the recurring instrument frame.
 *
 * `onDragStart` turns the header into the drag handle. **Only the header**, and
 * that is not a style choice: this rail scrolls, and a widget draggable by its
 * whole body would swallow every scroll gesture that began over one — which is
 * all of them. It also has to stay a real handle rather than an icon beside the
 * title, because a 9px grab target in a 244px rail is a control most people
 * never find.
 */
function Widget({
  title,
  children,
  onDragStart,
}: {
  title: string;
  children: React.ReactNode;
  onDragStart?: (event: React.PointerEvent) => void;
}) {
  return (
    <section className="inset" style={{ border: "1px solid var(--border)" }}>
      <header
        className="flex items-center justify-between border-b px-2 py-[5px]"
        style={{
          borderColor: "var(--border)",
          cursor: onDragStart ? "grab" : undefined,
          // The rail is a scroller and this is a drag surface. Without it, a
          // drag begun on the header scrolls the rail on touch devices instead.
          touchAction: onDragStart ? "none" : undefined,
        }}
        onPointerDown={onDragStart}
      >
        <span className="micro">{title}</span>
        {/* A tick mark, not an icon: it reads as instrument chrome. Grip bars
            when the widget can be moved, so the affordance is visible rather
            than something you have to try. */}
        {onDragStart ? (
          <svg width="9" height="9" viewBox="0 0 12 12" aria-hidden="true" style={{ color: "var(--text-faint)" }}>
            <path d="M1 3h10M1 6h10M1 9h10" stroke="currentColor" strokeWidth="1.5" strokeLinecap="square" />
          </svg>
        ) : (
          <span aria-hidden="true" style={{ color: "var(--text-faint)", fontSize: 9 }}>
            ┐
          </span>
        )}
      </header>
      <div className="px-2 py-2">{children}</div>
    </section>
  );
}

/** Label on the left, value on the right, aligned in a column so they compare. */
function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-2 py-[2px]">
      <span className="micro shrink-0">{label}</span>
      <span
        className="data truncate text-[10.5px]"
        style={{ color: "var(--text)" }}
        title={value}
      >
        {value}
      </span>
    </div>
  );
}

/**
 * The host sample both host widgets read.
 *
 * Shared through a hook rather than by nesting one widget inside the other,
 * because the two are separately orderable — HOST and TOP PROCESSES are
 * independent items in the rail, and a fragment returning both would move them
 * as one. Polling once for both is the point: two timers against the same host
 * would double the ssh round trips for the same numbers.
 */
/**
 * @param enabled  false when neither host instrument is on. **This is the whole
 *   point of the disable switch.** Unmounting a widget stops its own effects,
 *   but this poller lives in the *rail*, above them — so with HOST and TOP
 *   PROCESSES both switched off it would have kept opening an SSH channel every
 *   couple of seconds for readings nothing rendered. A setting that hides a
 *   widget while it keeps working is not a setting, it is a lie.
 */
function useHostSample(target: TargetRef, enabled: boolean) {
  const [sample, setSample] = useState<MetricsSample | null>(null);
  const [failed, setFailed] = useState(false);
  const [cpuHistory, setCpuHistory] = useState<number[]>([]);
  const [ramHistory, setRamHistory] = useState<number[]>([]);
  // A rolling window of recent throughput, not an all-time maximum.
  //
  // This was `Math.max(peak, now)`, which only ever grew — so one burst (an
  // agent upload, a `git clone`) pinned the scale permanently and every normal
  // reading afterwards rendered as a fraction of a percent, which is to say a
  // bar that is always empty. A window over the same 30 samples the charts use
  // means the bar answers "busy compared to lately", which is the only
  // comparison a network rate has: there is no equivalent of "100% of CPU".
  const [netHistory, setNetHistory] = useState<number[]>([]);

  useEffect(() => {
    if (!enabled) return;
    if (!isTauri()) return;
    let cancelled = false;

    const tick = async () => {
      try {
        const next = await api.metricsSample(target);
        if (cancelled) return;
        setSample(next);
        setFailed(false);

        // A fixed-length window: the chart's x-axis is "the last 30 samples",
        // so an unbounded buffer would silently change what it means.
        if (next.cpuPercent != null) {
          setCpuHistory((h) => [...h, next.cpuPercent!].slice(-HISTORY));
        }
        if (next.memoryTotalBytes > 0) {
          const pct = (next.memoryUsedBytes / next.memoryTotalBytes) * 100;
          setRamHistory((h) => [...h, pct].slice(-HISTORY));
        }
        // Only once a rate exists. `null` is "no second sample yet", and
        // feeding that in as a zero would drag the window down with a reading
        // nobody took.
        if (next.netRxBps != null || next.netTxBps != null) {
          const now = (next.netRxBps ?? 0) + (next.netTxBps ?? 0);
          setNetHistory((h) => [...h, now].slice(-HISTORY));
        }
      } catch {
        if (!cancelled) setFailed(true);
      }
    };

    // Poll only while the window is on screen. Each tick is an SSH round trip,
    // and nothing renders this reading when the window is hidden — so stand the
    // timer down entirely (not merely skip the body) to actually drop the
    // wakeups. Mirrors ClaudePanel's status poll.
    let timer: number | undefined;
    const start = () => {
      if (timer === undefined && !cancelled) {
        bench(`gate host=${target.host ?? "local"} action=start`);
        void tick();
        timer = window.setInterval(() => void tick(), 2000);
      }
    };
    const stop = () => {
      if (timer !== undefined) {
        bench(`gate host=${target.host ?? "local"} action=stop`);
        window.clearInterval(timer);
        timer = undefined;
      }
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") start();
      else stop();
    };
    document.addEventListener("visibilitychange", onVisibility);
    if (document.visibilityState === "visible") start();

    return () => {
      cancelled = true;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
    // `enabled` belongs here, not just in the early return: without it the
    // effect never re-runs when an instrument is switched back on, so the rail
    // would show HOST again and never fill it. Leaving it out is the classic
    // way a disable switch becomes a one-way door.
  }, [target, enabled]);

  // A floor of 1 keeps the division safe on an idle host; the bar reads empty
  // either way, which is correct — an idle link *is* empty.
  const netPeak = Math.max(1, ...netHistory);

  return { sample, failed, cpuHistory, ramHistory, netPeak };
}

/** The host: name, uptime, CPU / RAM / NET, and their recent history. */
function HostWidget({
  host,
  onDragStart,
}: {
  host: ReturnType<typeof useHostSample>;
  onDragStart?: (event: React.PointerEvent) => void;
}) {
  if (host.failed && !host.sample) {
    return (
      <Widget title="HOST" onDragStart={onDragStart}>
        <span className="micro">unreachable</span>
      </Widget>
    );
  }
  if (!host.sample) {
    return (
      <Widget title="HOST" onDragStart={onDragStart}>
        <PanelLoader variant="inline" phase="READING THE HOST" />
      </Widget>
    );
  }

  return (
    <Widget title="HOST" onDragStart={onDragStart}>
      <HostStatus
        sample={host.sample}
        cpuHistory={host.cpuHistory}
        ramHistory={host.ramHistory}
        netPeak={host.netPeak}
      />
    </Widget>
  );
}

/** What is actually using the machine. */
function ProcessesWidget({
  target,
  host,
  onDragStart,
}: {
  target: TargetRef;
  host: ReturnType<typeof useHostSample>;
  onDragStart?: (event: React.PointerEvent) => void;
}) {
  const sample = host.sample;
  if (!sample) return null;

  const memPercent = sample.memoryTotalBytes
    ? (sample.memoryUsedBytes / sample.memoryTotalBytes) * 100
    : 0;

  return (
    <Widget title="TOP PROCESSES" onDragStart={onDragStart}>
      <TopProcesses
        target={target}
        hostname={sample.hostname}
        cpuPercent={sample.cpuPercent}
        memoryPercent={memPercent}
        cores={sample.cores}
      />
    </Widget>
  );
}

/** What this conversation has cost. */
function UsageWidget({
  session,
  onDragStart,
}: {
  session: Active;
  onDragStart?: (event: React.PointerEvent) => void;
}) {
  const [usage, setUsage] = useState<TokenUsage | null>(null);
  const [status, setStatus] = useState<ClaudeStatus | null>(null);
  // Token spend only moves when Claude produces output, so this is driven by the
  // session's status edges rather than a timer — an idle or backgrounded session
  // reads nothing. Same signal the rail uses; see status-watch / ClaudePanel.
  const sessionStatus = useWorkspace((s) => s.runtime[session.id]?.status ?? "idle");

  const tick = useCallback(async () => {
    if (!isTauri()) return;
    try {
      // A small tail: the widget needs recent turns, not the whole history, and
      // this reads a file that can be hundreds of MB.
      const t = await api.claudeTranscript(session.target, session.folder, session.resume, 256 * 1024);
      setUsage(t.usage);
      setStatus(t.status);
    } catch {
      // Not worth reporting in a widget — the Claude tab already shows why.
    }
  }, [session.target, session.folder, session.resume]);

  // Baseline on mount, and on reconnect (new resume/target/folder).
  useEffect(() => {
    bench(`fetch kind=token session=${session.id} trigger=open`);
    void tick();
  }, [tick, session.id]);

  // Refresh only when Claude finished a turn (`working → idle`) or paused for
  // input (`→ waiting`) — the moments spend changes. `working` is skipped: no
  // new tokens land at the start of a turn.
  useEffect(() => {
    if (sessionStatus === "idle" || sessionStatus === "waiting") {
      bench(`fetch kind=token session=${session.id} trigger=${sessionStatus}`);
      void tick();
    }
  }, [sessionStatus, tick, session.id]);

  return (
    <Widget title="TOKEN SPEND" onDragStart={onDragStart}>
      <div className="flex flex-col gap-2">
        {/* `?? 0` conflated "not read yet" with "nothing spent", so the widget
            asserted "no usage recorded yet" while it was still reading — a
            different and wrong fact, and the one people act on. */}
        {usage ? (
          <TokenSpend input={usage.input} output={usage.output} />
        ) : (
          <PanelLoader variant="inline" phase="READING THE TRANSCRIPT" />
        )}

        <ContextRow status={status} window={session.contextWindow} />

        {/* Cache reads dwarf everything else and are nearly free; showing them
            beside the billed figures is what makes the billed ones legible. */}
        <Row label="CACHED" value={usage ? compact(usage.cacheRead) : "\u2014"} />
        <Row label="TURNS" value={usage ? String(usage.turns) : "\u2014"} />
      </div>
    </Widget>
  );
}

/** How full the context window is. Drawn by `ContextMeter`. */
function ContextRow({ status, window: configured }: { status: ClaudeStatus | null; window?: number }) {
  const context = status?.contextTokens ?? 0;
  // The model is worth showing even before a single turn has been recorded —
  // it is the thing that decides what everything else here costs.
  const model = status?.model?.replace(/^claude-/, "");

  if (!context) {
    return (
      <>
        {model && <Row label="MODEL" value={model} />}
        <Row label="CONTEXT" value="—" />
      </>
    );
  }

  return (
    <div className="flex flex-col gap-1">
      {model && <Row label="MODEL" value={model} />}
      <ContextMeter
        tokens={context}
        limit={contextLimit(status?.model, context, configured)}
      />
    </div>
  );
}

/** Where you are, and for how long. */
function SessionWidget({
  session,
  onDragStart,
}: {
  session: Active;
  onDragStart?: (event: React.PointerEvent) => void;
}) {
  const [seconds, setSeconds] = useState(0);

  useEffect(() => {
    setSeconds(0);
    const timer = setInterval(() => {
      // Foreground only: counting while the window is hidden measures elapsed
      // time, not time spent.
      if (document.visibilityState === "visible") setSeconds((s) => s + 1);
    }, 1000);
    return () => clearInterval(timer);
  }, [session.id]);

  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const secs = seconds % 60;
  const pad = (n: number) => String(n).padStart(2, "0");

  return (
    <Widget title="SESSION" onDragStart={onDragStart}>
      <Row label="HOST" value={session.target.host ?? "local"} />
      <Row label="FOLDER" value={basename(session.folder)} />
      <Row label="CLAUDE" value={session.resume ? session.resume.slice(0, 8) : "new"} />
      <Row
        label="FOCUS"
        value={hours > 0 ? `${hours}:${pad(minutes)}:${pad(secs)}` : `${minutes}:${pad(secs)}`}
      />
    </Widget>
  );
}

/**
 * Which instruments exist, and the order they start in.
 *
 * The order is the operator's to change, so it is stored — but this list stays
 * the source of truth for *membership*. A stored order is reconciled against it
 * on every read: ids that no longer exist are dropped, and new ones are
 * appended rather than hidden. Trusting the stored array alone would mean every
 * widget added after someone first dragged one is invisible to them forever,
 * with nothing to suggest why.
 */
const INSTRUMENTS = ["clock", "host", "processes", "uplink", "usage", "sent", "jira", "note", "session"] as const;
type InstrumentId = (typeof INSTRUMENTS)[number];

const ORDER_KEY = "rmux.widgetRail.order";
const ENABLED_KEY = "rmux.widgetRail.enabled";
/**
 * Which instruments existed when the preference above was written.
 *
 * Without this the enabled list is ambiguous: it records only what is *on*, so
 * an id being absent means either "switched off" or "did not exist yet", and
 * those want opposite treatment. Storing the vocabulary alongside the choice
 * makes the difference readable.
 */
const KNOWN_KEY = "rmux.widgetRail.known";

/**
 * Which instruments are switched on.
 *
 * Absent means *all of them* — the default is the full rail, and a first run
 * must not open to an empty one. Reconciled against `INSTRUMENTS` like the
 * order is, so a widget added later appears rather than being silently off for
 * anyone who has ever touched this.
 */
function readEnabled(): Set<InstrumentId> {
  let stored: unknown;
  try {
    stored = JSON.parse(localStorage.getItem(ENABLED_KEY) ?? "null");
  } catch {
    stored = null;
  }
  if (!Array.isArray(stored)) return new Set(INSTRUMENTS);

  const valid = new Set<string>(INSTRUMENTS);
  const on = new Set<InstrumentId>();
  for (const id of stored) {
    if (typeof id === "string" && valid.has(id)) on.add(id as InstrumentId);
  }

  // **A widget added since this preference was written is ON.**
  //
  // This used to intersect the stored list with `INSTRUMENTS` and stop, so a new
  // instrument was *off* for everyone who had ever opened the instruments menu —
  // invisible, with nothing to say it existed. The comment here already claimed
  // the set was reconciled "like the order is"; `readOrder` appends what it has
  // not seen and this did not, so the rule was written down and never
  // implemented.
  //
  // It cannot simply add every absent id, because the enabled list records only
  // what is *on* — absent means "switched off" **or** "did not exist yet", and
  // those want opposite answers. `KNOWN_KEY` stores the vocabulary the choice
  // was made against, so the two are distinguishable: absent from `enabled` but
  // present in `known` is a decision and stays off; absent from both is new.
  let known: unknown;
  try {
    known = JSON.parse(localStorage.getItem(KNOWN_KEY) ?? "null");
  } catch {
    known = null;
  }
  // No vocabulary recorded means the preference predates this. The safe reading
  // is that it knew about what it enabled and nothing more, so anything else is
  // treated as new. That switches previously-hidden widgets back on once, which
  // is visible and one click to undo — where the alternative failure, a widget
  // that silently never appears, is neither.
  const vocabulary = new Set<string>(Array.isArray(known) ? known.filter((x) => typeof x === "string") : stored.filter((x) => typeof x === "string"));
  for (const id of INSTRUMENTS) {
    if (!vocabulary.has(id)) on.add(id);
  }
  return on;
}

function readOrder(): InstrumentId[] {
  let stored: unknown;
  try {
    stored = JSON.parse(localStorage.getItem(ORDER_KEY) ?? "null");
  } catch {
    stored = null;
  }
  const known = new Set<string>(INSTRUMENTS);
  const seen = new Set<string>();
  const out: InstrumentId[] = [];

  if (Array.isArray(stored)) {
    for (const id of stored) {
      if (typeof id === "string" && known.has(id) && !seen.has(id)) {
        seen.add(id);
        out.push(id as InstrumentId);
      }
    }
  }
  for (const id of INSTRUMENTS) {
    if (!seen.has(id)) out.push(id);
  }
  return out;
}

/** One draggable instrument. */
function Instrument({
  id,
  session,
  host,
}: {
  id: InstrumentId;
  session: Active;
  host: ReturnType<typeof useHostSample>;
}) {
  // Dragging is started by the header, not by the item — see `Widget`. Without
  // `dragListener={false}` the whole widget is a drag surface and the rail
  // stops scrolling.
  const controls = useDragControls();
  const start = (event: React.PointerEvent) => controls.start(event);

  const body = (() => {
    switch (id) {
      case "clock":
        return (
          <Widget title="MISSION TIME" onDragStart={start}>
            <Clock />
          </Widget>
        );
      case "host":
        return <HostWidget host={host} onDragStart={start} />;
      case "processes":
        return <ProcessesWidget target={session.target} host={host} onDragStart={start} />;
      case "usage":
        return <UsageWidget session={session} onDragStart={start} />;
      case "jira":
        return session.jiraProject ? (
          <Widget title={`JIRA · ${session.jiraProject}`} onDragStart={start}>
            <JiraProgress project={session.jiraProject} sessionId={session.id} />
          </Widget>
        ) : null;
      case "uplink":
        return (
          <Widget title="UPLINK" onDragStart={start}>
            <Uplink target={session.target} />
          </Widget>
        );
      case "sent":
        return (
          <Widget title="SENT" onDragStart={start}>
            <SentPrompts target={session.target} folder={session.folder} resume={session.resume} />
          </Widget>
        );
      case "note":
        return (
          <Widget title="NOTE" onDragStart={start}>
            <Note sessionId={session.id} />
          </Widget>
        );
      case "session":
        return <SessionWidget session={session} onDragStart={start} />;
    }
  })();

  if (!body) return null;

  return (
    <Reorder.Item
      value={id}
      dragListener={false}
      dragControls={controls}
      className="pb-2"
      // Lifted while moving so it reads as picked up rather than as the list
      // rearranging itself around a stationary card.
      whileDrag={{ scale: 1.02, zIndex: 2, cursor: "grabbing" }}
      transition={{ type: "spring", stiffness: 520, damping: 42 }}
    >
      {body}
    </Reorder.Item>
  );
}

/** The instruments, in the operator's own order. */
function Instruments({ session, customising }: { session: Active; customising: boolean }) {
  const [order, setOrder] = useState<InstrumentId[]>(readOrder);
  const [enabled, setEnabled] = useState<Set<InstrumentId>>(readEnabled);

  // Polled once here and shared, so HOST and TOP PROCESSES can be ordered
  // independently without doubling the ssh traffic behind them — and not polled
  // at all when neither is on.
  const host = useHostSample(session.target, enabled.has("host") || enabled.has("processes"));

  const toggle = (id: InstrumentId) => {
    const next = new Set(enabled);
    if (!next.delete(id)) next.add(id);
    setEnabled(next);
    try {
      localStorage.setItem(ENABLED_KEY, JSON.stringify([...next]));
      // Record the vocabulary this choice was made against, so a widget added
      // later can be told apart from one deliberately switched off.
      localStorage.setItem(KNOWN_KEY, JSON.stringify([...INSTRUMENTS]));
    } catch {
      // A full localStorage costs the preference, not the app.
    }
  };

  const reorder = (next: InstrumentId[]) => {
    setOrder(next);
    try {
      localStorage.setItem(ORDER_KEY, JSON.stringify(next));
    } catch {
      // A full localStorage costs the arrangement, not the app.
    }
  };

  if (customising) {
    // A plain list of switches, in the rail's own order so the two views agree.
    // Shown *instead of* the instruments rather than beside them: the rail is
    // 216px wide and there is no room for both.
    return (
      <div className="flex flex-col gap-[2px] p-2">
        <span className="micro pb-1">SHOW</span>
        {order.map((id) => (
          <button
            key={id}
            type="button"
            onClick={() => toggle(id)}
            aria-pressed={enabled.has(id)}
            className="flex items-center gap-2 px-1 py-[4px] text-left"
          >
            {/* A filled square reads as on at 9px where a tick does not. */}
            <span
              aria-hidden="true"
              style={{
                width: 9,
                height: 9,
                flexShrink: 0,
                border: "1px solid var(--border-strong)",
                background: enabled.has(id) ? "rgb(var(--primary))" : "transparent",
              }}
            />
            <span
              className="micro truncate"
              style={{ color: enabled.has(id) ? "var(--text)" : "var(--text-faint)" }}
            >
              {LABELS[id]}
            </span>
          </button>
        ))}
        <span className="micro pt-2" style={{ lineHeight: 1.5 }}>
          SWITCHED OFF MEANS NOT RUNNING — NO POLLING, NO MEMORY
        </span>
      </div>
    );
  }

  return (
    <Reorder.Group axis="y" values={order} onReorder={reorder} className="flex flex-col">
      {order.filter((id) => enabled.has(id)).map((id) => (
        <Instrument key={id} id={id} session={session} host={host} />
      ))}
    </Reorder.Group>
  );
}

/** Names for the switch list. The instruments carry their own headers. */
const LABELS: Record<InstrumentId, string> = {
  clock: "CLOCK",
  host: "HOST",
  processes: "TOP PROCESSES",
  uplink: "UPLINK",
  usage: "TOKEN SPEND",
  jira: "JIRA",
  sent: "SENT",
  note: "NOTE",
  session: "SESSION",
};

export function WidgetRail({ session }: { session: Active | null }) {
  // Collapse lives in the workspace store, so the footer's mirror of the servers
  // toggle can drive it too — the rail is on the right, the toggle at bottom-right.
  const collapsed = useWorkspace((s) => s.widgetsCollapsed);
  // Not persisted: this is a mode you are *in*, not a preference. Reopening the
  // app into a settings list rather than the instruments would be a surprise.
  const [customising, setCustomising] = useState(false);

  const { width: railWidth, startResize } = useRailWidth("rmux.widgets.width", RAIL_DEFAULT);

  return (
    <motion.aside
      className="panel relative flex shrink-0 flex-col overflow-hidden"
      // Collapsed goes to width 0 — the rail leaves the layout entirely rather
      // than parking a strip on the right edge. The footer toggle is the way
      // back, so a collapsed remnant would only be a second, redundant control.
      animate={{ width: collapsed ? RAIL_COLLAPSED : railWidth }}
      transition={{ type: "spring", stiffness: 320, damping: 34 }}
      style={{ width: collapsed ? RAIL_COLLAPSED : railWidth }}
    >
      {/* This rail sits on the right, so its grip is on its *left* edge and the
          drag direction is inverted — see `useRailWidth`. */}
      {!collapsed && <RailGrip side="left" onPointerDown={(e) => startResize(e, "right")} />}
      {!collapsed && (
        <>
          <header
            className="flex shrink-0 items-center justify-between border-b px-2 py-2"
            style={{ borderColor: "var(--border)" }}
          >
            <button
              type="button"
              className="chip"
              aria-pressed={customising}
              onClick={() => setCustomising((c) => !c)}
              title={
                customising
                  ? "Back to the instruments"
                  : "Choose which instruments run. A widget switched off is unmounted, not hidden — it stops using memory."
              }
            >
              {/* Sliders, because the label alone was the problem. Deliberately
                  coarse: at 10px anything finer than three strokes is mush, and a
                  mark nobody can resolve is decoration. Square caps, 2px, per the
                  icon rule. */}
              <svg
                width="10"
                height="10"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="square"
                aria-hidden="true"
              >
                <path d="M3 7h18M3 17h18" />
                <path d="M9 4v6M16 14v6" />
              </svg>
              {customising ? "DONE" : "INSTRUMENTS"}
            </button>
            {/* Collapse is driven from the footer's mirror of the servers toggle
                (`Workbench` footer, bottom-right), not from an in-rail chip — one
                consistent affordance per bar. */}
          </header>

          <div className="flex min-h-0 flex-1 flex-col overflow-y-auto p-2">
            {session ? (
              <Instruments session={session} customising={customising} />
            ) : (
              <span className="micro">no session selected</span>
            )}
          </div>
        </>
      )}
    </motion.aside>
  );
}
