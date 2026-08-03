import { useEffect, useState } from "react";

import { api, type ModelProfile } from "../lib/api";

/**
 * Which provider this session's Claude runs against.
 *
 * **Per session, and kept with it** — the same reasoning as `PermissionChoice`.
 * Which provider a piece of work runs on is a property of that work: this
 * project on the org's Anthropic account, that experiment on a cheaper vendor.
 * An app-wide default would silently move a conversation to another provider on
 * the next restart, which is billing and prompt-routing changing underneath
 * someone with nothing on screen to say so.
 *
 * **Absent, not disabled, when there is nothing to choose.** Someone with no
 * profiles configured has no decision to make here, and a dead control invites
 * "how do I turn this on?" A one-line pointer to Settings appears instead, and
 * only on the launch screen where the question could arise.
 */
export function ModelChoice({
  value,
  onChange,
  disabled,
  /** Show a pointer to Settings when nothing is configured yet. */
  hintWhenEmpty,
}: {
  value: string | null;
  onChange: (id: string | null) => void;
  disabled?: boolean;
  hintWhenEmpty?: boolean;
}) {
  const [profiles, setProfiles] = useState<ModelProfile[] | null>(null);

  useEffect(() => {
    api.modelProfiles().then(setProfiles).catch(() => setProfiles([]));
  }, []);

  if (profiles === null) return null;

  if (profiles.length === 0) {
    if (!hintWhenEmpty) return null;
    return (
      <span className="micro">
        MODEL · ANTHROPIC — ADD A PROFILE IN SETTINGS TO USE KIMI, GLM OR A GATEWAY
      </span>
    );
  }

  const selected = profiles.find((p) => p.id === value) ?? null;

  // A session pointing at a deleted profile must say so rather than quietly
  // reading as Anthropic — that is a silent change of provider.
  const missing = value !== null && selected === null;

  return (
    <div className="flex flex-col gap-[6px]">
      <span className="micro">MODEL</span>

      {/* Segmented rather than a dropdown: a `select` hides both the options and
          the current value behind a click, and the current value is the whole
          point here. */}
      <div className="seg seg-wrap">
        <Choice
          selected={value === null}
          disabled={disabled}
          onClick={() => onChange(null)}
          label="ANTHROPIC"
        />
        {profiles.map((p) => (
          <Choice
            key={p.id}
            selected={value === p.id}
            disabled={disabled}
            onClick={() => onChange(p.id)}
            label={p.name.toUpperCase()}
          />
        ))}
      </div>

      {/* The destination, stated. Choosing a profile decides where a bearer
          token is sent, and a profile's name proves nothing about its URL. */}
      {selected && (
        <span className="data text-[10.5px] leading-[1.45]" style={{ color: "var(--text-soft)" }}>
          Requests and {selected.hasCredential ? "this profile's token" : "the host's own login"} go
          to <span style={{ color: "var(--text)" }}>{selected.endpoint ?? "api.anthropic.com"}</span>
          .
        </span>
      )}

      {missing && (
        <span className="data text-[10.5px]" style={{ color: "rgb(var(--primary))" }}>
          This session's model profile has been deleted. Pick another before starting it.
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
}: {
  selected: boolean;
  disabled?: boolean;
  onClick: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-pressed={selected}
      // Fill, tone and underline all come from `.seg` now — this was a
      // hand-rolled copy of it, and two definitions of "selected" drift.
      style={{ padding: "6px 12px" }}
    >
      {label}
    </button>
  );
}
