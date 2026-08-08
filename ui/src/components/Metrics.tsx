import { useEffect, useState } from "react";

import { api, isTauri, type MetricsSample, type TargetRef } from "../lib/api";

/** Bytes as a compact human figure, e.g. "29.6G". */
function humanBytes(bytes: number): string {
  const units = ["B", "K", "M", "G", "T"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 || unit === 0 ? 0 : 1)}${units[unit]}`;
}

/**
 * Live CPU and memory for the current target, in the status bar.
 *
 * Sampled every two seconds. The bar animates; the printed figure never does —
 * a number that eases toward its target is showing a value that was never
 * measured. Amber means load, not alarm: red is reserved for things the operator
 * must act on, and a busy server is usually just a server doing its job.
 */
export function Metrics({ target }: { target: TargetRef }) {
  const [sample, setSample] = useState<MetricsSample | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;

    let cancelled = false;
    setSample(null);
    setFailed(false);

    const tick = async () => {
      try {
        const next = await api.metricsSample(target);
        if (!cancelled) {
          setSample(next);
          setFailed(false);
        }
      } catch {
        // A host that briefly stops answering should not blank the reading it
        // last gave; just mark it stale.
        if (!cancelled) setFailed(true);
      }
    };

    void tick();
    const id = setInterval(tick, 2000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [target]);

  // The footer keeps its shape while the first sample is taken. A bar that
  // appears from nothing several seconds in reads as the app finishing its
  // start-up long after it has started.
  if (!sample) {
    return (
      <span className="micro" style={{ color: "var(--text-faint)" }}>
        SAMPLING…
      </span>
    );
  }

  const memPercent = sample.memoryTotalBytes
    ? (sample.memoryUsedBytes / sample.memoryTotalBytes) * 100
    : 0;

  return (
    <div className="flex items-center gap-3" style={{ opacity: failed ? 0.45 : 1 }}>
      {sample.cpuPercent !== null && (
        <span className="micro data" title="CPU">
          CPU {sample.cpuPercent.toFixed(0).padStart(2, " ")}%
        </span>
      )}
      <span className="micro data" title="Memory used / total">
        MEM {humanBytes(sample.memoryUsedBytes)}/{humanBytes(sample.memoryTotalBytes)}
      </span>
      <span className="micro data" title="Load average (1 min)">
        LOAD {sample.loadAverage.toFixed(2)}
      </span>
      {/* A meter for memory, since that is the figure with a hard ceiling. */}
      <div className="h-[8px] w-[52px]" style={{ background: "color-mix(in srgb, var(--text) 9%, transparent)" }}>
        <div
          className="h-full"
          style={{
            width: `${Math.min(100, memPercent)}%`,
            background: memPercent > 85 ? "rgb(var(--busy))" : "var(--text-soft)",
          }}
        />
      </div>
    </div>
  );
}
