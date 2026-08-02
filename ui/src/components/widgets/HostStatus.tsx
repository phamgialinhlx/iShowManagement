import { useEffect, useRef } from "react";

/**
 * The host: what it is called, how long it has been up, and what it is doing.
 *
 * A port of the cowork status card. The chart is a canvas rather than SVG
 * because it redraws every frame to ease between polls — telemetry lands every
 * couple of seconds, and plotting it raw makes the trace jump a step at a time.
 * Each frame the drawn series moves a fraction of the way toward the polled one.
 *
 * **The easing never invents a reading.** It only smooths the path between two
 * real ones, and every printed number comes straight from the sample. That is
 * the line the design system draws: meters may breathe, printed values may not.
 */

const RED = "#e63b2e";
const CHALK = "#e8e6e1";
/** Axis labels. rmux's own faint token, not the old app's #5c5953 — that
 *  measured 2.77:1 and these are 11px. */
const AXIS = "#7e7b74";

export type HostSample = {
  cpuPercent: number | null;
  memoryUsedBytes: number;
  memoryTotalBytes: number;
  hostname: string;
  uptimeSeconds: number;
  netRxBps: number | null;
  netTxBps: number | null;
};

/** "46d 5h" — the two largest non-zero units. */
export function formatUptime(seconds: number): string {
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

const gb = (bytes: number) => (bytes / 1e9).toFixed(1);

/**
 * Compact, so the rail never truncates a real number — but **with its unit**.
 *
 * `489K` says nothing on its own: kilobits, kilobytes, packets? The counters
 * behind it are bytes per second, and a network figure without a unit is the
 * one number in this widget people actively misread, because the habit from
 * every speed test is bits.
 */
export function formatRate(bps: number | null | undefined): string {
  const v = bps ?? 0;
  if (v >= 1e6) return `${(v / 1e6).toFixed(1)} MB/s`;
  if (v >= 1e3) return `${Math.round(v / 1e3)} KB/s`;
  return `${Math.round(v)} B/s`;
}

function Bar({ percent, color }: { percent: number; color: string }) {
  return (
    <div style={{ height: 6, background: "var(--border)", overflow: "hidden" }}>
      <div
        style={{
          height: "100%",
          width: `${Math.max(0, Math.min(100, percent))}%`,
          background: color,
          transition: "width 0.7s cubic-bezier(.4,0,.2,1)",
        }}
      />
    </div>
  );
}

function Row({ label, bar, value }: { label: string; bar: React.ReactNode; value: string }) {
  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "30px minmax(40px, 1fr) auto",
        gap: 8,
        alignItems: "center",
        marginTop: 8,
      }}
    >
      <span
        className="data"
        style={{
          fontSize: 9,
          letterSpacing: "0.14em",
          textTransform: "uppercase",
          color: "var(--text-soft)",
        }}
      >
        {label}
      </span>
      {bar}
      <span
        className="data"
        style={{
          fontSize: 10,
          color: "var(--text-soft)",
          textAlign: "right",
          whiteSpace: "nowrap",
        }}
      >
        {value}
      </span>
    </div>
  );
}

/** CPU as a filled area, RAM as a line over it. */
function History({ cpu, ram }: { cpu: number[]; ram: number[] }) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const target = useRef<{ cpu: number[]; ram: number[] }>({ cpu: [], ram: [] });
  const shown = useRef<{ cpu: number[]; ram: number[] }>({ cpu: [], ram: [] });

  target.current = { cpu, ram };
  if (shown.current.cpu.length === 0 && cpu.length > 0) {
    shown.current = { cpu: [...cpu], ram: [...ram] };
  }

  useEffect(() => {
    const el = canvas.current;
    if (!el) return;

    let raf = 0;
    let last = 0;

    const draw = (now: number) => {
      raf = requestAnimationFrame(draw);
      // ~30fps, and nothing at all behind another window. The design leans on
      // continuous motion; without this the compositor keeps paying for it.
      if (document.hidden || now - last < 33) return;
      last = now;

      const box = wrap.current;
      if (!box) return;

      // Backing store at 2× so the hairlines and 11px labels stay crisp.
      const W = Math.max(160, box.clientWidth) * 2;
      const H = 128;
      if (el.width !== W || el.height !== H) {
        el.width = W;
        el.height = H;
      }
      const g = el.getContext("2d");
      if (!g) return;

      const tg = target.current;
      const sh = shown.current;
      (["cpu", "ram"] as const).forEach((key) => {
        if (sh[key].length !== tg[key].length) sh[key] = [...tg[key]];
        for (let i = 0; i < tg[key].length; i += 1) {
          const to = tg[key][i];
          const from = sh[key][i];
          if (to == null || from == null) continue;
          sh[key][i] = from + (to - from) * 0.05;
        }
      });

      const PL = 44;
      const PR = 8;
      const PT = 8;
      const PB = 30;
      const PW = W - PL - PR;
      const PH = H - PT - PB;

      g.clearRect(0, 0, W, H);
      g.font = '11px "IBM Plex Mono", monospace';

      [0, 50, 100].forEach((v) => {
        const y = PT + (1 - v / 100) * PH;
        g.strokeStyle = "rgba(232,230,225,0.09)";
        g.lineWidth = 1;
        g.beginPath();
        g.moveTo(PL, y);
        g.lineTo(W - PR, y);
        g.stroke();
        g.fillStyle = AXIS;
        g.textAlign = "right";
        g.fillText(String(v), PL - 7, y + 4);
      });

      g.strokeStyle = "rgba(232,230,225,0.06)";
      [0.25, 0.5, 0.75].forEach((f) => {
        const x = PL + f * PW;
        g.beginPath();
        g.moveTo(x, PT);
        g.lineTo(x, PT + PH);
        g.stroke();
      });

      g.fillStyle = AXIS;
      g.textAlign = "center";
      ([["-30S", 0.25], ["-20S", 0.5], ["-10S", 0.75]] as [string, number][]).forEach(
        ([label, f]) => g.fillText(label, PL + f * PW, H - 10),
      );
      g.textAlign = "right";
      g.fillText("NOW", W - PR, H - 10);

      const path = (values: number[]) => {
        g.beginPath();
        const n = values.length;
        for (let i = 0; i < n; i += 1) {
          const x = PL + (i / Math.max(1, n - 1)) * PW;
          const y = PT + (1 - Math.max(0, Math.min(100, values[i]!)) / 100) * PH;
          if (i) g.lineTo(x, y);
          else g.moveTo(x, y);
        }
      };

      g.save();
      g.beginPath();
      g.rect(PL, PT, PW, PH);
      g.clip();

      // RAM first, so a busy CPU reads over it rather than under.
      if (sh.ram.length > 1) {
        path(sh.ram);
        g.strokeStyle = CHALK;
        g.lineWidth = 1.6;
        g.stroke();
      }
      if (sh.cpu.length > 1) {
        path(sh.cpu);
        g.lineTo(PL + PW, PT + PH);
        g.lineTo(PL, PT + PH);
        g.closePath();
        g.fillStyle = "rgba(230,59,46,0.12)";
        g.fill();
        path(sh.cpu);
        g.strokeStyle = RED;
        g.lineWidth = 1.8;
        g.stroke();
      }
      g.restore();
    };

    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <div ref={wrap} style={{ marginTop: 12 }}>
      <canvas ref={canvas} style={{ width: "100%", height: 64, display: "block" }} />
      <div style={{ display: "flex", gap: 16, marginTop: 5 }}>
        <Legend color={RED} label="CPU" />
        <Legend color={CHALK} label="RAM" />
      </div>
    </div>
  );
}

function Legend({ color, label }: { color: string; label: string }) {
  return (
    <span
      className="data"
      style={{
        fontSize: 8.5,
        letterSpacing: "0.14em",
        color: "var(--text-soft)",
        display: "flex",
        alignItems: "center",
        gap: 6,
      }}
    >
      <i style={{ width: 9, height: 2, background: color, display: "inline-block" }} />
      {label}
    </span>
  );
}

export function HostStatus({
  sample,
  cpuHistory,
  ramHistory,
  netPeak,
}: {
  sample: HostSample;
  cpuHistory: number[];
  ramHistory: number[];
  /** The busiest traffic seen so far, so the bar stays meaningful on a quiet host. */
  netPeak: number;
}) {
  const ramPercent =
    sample.memoryTotalBytes > 0 ? (sample.memoryUsedBytes / sample.memoryTotalBytes) * 100 : 0;
  const netNow = (sample.netRxBps ?? 0) + (sample.netTxBps ?? 0);
  const netPercent = netPeak > 0 ? (netNow / netPeak) * 100 : 0;

  return (
    <>
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
        <span
          style={{ width: 8, height: 8, background: "rgb(var(--primary))", flexShrink: 0 }}
        />
        <span
          style={{
            fontFamily: "var(--font-display)",
            fontWeight: 700,
            letterSpacing: "0.08em",
            textTransform: "uppercase",
            fontSize: 15,
            color: "var(--text)",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
          title={sample.hostname}
        >
          {sample.hostname || "—"}
        </span>
        <span style={{ flex: 1 }} />
        <span
          className="data"
          style={{ fontSize: 10, color: "var(--text-soft)", whiteSpace: "nowrap" }}
        >
          up {formatUptime(sample.uptimeSeconds)}
        </span>
      </div>

      <Row
        label="CPU"
        bar={<Bar percent={sample.cpuPercent ?? 0} color="var(--text)" />}
        // `null` until a second sample exists to difference against. Showing 0
        // would be a measurement nobody took.
        value={sample.cpuPercent == null ? "—" : `${Math.round(sample.cpuPercent)}%`}
      />
      <Row
        label="RAM"
        bar={<Bar percent={ramPercent} color="var(--text-soft)" />}
        value={`${gb(sample.memoryUsedBytes)}/${gb(sample.memoryTotalBytes)}G`}
      />
      <Row
        label="NET"
        bar={<Bar percent={netPercent} color="var(--text-soft)" />}
        value={
          sample.netRxBps == null
            ? "—"
            : `↓${formatRate(sample.netRxBps)} ↑${formatRate(sample.netTxBps)}`
        }
      />

      <History cpu={cpuHistory} ram={ramHistory} />
    </>
  );
}
