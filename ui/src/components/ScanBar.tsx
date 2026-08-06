/**
 * A segmented meter — fine ticks, lit proportionally to `value`.
 *
 * Segmented rather than continuous on purpose: at a glance you read the *count*
 * of lit ticks, which is easier to compare across stacked meters than the length
 * of a solid bar. The printed number beside it stays authoritative.
 *
 * The segmentation is a CSS mask of fixed-width stripes rather than N flex
 * children. That distinction matters: with flex children the tick width scales
 * with the container, so the same meter renders as fine ticks in a narrow rail
 * and as chunky slabs in a wide panel. A tick is a unit of reading, so it must
 * stay the same size wherever the meter is placed.
 */
export function ScanBar({
  value,
  max = 100,
  /** Amber for load, chalk for everything else. Red is reserved — see rule 0. */
  tone = "chalk",
  breathe = false,
  segmentWidth = 5,
  segmentGap = 2,
}: {
  value: number;
  max?: number;
  tone?: "chalk" | "busy";
  breathe?: boolean;
  segmentWidth?: number;
  segmentGap?: number;
}) {
  const ratio = max > 0 ? Math.min(1, Math.max(0, value / max)) : 0;
  const color = tone === "busy" ? "rgb(var(--busy))" : "var(--text)";

  const stripes = `repeating-linear-gradient(90deg, #000 0 ${segmentWidth}px, transparent ${segmentWidth}px ${segmentWidth + segmentGap}px)`;

  return (
    <div
      className={`relative h-[10px] w-full ${breathe ? "breathe" : ""}`}
      style={{ maskImage: stripes, WebkitMaskImage: stripes }}
      role="meter"
      aria-valuenow={value}
      aria-valuemin={0}
      aria-valuemax={max}
    >
      <div className="absolute inset-0" style={{ background: "color-mix(in srgb, var(--text) 9%, transparent)" }} />
      {/*
        No transition on the fill: a meter that eases between readings is
        displaying a value that was never measured.
      */}
      <div
        className="absolute inset-y-0 left-0"
        style={{ width: `${ratio * 100}%`, background: color }}
      />
    </div>
  );
}
