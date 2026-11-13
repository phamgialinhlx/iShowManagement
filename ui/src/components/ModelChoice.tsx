import { useEffect, useState } from "react";

import { api, type ModelProfile, type ProfileChain, type ProfileKind } from "../lib/api";

/** What "no profile" means for each kind — the provider the agent defaults to. */
const DEFAULT_PROVIDER: Record<ProfileKind, { label: string; host: string; hint: string }> = {
  claude: {
    label: "ANTHROPIC",
    host: "api.anthropic.com",
    hint: "MODEL · ANTHROPIC — ADD A PROFILE IN SETTINGS TO USE KIMI, GLM OR A GATEWAY",
  },
  deepseek: {
    label: "HOST LOGIN",
    host: "api.deepseek.com",
    hint: "MODEL · THE HOST'S OWN DEEPSEEK SETUP — ADD A DEEPSEEK PROFILE IN SETTINGS TO SEND A KEY",
  },
  openai: {
    label: "HOST LOGIN",
    host: "api.openai.com",
    hint: "MODEL · THE HOST'S OWN CODEX LOGIN — ADD AN OPENAI PROFILE IN SETTINGS TO USE AN API KEY",
  },
};

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
  /**
   * Which agent's profiles to offer. Only profiles of this kind are listed: a
   * DeepSeek profile on a Claude session would be refused at start anyway, and
   * offering it is a control that cannot work.
   */
  kind = "claude",
}: {
  value: string | null;
  onChange: (id: string | null) => void;
  disabled?: boolean;
  hintWhenEmpty?: boolean;
  kind?: ProfileKind;
}) {
  const [all, setProfiles] = useState<ModelProfile[] | null>(null);
  const [allChains, setChains] = useState<ProfileChain[]>([]);

  useEffect(() => {
    api.modelProfiles().then(setProfiles).catch(() => setProfiles([]));
    // Chains are Claude-only; an older backend without the command offers none.
    api.modelChains().then((v) => setChains(v.chains)).catch(() => setChains([]));
  }, []);
  const chains = allChains.filter((c) => (c.kind ?? "claude") === kind);

  // `kind ?? "claude"` on each profile: a backend from before kinds sends none.
  const profiles = all && all.filter((p) => (p.kind ?? "claude") === kind);
  const fallback = DEFAULT_PROVIDER[kind];

  // Not `null`. An absent control reads as "this feature does not exist", and
  // the operator goes looking for it in Settings — the rule that a state which
  // can outlast a frame needs a visible state of its own.
  if (profiles === null) {
    return (
      <span className="micro" style={{ color: "var(--text-faint)" }}>
        READING MODEL PROFILES…
      </span>
    );
  }

  if (profiles.length === 0 && chains.length === 0) {
    if (!hintWhenEmpty) return null;
    // A session still naming a profile (deleted, or of another kind) must say
    // so even when nothing else is on offer — that is a session refusing to start.
    if (value !== null) {
      return (
        <span className="data text-[10.5px]" style={{ color: "rgb(var(--primary))" }}>
          This session's model profile no longer exists. Pick another before starting it.
        </span>
      );
    }
    return <span className="micro">{fallback.hint}</span>;
  }

  const selected = profiles.find((p) => p.id === value) ?? null;
  const chain = chains.find((c) => c.id === value) ?? null;
  const nameOf = (id: string) => profiles.find((p) => p.id === id)?.name ?? `#${id}`;

  // A session pointing at a deleted profile must say so rather than quietly
  // reading as Anthropic — that is a silent change of provider.
  const missing = value !== null && selected === null && chain === null;

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
          label={fallback.label}
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
        {chains.map((c) => (
          <Choice
            key={c.id}
            selected={value === c.id}
            disabled={disabled}
            onClick={() => onChange(c.id)}
            label={`CHAIN · ${c.name.toUpperCase()}`}
          />
        ))}
      </div>

      {/* A chain moves the session between accounts on its own; say so, and
          in which order, before it is chosen rather than after it happens. */}
      {chain && (
        <span className="data text-[10.5px] leading-[1.45]" style={{ color: "var(--text-soft)" }}>
          Starts on <span style={{ color: "var(--text)" }}>{nameOf(chain.members[0] ?? "")}</span>. On a usage
          limit the host moves it to the next of{" "}
          <span style={{ color: "var(--text)" }}>{chain.members.map(nameOf).join(" → ")}</span>, resumes the
          conversation and says so in the pane. It never cycles back on its own.
        </span>
      )}

      {/* The destination, stated. Choosing a profile decides where a bearer
          token is sent, and a profile's name proves nothing about its URL. */}
      {selected && (
        <span className="data text-[10.5px] leading-[1.45]" style={{ color: "var(--text-soft)" }}>
          Requests and {selected.hasCredential ? "this profile's token" : "the host's own login"} go
          to <span style={{ color: "var(--text)" }}>{selected.endpoint ?? fallback.host}</span>
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
