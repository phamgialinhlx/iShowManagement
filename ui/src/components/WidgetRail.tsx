import { useEffect, useState } from "react";
import { motion } from "motion/react";

import {
  api,
  isTauri,
  type ClaudeStatus,
  type MetricsSample,
  type TokenUsage,
  type TargetRef,
} from "../lib/api";
import { compactTokens, contextLimit } from "../lib/context-window";
import { ClaudeAccountWidget } from "./ClaudeAccount";
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
const COLLAPSE_KEY = "rmux.widgetRail.collapsed";

const humanBytes = (bytes: number) => {
  const units = ["B", "K", "M", "G", "T"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 || unit === 0 ? 0 : 1)}${units[unit]}`;
};

const compact = (n: number) => {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 100_000 ? 0 : 1)}k`;
  return String(n);
};

/** A panel with a corner-bracketed header — the recurring instrument frame. */
function Widget({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="inset" style={{ border: "1px solid var(--border)" }}>
      <header
        className="flex items-center justify-between border-b px-2 py-[5px]"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="micro">{title}</span>
        {/* A tick mark, not an icon: it reads as instrument chrome. */}
        <span aria-hidden="true" style={{ color: "var(--text-faint)", fontSize: 9 }}>
          ┐
        </span>
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

/** A segmented meter. The bar animates; the number never does. */
function Meter({ percent, tone }: { percent: number; tone: string }) {
  const clamped = Math.max(0, Math.min(100, percent));
  return (
    <div style={{ height: 4, background: "rgba(232,230,225,0.10)" }}>
      <motion.div
        style={{ height: "100%", background: tone, transformOrigin: "left" }}
        initial={false}
        animate={{ scaleX: clamped / 100 }}
        transition={{ duration: 0.3, ease: [0.2, 0.9, 0.3, 1] }}
      />
    </div>
  );
}

/** Live CPU and memory for the session's host. */
function HostWidget({ target }: { target: TargetRef }) {
  const [sample, setSample] = useState<MetricsSample | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;

    const tick = async () => {
      try {
        const next = await api.metricsSample(target);
        if (!cancelled) {
          setSample(next);
          setFailed(false);
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

  if (failed && !sample) {
    return (
      <Widget title="HOST">
        <span className="micro">unreachable</span>
      </Widget>
    );
  }

  const memPercent = sample?.memoryTotalBytes
    ? (sample.memoryUsedBytes / sample.memoryTotalBytes) * 100
    : 0;

  return (
    <Widget title="HOST">
      <div className="flex flex-col gap-2">
        <div>
          <Row
            label="CPU"
            // `null` until a second sample exists to difference against — a
            // cumulative counter cannot describe "now" on its own, and showing 0
            // would be a measurement nobody took.
            value={sample?.cpuPercent == null ? "—" : `${sample.cpuPercent.toFixed(0)}%`}
          />
          <Meter percent={sample?.cpuPercent ?? 0} tone="rgb(var(--busy))" />
        </div>

        <div>
          <Row
            label="MEM"
            value={
              sample
                ? `${humanBytes(sample.memoryUsedBytes)}/${humanBytes(sample.memoryTotalBytes)}`
                : "—"
            }
          />
          <Meter percent={memPercent} tone="var(--text-soft)" />
        </div>

        <Row label="LOAD" value={sample ? sample.loadAverage.toFixed(2) : "—"} />
      </div>
    </Widget>
  );
}

/** What this conversation has cost. */
function UsageWidget({ session }: { session: Session }) {
  const [usage, setUsage] = useState<TokenUsage | null>(null);
  const [perTurn, setPerTurn] = useState<number[]>([]);
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
        setPerTurn(t.perTurn);
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

  const bars = perTurn.slice(-32);
  const peak = Math.max(1, ...bars);

  return (
    <Widget title="CLAUDE USAGE">
      <div className="flex flex-col gap-2">
        {/* Output tokens per turn. Chalk, not red: spend is information, not an
            alarm. */}
        <div className="flex h-[34px] items-end gap-[2px]">
          {bars.length === 0 && <span className="micro">no turns recorded</span>}
          {bars.map((value, i) => (
            <div
              key={i}
              title={`${value.toLocaleString()} output tokens`}
              style={{
                flex: 1,
                minWidth: 2,
                height: `${Math.max(6, (value / peak) * 100)}%`,
                background: "var(--text-soft)",
              }}
            />
          ))}
        </div>

        <ContextRow status={status} />

        <Row label="OUT" value={usage ? compact(usage.output) : "—"} />
        <Row label="IN" value={usage ? compact(usage.input) : "—"} />
        {/* Cache reads dwarf everything else and are nearly free; showing them
            beside the billed figures is what makes the billed ones legible. */}
        <Row label="CACHED" value={usage ? compact(usage.cacheRead) : "—"} />
        <Row label="TURNS" value={usage ? String(usage.turns) : "—"} />
      </div>
    </Widget>
  );
}

/**
 * How full the context window is.
 *
 * The meter appears only when the window size is actually known — see
 * `contextLimit`. Otherwise the token count stands alone, because a bar drawn
 * against a guessed denominator is a confident lie about how much room is left.
 */
function ContextRow({ status }: { status: ClaudeStatus | null }) {
  const context = status?.contextTokens ?? 0;
  if (!context) return <Row label="CONTEXT" value="—" />;

  const limit = contextLimit(status?.model, context);
  const percent = limit ? (context / limit) * 100 : null;

  return (
    <div>
      <Row
        label="CONTEXT"
        value={percent === null ? compactTokens(context) : `${Math.round(percent)}%`}
      />
      {percent !== null && (
        // Amber past three quarters: worth noticing before a compaction is
        // forced, but not something to act on — so never red.
        <Meter percent={percent} tone={percent >= 75 ? "rgb(var(--busy))" : "var(--text-soft)"} />
      )}
    </div>
  );
}

/** Where you are, and for how long. */
function SessionWidget({ session }: { session: Session }) {
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
    <Widget title="SESSION">
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
        <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto p-2">
          {session ? (
            <>
              <HostWidget target={session.target} />
              <UsageWidget session={session} />
              <SessionWidget session={session} />
              <Widget title="CLAUDE ACCOUNT">
                <ClaudeAccountWidget />
              </Widget>
            </>
          ) : (
            <span className="micro">no session selected</span>
          )}
        </div>
      )}
    </motion.aside>
  );
}
