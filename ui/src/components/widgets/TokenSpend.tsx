/**
 * Token spend — the three figures that matter.
 *
 * Output carries the one accent because it is the billed half and the one that
 * moves; input stays soft, since two reds would mean neither.
 */

const RED = "#e63b2e";

/** 63.59M / 56.7k / 812 — two decimals at millions, one at thousands. */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k`;
  return String(n);
}

function Metric({ label, value, color }: { label: string; value: string; color?: string }) {
  return (
    <div>
      <div
        className="data"
        style={{
          fontSize: 9.5,
          letterSpacing: "0.1em",
          textTransform: "uppercase",
          color: "var(--text-faint)",
        }}
      >
        {label}
      </div>
      <div style={{ fontSize: 15, fontFamily: "var(--font-mono)", color: color ?? "var(--text)" }}>
        {value}
      </div>
    </div>
  );
}

export function TokenSpend({
  input,
  output,
}: {
  input: number;
  output: number;
}) {
  const total = input + output;

  if (total === 0) {
    return (
      <span className="data" style={{ fontSize: 11, color: "var(--text-faint)" }}>
        no usage recorded yet
      </span>
    );
  }

  return (
    <>
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(72px, 1fr))",
          gap: 12,
          marginBottom: 12,
        }}
      >
        <Metric label="Total" value={formatTokens(total)} />
        {/* Output is the billed half and the one that moves, so it carries the
            one accent. Input stays soft — two reds would mean neither. */}
        <Metric label="Output" value={formatTokens(output)} color={RED} />
        <Metric label="Input" value={formatTokens(input)} color="var(--text-soft)" />
      </div>

    </>
  );
}
