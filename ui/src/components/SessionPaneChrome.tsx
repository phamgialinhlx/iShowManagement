/**
 * Shared chrome for the agent session panes (Claude and pi).
 *
 * These are the pieces that make the two panes look identical: the inline-SVG
 * `Icon`, the soft-at-rest `IconButton` on the status line, the `BackView` with
 * its `←`-back bar, and the icon path constants. They live here — rather than in
 * `ClaudeSessionPane` with copies in `PiSessionPane` — so the two cannot drift
 * (a second copy of the menu drifted before review; same failure mode).
 */

/** Rule 3: inline SVG, Lucide-style, square caps — no glyph font, no emoji. */
export function Icon({ d, size = 14 }: { d: string; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="square"
      strokeLinejoin="miter"
      aria-hidden="true"
    >
      <path d={d} />
    </svg>
  );
}

/**
 * A header icon button — soft at rest, full on hover (rule: legible controls).
 *
 * `disabled` is here for the one control in this row that is *mutating* rather
 * than a view switch: RELOAD. The rule is that every mutating control reports its
 * own state beside itself, and for a button whose whole job is "do it again" the
 * useful report is that it is already doing it — which also stops a second press
 * landing while the first reattach is in flight.
 */
export function IconButton({
  label,
  onClick,
  disabled = false,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      aria-disabled={disabled}
      aria-busy={disabled}
      title={label}
      className={`flex shrink-0 items-center gap-1 px-1 ${
        disabled ? "cursor-default opacity-40" : "opacity-70 hover:opacity-100"
      }`}
      style={{ color: "var(--text-soft)" }}
    >
      {children}
    </button>
  );
}

/**
 * A body view with a thin `←`-back bar returning to what the pane normally
 * shows.
 *
 * `backLabel` exists because the shell pane uses this too, and "Back to the
 * conversation" on a plain terminal names something that is not there — the
 * label is read out by a screen reader and shown as the tooltip, so a wrong one
 * is a wrong statement rather than a cosmetic slip.
 */
export function BackView({
  label,
  onBack,
  backLabel = "Back to the conversation",
  children,
}: {
  label: string;
  onBack: () => void;
  backLabel?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className="flex shrink-0 items-center gap-2 border-b px-3 py-1"
        style={{ borderColor: "var(--border)" }}
      >
        <IconButton label={backLabel} onClick={onBack}>
          <Icon d={BACK_PATH} />
        </IconButton>
        <span className="micro" style={{ letterSpacing: "0.06em" }}>
          {label}
        </span>
      </div>
      <div className="min-h-0 flex-1">{children}</div>
    </div>
  );
}

export const TRANSCRIPT_PATH = "M4 6h16M4 10h16M4 14h10M4 18h7"; // stacked lines = a transcript
export const FOLDER_PATH = "M3 6v13h18V8h-9l-2-2H3z"; // folder = this project's files
export const BOARD_PATH = "M4 4h16v16H4zM10 4v16M16 4v16"; // kanban columns = Jira
export const SPLIT_PATH = "M4 4h16v16H4zM4 13h16"; // a pane divided = the companion shell
export const GIT_PATH = "M7 4v16M7 9h6a4 4 0 014 4v3M17 4v3"; // a branch leaving the trunk
export const GEAR_PATH = "M4 8h16M9 6v4M4 16h16M15 14v4"; // sliders = session settings
export const BACK_PATH = "M15 5l-7 7 7 7"; // ← return to the conversation
/**
 * A circling arrow with its head in the gap — reattach, not restart.
 *
 * Deliberately the same soft tone as every other icon in these headers: a
 * reattach is recovery, not an alarm, and `rgb(var(--primary))` is reserved for
 * "you must act now".
 */
export const RELOAD_PATH = "M21 12a9 9 0 11-2.64-6.36M21 3v5h-5"; // a circling arrow = reattach
