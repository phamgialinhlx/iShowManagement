import { useEffect, useRef, useState } from "react";

import { ModelChoice } from "./ModelChoice";
import { PermissionChoice } from "./PermissionChoice";

/**
 * The two questions a new session cannot answer for you.
 *
 * The rail's `+` used to create a session outright: the name from the folder,
 * the profile inherited when the group agreed, and permissions silently safe.
 * That is the right default set and the wrong way to apply it, because the two
 * things it decided are the two the operator is *supposed* to decide.
 *
 * - **`--dangerously-skip-permissions` is a judgement about one piece of work on
 *   one machine, made at the moment of starting it.** The design rule is that
 *   the dangerous option is never the default and never one click from the safe
 *   one. `+` honoured the first half and skipped the second: you could not
 *   choose it here at all, so the quick path was the *un*-configurable one and
 *   anyone wanting it had to go the long way round.
 * - **A model profile decides which vendor the operator's credential is sent
 *   to.** Inheriting it from the group is a good guess and stays the default
 *   here, but a guess about where a paid credential goes should be visible
 *   before it is acted on, not discovered afterwards in Session Settings.
 *
 * It reuses `PermissionChoice` and `ModelChoice` rather than restating them, so
 * the quick path and the new-session wizard cannot drift into offering
 * different words for the same choice — and the endpoint a profile carries is
 * printed by `ModelChoice` in both.
 */
export function QuickAdd({
  label,
  where,
  inheritedProfile,
  busy,
  onCancel,
  onCreate,
}: {
  /** The name the session will take — the group's folder. */
  label: string;
  /** `folder on host`, so the target is stated rather than assumed. */
  where: string;
  inheritedProfile?: string;
  busy: boolean;
  onCancel: () => void;
  onCreate: (choice: { skipPermissions: boolean; modelProfile?: string }) => void;
}) {
  const [skip, setSkip] = useState(false);
  const [profile, setProfile] = useState<string | null>(inheritedProfile ?? null);
  const box = useRef<HTMLDivElement>(null);

  // Escape closes it, and the first control takes focus — this opens from a
  // click on a 14px button, so the hand is already on the pointer but the
  // keyboard has nowhere to go otherwise.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onCancel();
      }
    };
    const el = box.current;
    el?.addEventListener("keydown", onKey);
    el?.querySelector("button")?.focus();
    return () => el?.removeEventListener("keydown", onKey);
  }, [onCancel]);

  return (
    <div
      ref={box}
      role="dialog"
      aria-label={`New session in ${label}`}
      className="inset flex flex-col gap-3 px-3 py-3"
      style={{ borderBottom: "1px solid var(--border)" }}
    >
      <div className="flex flex-col gap-[2px]">
        <span className="micro" style={{ color: "var(--text-soft)" }}>
          NEW SESSION
        </span>
        <span className="data truncate text-[12px]" style={{ color: "var(--text)" }}>
          {label}
        </span>
        {/* The host is stated. Two groups can share a folder name across
            machines, and this control is reached without ever naming one. */}
        <span className="micro truncate" style={{ color: "var(--text-faint)" }}>
          {where}
        </span>
      </div>

      <PermissionChoice value={skip} onChange={setSkip} disabled={busy} />

      <ModelChoice value={profile} onChange={setProfile} disabled={busy} hintWhenEmpty />

      <div className="flex gap-2">
        <button
          type="button"
          className="chip"
          disabled={busy}
          onClick={() => onCreate({ skipPermissions: skip, modelProfile: profile ?? undefined })}
          style={{ flex: "1 1 auto" }}
        >
          {busy ? "STARTING…" : "START"}
        </button>
        <button type="button" className="chip" disabled={busy} onClick={onCancel}>
          CANCEL
        </button>
      </div>
    </div>
  );
}
