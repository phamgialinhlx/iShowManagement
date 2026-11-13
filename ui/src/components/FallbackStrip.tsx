import { useEffect, useState } from "react";

import { api, isChainId, type TargetRef } from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { describeSwitch, recovered, refreshFallback, useFallbackState } from "../lib/fallback-watch";

/**
 * The loud half of a fallback chain: a strip across the top of the pane that
 * says, in a sentence, that this conversation moved to another account, from
 * which, when, and when the first one resets.
 *
 * A switch the operator does not notice is a provider change they did not see —
 * the failure the profile design exists to prevent — so it is not a toast that
 * fades. It stays until dismissed, and comes back with the next switch.
 *
 * It also keeps the session's `activeProfile` in step with the host, so a
 * restart from the UI resumes on the member the daemon chose, and offers
 * **switch back** once an earlier member's reset time has passed. Offered,
 * never done: moving a running conversation is the operator's call.
 */
export function FallbackStrip({
  sessionId,
  target,
  daemonName,
}: {
  sessionId: string;
  target: TargetRef | undefined;
  daemonName: string;
}) {
  const session = useWorkspace((s) => s.sessions.find((x) => x.id === sessionId));
  const setActiveProfile = useWorkspace((s) => s.setActiveProfile);
  const chained = isChainId(session?.modelProfile);
  const state = useFallbackState(target, daemonName, chained);
  const [dismissed, setDismissed] = useState<number | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now() / 1000);

  const active = state.binding?.binding?.set;
  useEffect(() => {
    if (chained && active && active !== session?.activeProfile) setActiveProfile(sessionId, active);
  }, [chained, active, session?.activeProfile, sessionId, setActiveProfile]);

  // A reset time passing is the one thing that changes with nothing on the
  // wire, so the clock is re-read once a minute — only while chained.
  useEffect(() => {
    if (!chained) return;
    const t = setInterval(() => setNow(Date.now() / 1000), 60_000);
    return () => clearInterval(t);
  }, [chained]);

  if (!chained) return null;
  const last = state.last;
  const back = recovered(state.binding, now);
  const labels = state.binding?.labels ?? {};
  const showRecord = last && last.at !== dismissed;
  if (!showRecord && back.length === 0) return null;

  const switchBack = (member: string) => {
    if (!target) return;
    setBusy(member);
    setError(null);
    api
      .claudeFallbackSwitch(target, daemonName, member)
      .then(() => {
        setActiveProfile(sessionId, member);
        refreshFallback(target);
      })
      .catch((e) => setError(typeof e === "string" ? e : String(e)))
      .finally(() => setBusy(null));
  };

  const alarm = last && (last.outcome.startsWith("end_of_chain") || last.outcome.startsWith("failed"));
  return (
    <div
      className="data flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-[6px] text-[11px]"
      style={{
        borderBottom: "1px solid var(--line, rgba(255,255,255,0.08))",
        background: "rgb(var(--primary) / 0.12)",
        color: alarm ? "rgb(var(--primary))" : "var(--text)",
      }}
      role="status"
    >
      {showRecord && <span className="min-w-0 flex-1">{describeSwitch(last)}</span>}
      {back.map((m) => (
        <button
          key={m}
          type="button"
          className="micro"
          disabled={busy !== null}
          onClick={() => switchBack(m)}
          title="Relaunches this conversation on that account. Nothing is typed into it."
        >
          {busy === m ? "SWITCHING…" : `SWITCH BACK TO ${(labels[m] ?? m).toUpperCase()}`}
        </button>
      ))}
      {error && <span style={{ color: "rgb(var(--primary))" }}>{error}</span>}
      {showRecord && (
        <button type="button" className="micro" onClick={() => setDismissed(last.at)} aria-label="Dismiss">
          DISMISS
        </button>
      )}
    </div>
  );
}
