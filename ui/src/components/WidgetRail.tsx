import { useEffect, useState } from "react";
import { motion, Reorder, useDragControls } from "motion/react";

import { HostStatus } from "./widgets/HostStatus";
import { TopProcesses } from "./widgets/TopProcesses";
import { TokenSpend } from "./widgets/TokenSpend";
import { Clock } from "./widgets/Clock";
import { Note } from "./widgets/Note";
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
import { basename, useSessions, type Session } from "../lib/sessions";

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

const RAIL_WIDTH = 244;
const RAIL_COLLAPSED = 40;
/** Samples kept for the history chart. Its x-axis is "the last 30", so an
 *  unbounded buffer would quietly change what the chart means. */
const HISTORY = 30;

const COLLAPSE_KEY = "rmux.widgetRail.collapsed";


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
function useHostSample(target: TargetRef) {
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

    void tick();
    const timer = setInterval(tick, 2000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [target]);

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
        <span className="micro">reading…</span>
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
  session: Session;
  onDragStart?: (event: React.PointerEvent) => void;
}) {
  const [usage, setUsage] = useState<TokenUsage | null>(null);
  const [status, setStatus] = useState<ClaudeStatus | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;

    const tick = async () => {
      try {
        // A small tail: the widget needs recent turns, not the whole history,
        // and this runs on a timer against a file that can be hundreds of MB.
        const t = await api.claudeTranscript(session.target, session.folder, session.resume, 256 * 1024);
        if (cancelled) return;
        setUsage(t.usage);
        setStatus(t.status);
      } catch {
        // Not worth reporting in a widget — the Claude tab already shows why.
      }
    };

    void tick();
    const timer = setInterval(tick, 15_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [session.target, session.folder, session.resume]);

  return (
    <Widget title="TOKEN SPEND" onDragStart={onDragStart}>
      <div className="flex flex-col gap-2">
        <TokenSpend input={usage?.input ?? 0} output={usage?.output ?? 0} />

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
  session: Session;
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
const INSTRUMENTS = ["clock", "host", "processes", "uplink", "usage", "jira", "note", "session"] as const;
type InstrumentId = (typeof INSTRUMENTS)[number];

const ORDER_KEY = "rmux.widgetRail.order";

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
  session: Session;
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
            <JiraProgress project={session.jiraProject} />
          </Widget>
        ) : null;
      case "uplink":
        return (
          <Widget title="UPLINK" onDragStart={start}>
            <Uplink target={session.target} />
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
function Instruments({ session }: { session: Session }) {
  const [order, setOrder] = useState<InstrumentId[]>(readOrder);
  // Polled once here and shared, so HOST and TOP PROCESSES can be ordered
  // independently without doubling the ssh traffic behind them.
  const host = useHostSample(session.target);

  const reorder = (next: InstrumentId[]) => {
    setOrder(next);
    try {
      localStorage.setItem(ORDER_KEY, JSON.stringify(next));
    } catch {
      // A full localStorage costs the arrangement, not the app.
    }
  };

  return (
    <Reorder.Group axis="y" values={order} onReorder={reorder} className="flex flex-col">
      {order.map((id) => (
        <Instrument key={id} id={id} session={session} host={host} />
      ))}
    </Reorder.Group>
  );
}

export function WidgetRail({ session }: { session: Session | null }) {
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem(COLLAPSE_KEY) === "1",
  );
  const waiting = useSessions((s) => s.sessions.filter((x) => x.status === "waiting").length);

  useEffect(() => {
    localStorage.setItem(COLLAPSE_KEY, collapsed ? "1" : "0");
  }, [collapsed]);

  return (
    <motion.aside
      className="panel flex shrink-0 flex-col overflow-hidden"
      animate={{ width: collapsed ? RAIL_COLLAPSED : RAIL_WIDTH }}
      transition={{ type: "spring", stiffness: 320, damping: 34 }}
      style={{ width: collapsed ? RAIL_COLLAPSED : RAIL_WIDTH }}
    >
      <header
        className="flex shrink-0 items-center justify-between border-b px-2 py-2"
        style={{ borderColor: "var(--border)" }}
      >
        {!collapsed && <span className="micro">INSTRUMENTS</span>}
        <button
          type="button"
          className="micro"
          onClick={() => setCollapsed((c) => !c)}
          title={collapsed ? "Expand instruments" : "Collapse instruments"}
          aria-label={collapsed ? "Expand instruments" : "Collapse instruments"}
          style={{ marginLeft: collapsed ? "auto" : 0, marginRight: collapsed ? "auto" : 0 }}
        >
          {collapsed ? "«" : "»"}
        </button>
      </header>

      {collapsed ? (
        // Collapsed still has to carry signal, or collapsing it is free and
        // therefore permanent. The count of sessions wanting attention is the
        // one number worth keeping.
        <div className="flex flex-1 items-start justify-center pt-3">
          {waiting > 0 && (
            <span className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
              {waiting}
            </span>
          )}
        </div>
      ) : (
        <div className="flex min-h-0 flex-1 flex-col overflow-y-auto p-2">
          {session ? (
            <Instruments session={session} />
          ) : (
            <span className="micro">no session selected</span>
          )}
        </div>
      )}
    </motion.aside>
  );
}
