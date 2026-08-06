import { useEffect, useRef, useState } from "react";

import { api, isTauri, type ProcessInfo, type TargetRef } from "../../lib/api";
import { accent, paint, textRamp, tokenAlpha } from "../../lib/palette";

/**
 * What is actually using the machine.
 *
 * A port of the cowork reactor: a segmented ring of the five heaviest
 * processes, each called out with a leader line, and the **whole-host** figure
 * in the middle. That distinction matters — the centre is not the sum of the
 * segments. The segments are five processes; the centre is the machine.
 *
 * Canvas rather than SVG because the arcs and the printed values ease toward
 * each new poll every frame; the ring re-sorting itself in a single step on
 * each refresh is what makes a chart like this unreadable.
 */

/** Gap between segments, in radians. */
const GAP = 0.07;
const POLL_MS = 3500;

type SortBy = "cpu" | "memory";

/** Distance kept between any label and the canvas edge. */
const PAD = 8;

/**
 * Shorten `text` until it fits `room`, with an ellipsis when it had to.
 *
 * Canvas has no overflow: text drawn past the edge is simply gone, with no
 * clue that anything was lost. Truncating on a measured width rather than a
 * character count is what makes the same widget work at any rail width.
 */
function fit(g: CanvasRenderingContext2D, text: string, room: number): string {
  if (room <= 0) return "";
  if (g.measureText(text).width <= room) return text;

  let out = text;
  while (out.length > 1 && g.measureText(`${out}…`).width > room) {
    out = out.slice(0, -1);
  }
  return `${out}…`;
}

function Donut({
  rows,
  by,
  overall,
  coreCount,
}: {
  rows: ProcessInfo[];
  by: SortBy;
  overall: number;
  coreCount: number;
}) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const state = useRef<{ items: { name: string; v: number; s: number }[] }>({ items: [] });
  const rotation = useRef(0);

  // Read by the animation loop, which outlives any single render.
  const data = useRef(rows);
  const metric = useRef(by);
  const total = useRef(overall);
  const cores = useRef(coreCount);
  data.current = rows;
  metric.current = by;
  total.current = overall;
  cores.current = coreCount;

  useEffect(() => {
    const el = canvas.current;
    if (!el) return;

    let raf = 0;
    let last = 0;

    const draw = (t: number) => {
      raf = requestAnimationFrame(draw);
      if (document.hidden || t - last < 33) return;
      last = t;

      const box = wrap.current;
      if (!box) return;
      const w = box.clientWidth * 2;
      const h = box.clientHeight * 2;
      if (w === 0 || h === 0) return;
      if (el.width !== w || el.height !== h) {
        el.width = w;
        el.height = h;
      }
      const g = el.getContext("2d");
      if (!g) return;

      const st = state.current;
      const live = data.current.map((p) => ({
        name: p.name,
        // `ps` reports %CPU per core, so a single busy process on a 16-core box
        // reads 1600%. Divided by the core count it becomes a share of the
        // machine, which is what the ring is drawing.
        v:
          metric.current === "cpu"
            ? Math.min(100, p.cpuPercent / Math.max(1, cores.current))
            : p.memoryPercent,
      }));

      // Match by name so a process keeps its animated sweep across polls;
      // rebuilding from scratch would make every refresh snap.
      st.items = live.map((row) => {
        const previous = st.items.find((i) => i.name === row.name);
        return previous
          ? { name: row.name, v: previous.v + (row.v - previous.v) * 0.08, s: previous.s }
          : { name: row.name, v: row.v, s: 0 };
      });

      const sum = Math.max(1, st.items.reduce((a, i) => a + i.v, 0));
      st.items.forEach((i) => {
        const target = (i.v / sum) * (Math.PI * 2 - GAP * st.items.length);
        i.s += (target - i.s) * 0.08;
      });
      rotation.current += 0.0015;

      const cx = w / 2;
      const cy = h / 2 + 4;
      const R = Math.min(h * 0.34, w * 0.2);

      g.clearRect(0, 0, w, h);

      // Resolved once per frame, so the ring follows the active theme. Chalk
      // fading toward the background, brightest first.
      const ramp = textRamp(5);

      // A slowly turning dotted ring. This is the widget's liveness cue —
      // motion, not a blinking number.
      g.fillStyle = tokenAlpha("--text", 0.3);
      for (let d = 0; d < 60; d += 1) {
        const da = (d / 60) * Math.PI * 2 + rotation.current;
        g.fillRect(cx + Math.cos(da) * R * 0.72 - 1, cy + Math.sin(da) * R * 0.72 - 1, 2, 2);
      }

      g.textAlign = "center";
      g.textBaseline = "middle";
      g.fillStyle = total.current >= 90 ? accent() : paint("--text");
      g.font = '700 28px "SFU Futura", "IBM Plex Mono", monospace';
      g.fillText(`${Math.round(total.current)}%`, cx, cy - 3);
      g.fillStyle = paint("--text-faint");
      g.font = '600 9px "IBM Plex Mono", monospace';
      g.fillText(metric.current === "cpu" ? "CPU LOAD" : "RAM USED", cx, cy + 16);
      g.textBaseline = "alphabetic";

      let a = -Math.PI / 2;
      const sides: { l: { i: number; mid: number }[]; r: { i: number; mid: number }[] } = {
        l: [],
        r: [],
      };

      st.items.forEach((item, i) => {
        g.strokeStyle = ramp[i] ?? ramp[ramp.length - 1] ?? paint("--text-faint");
        g.lineWidth = 6;
        g.lineCap = "butt";
        g.beginPath();
        g.arc(cx, cy, R, a, a + item.s);
        g.stroke();

        const mid = a + item.s / 2;
        sides[Math.cos(mid) >= 0 ? "r" : "l"].push({ i, mid });
        a += item.s + GAP;
      });

      (["l", "r"] as const).forEach((side) => {
        const entries = sides[side].sort((x, y) => Math.sin(x.mid) - Math.sin(y.mid));
        entries.forEach((entry, k) => {
          const dir = side === "r" ? 1 : -1;
          const item = st.items[entry.i];
          if (!item) return;

          const px = cx + Math.cos(entry.mid) * (R + 5);
          const py = cy + Math.sin(entry.mid) * (R + 5);
          // Stacked evenly about the centre so callouts never overlap, however
          // the segments happen to fall.
          const ly = cy + (k - (entries.length - 1) / 2) * 52;
          // Clamped inside the canvas. The original geometry assumed a 264px
          // floating window; in a 240px rail `R + 40` put the text off the left
          // edge, which is why names read as "ntainerd".
          const lx = Math.max(PAD, Math.min(w - PAD, cx + dir * (R + 40)));
          // Whatever room is actually left on this side, after the leader line.
          const room = dir > 0 ? w - lx - PAD : lx - PAD;

          g.strokeStyle = tokenAlpha("--text", 0.25);
          g.lineWidth = 1;
          g.beginPath();
          g.moveTo(px, py);
          g.lineTo(lx - dir * 12, ly);
          g.lineTo(lx, ly);
          g.stroke();

          g.textAlign = dir > 0 ? "left" : "right";
          g.fillStyle = paint("--text-soft");
          g.font = '16px "IBM Plex Mono", monospace';
          // Measured rather than guessed at a character count: a name only
          // reads as a name if it is not cut off mid-word by the canvas edge.
          g.fillText(fit(g, item.name, room - 6), lx + dir * 6, ly - 4);
          g.fillStyle = ramp[entry.i] ?? ramp[ramp.length - 1] ?? paint("--text-faint");
          g.font = '700 20px "SFU Futura", "IBM Plex Mono", monospace';
          g.fillText(`${Math.round(item.v)}%`, lx + dir * 6, ly + 16);
        });
      });
    };

    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <div ref={wrap} style={{ flex: 1, minHeight: 150, position: "relative" }}>
      <canvas ref={canvas} style={{ position: "absolute", inset: 0, width: "100%", height: "100%" }} />
    </div>
  );
}

export function TopProcesses({
  target,
  hostname,
  cpuPercent,
  memoryPercent,
  cores,
}: {
  target: TargetRef;
  hostname: string;
  /** Whole-host figures, for the centre readout. */
  cpuPercent: number | null;
  memoryPercent: number;
  cores: number;
}) {
  const [rows, setRows] = useState<ProcessInfo[]>([]);
  const [by, setBy] = useState<SortBy>("cpu");
  const [error, setError] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;
    let alive = true;

    const load = () => {
      api
        .metricsProcesses(target, by)
        .then((next) => {
          if (!alive) return;
          setRows(next);
          setError(false);
        })
        .catch(() => alive && setError(true));
    };

    load();
    const timer = setInterval(load, POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [target, by]);

  const tab = (kind: SortBy) => ({
    border: "none",
    cursor: "pointer",
    padding: "1px 7px",
    fontSize: 9.5,
    fontFamily: "var(--font-mono)",
    letterSpacing: "0.06em",
    textTransform: "uppercase" as const,
    background: by === kind ? "color-mix(in srgb, var(--text) 26%, transparent)" : "transparent",
    color: by === kind ? "var(--text)" : "var(--text-soft)",
  });

  return (
    <div style={{ display: "flex", flexDirection: "column", minHeight: 0, flex: 1 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 7, marginBottom: 6 }}>
        <span
          className="data"
          style={{
            fontSize: 9.5,
            letterSpacing: "0.12em",
            textTransform: "uppercase",
            color: "var(--text-soft)",
          }}
        >
          Reactor
        </span>
        <span
          className="data"
          style={{
            fontSize: 9.5,
            minWidth: 0,
            flex: 1,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            color: "var(--text-faint)",
          }}
        >
          {hostname || "no host"}
        </span>
        <div
          style={{
            display: "inline-flex",
            gap: 2,
            border: "1px solid var(--border)",
            padding: 1,
          }}
        >
          <button type="button" style={tab("cpu")} onClick={() => setBy("cpu")}>
            cpu
          </button>
          <button type="button" style={tab("memory")} onClick={() => setBy("memory")}>
            mem
          </button>
        </div>
      </div>

      {rows.length === 0 ? (
        <div
          className="data"
          style={{ fontSize: 11, margin: "auto", color: "var(--text-faint)" }}
        >
          {error ? "unreachable" : "reading…"}
        </div>
      ) : (
        <Donut
          rows={rows}
          by={by}
          overall={by === "cpu" ? (cpuPercent ?? 0) : memoryPercent}
          coreCount={cores}
        />
      )}
    </div>
  );
}
