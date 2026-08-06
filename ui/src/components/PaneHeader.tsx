import type { ReactNode } from "react";

/**
 * The bar that names a pane.
 *
 * Claude panes have carried one since they gained a status strip; shells, host
 * panels and file browsers had none, so in a grid half the tiles announced
 * themselves and half did not — and a terminal is exactly the pane whose
 * contents tell you least about which machine and which session you are looking
 * at. Reported as "why does the top bar of the pane disappear when we are in
 * file browser or terminal?", which is the right question: it read as something
 * vanishing rather than something never drawn.
 *
 * It matches `ClaudePanel`'s own header deliberately — same height, same
 * padding, same border — because two headers that nearly agree look like a
 * misalignment on every screen that shows both at once.
 *
 * **Truncated, never wrapped.** A second line would silently cost the terminal
 * below it a row: `FitAddon` measures what is left after this bar, so a header
 * whose height depends on the length of a session name makes the grid depend on
 * it too.
 */
export function PaneHeader({
  label,
  detail,
  actions,
}: {
  label: string;
  /** Where it is — the host, the folder. Dropped first when space runs out. */
  detail?: string;
  actions?: ReactNode;
}) {
  return (
    <header
      className="flex shrink-0 items-center gap-3 border-b px-3 py-1"
      style={{ borderColor: "var(--border)" }}
    >
      <span
        className="micro truncate"
        style={{ color: "var(--text)", maxWidth: "44%", flex: "0 1 auto" }}
        title={label}
      >
        {label.toUpperCase()}
      </span>

      {detail && (
        <span className="micro min-w-0 truncate" style={{ color: "var(--text-faint)" }} title={detail}>
          {detail}
        </span>
      )}

      {actions && <div className="ml-auto flex items-center gap-1">{actions}</div>}
    </header>
  );
}
