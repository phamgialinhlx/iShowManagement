/**
 * How much Claude is allowed to do without asking, chosen at the moment a
 * session is created.
 *
 * **This is a launch argument, not a setting.** `--dangerously-skip-permissions`
 * is a judgement about one piece of work on one machine — a scratch container
 * you would happily rebuild, versus a server with the team's data on it. Stored
 * as a preference it would silently carry that judgement into work started weeks
 * later somewhere else, which is precisely the case where nobody re-reads the
 * flag. So it is asked here, per session, and it travels with that session
 * rather than with the app.
 *
 * It *is* kept with the session, though, because a pane that restarts must come
 * back with the same latitude it was launched with — quietly gaining or losing
 * permission checks across a reconnect would be worse than either setting.
 *
 * The default is the safe one, and the unsafe one has to be clicked.
 */
export function PermissionChoice({
  value,
  onChange,
  disabled,
}: {
  value: boolean;
  onChange: (skip: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <div className="flex flex-col gap-[6px]">
      <span className="micro">PERMISSIONS</span>

      <div className="flex" style={{ border: "1px solid var(--border-strong)" }}>
        <Choice
          selected={!value}
          disabled={disabled}
          onClick={() => onChange(false)}
          label="ASK EACH TIME"
        />
        <Choice
          selected={value}
          disabled={disabled}
          onClick={() => onChange(true)}
          label="SKIP PERMISSIONS"
          danger
        />
      </div>

      {/* Only shown for the dangerous choice, and it states the consequence
          rather than repeating the label. A caution that is always on screen
          stops being read — this one appears exactly when it applies, which is
          the one moment the operator has the context to weigh it. */}
      {value && (
        <span className="data text-[10.5px] leading-[1.45]" style={{ color: "rgb(var(--primary))" }}>
          Claude will edit files and run commands without asking. Use it where you would be
          comfortable rebuilding the machine.
        </span>
      )}
    </div>
  );
}

function Choice({
  selected,
  disabled,
  onClick,
  label,
  danger,
}: {
  selected: boolean;
  disabled?: boolean;
  onClick: () => void;
  label: string;
  danger?: boolean;
}) {
  // Red is reserved for where the operator must act — and the moment of taking
  // the unsupervised option is one. It is not worn while merely offered.
  const accent = danger ? "rgb(var(--primary))" : "var(--text)";
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      aria-pressed={selected}
      className="micro flex-1 px-3 py-[7px] text-center"
      style={{
        color: selected ? accent : "var(--text-soft)",
        background: selected ? "var(--app-elev)" : "transparent",
        borderLeft: label.startsWith("SKIP") ? "1px solid var(--border-strong)" : undefined,
        boxShadow: selected ? `inset 0 -2px 0 ${accent}` : undefined,
      }}
    >
      {label}
    </button>
  );
}
