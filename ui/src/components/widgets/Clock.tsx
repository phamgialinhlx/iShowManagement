import { useEffect, useState } from "react";

/**
 * Mission time.
 *
 * A port of the cowork clock. Sized with `cqw` against the rail's own width so
 * it stays as large as it can be without wrapping — the rail is collapsible and
 * a fixed size would either overflow it or waste it.
 *
 * `tabular-nums` matters more than it looks: without it the digits have
 * different widths, so a clock ticking once a second visibly jitters as the
 * glyphs change. The blinking block is a cursor, which is the one thing the
 * design system allows to blink.
 */
export function Clock() {
  const [now, setNow] = useState(() => new Date());

  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(timer);
  }, []);

  return (
    <div style={{ containerType: "inline-size" }}>
      <div
        style={{
          fontFamily: "var(--font-display)",
          fontWeight: 700,
          textTransform: "uppercase",
          fontSize: "clamp(24px, 20cqw, 38px)",
          lineHeight: 1,
          fontVariantNumeric: "tabular-nums",
          letterSpacing: "0.02em",
          whiteSpace: "nowrap",
          color: "var(--text)",
        }}
      >
        {now.toLocaleTimeString(undefined, { hour12: false })}
        <span className="blink">▮</span>
      </div>
      <div className="data" style={{ fontSize: 11, marginTop: 6, color: "var(--text-soft)" }}>
        {now.toLocaleDateString(undefined, {
          weekday: "short",
          year: "numeric",
          month: "short",
          day: "numeric",
        })}{" "}
        · {Intl.DateTimeFormat().resolvedOptions().timeZone}
      </div>
    </div>
  );
}
