import { useState } from "react";

import { api, isChainId, type TargetRef } from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { reattachName } from "../lib/workspace-model";
import { clock, describeSwitch, recovered, refreshFallback, useFallbackState } from "../lib/fallback-watch";

/**
 * Session Settings › the chain this session is on: every member in order, which
 * one it runs on now, which are spent and until when, and the last switch.
 * Read from the host — the daemon is the authority, this is the readout.
 */
export function ChainStatus({ sessionId, target }: { sessionId: string; target: TargetRef | undefined }) {
  const session = useWorkspace((s) => s.sessions.find((x) => x.id === sessionId));
  const setActiveProfile = useWorkspace((s) => s.setActiveProfile);
  const chained = isChainId(session?.modelProfile);
  const daemonName = session ? reattachName(session) : "";
  const state = useFallbackState(target, daemonName, chained);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (!chained || !session) return null;

  const chain = state.binding?.binding?.chain;
  const labels = state.binding?.labels ?? {};
  const now = Date.now() / 1000;
  const back = new Set(recovered(state.binding, now));

  const move = (member: string) => {
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

  return (
    <div className="flex flex-col gap-[6px]" style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}>
      <span className="micro">FALLBACK CHAIN</span>
      {!chain ? (
        <span className="data text-[10.5px]" style={{ color: "var(--text-soft)" }}>
          {state.error
            ? `Could not read the chain from the host: ${state.error}`
            : "The host has not reported this session's chain yet — it appears once the session is running."}
        </span>
      ) : (
        <ol className="flex flex-col gap-[3px]">
          {chain.members.map((m, i) => {
            const spent = chain.exhausted?.[m];
            const isActive = m === chain.active;
            const until = spent ? (spent.resetsAt ?? spent.at + 5 * 3600) : 0;
            return (
              <li key={m} className="data flex items-center gap-2 text-[11px]">
                <span style={{ color: "var(--text-faint)" }}>{i + 1}.</span>
                <span style={{ color: isActive ? "var(--text)" : "var(--text-soft)", fontWeight: isActive ? 600 : 400 }}>
                  {labels[m] ?? m}
                </span>
                {isActive && <span className="micro">ACTIVE</span>}
                {spent && !isActive && (
                  <span className="micro" style={{ color: back.has(m) ? "var(--text-soft)" : "rgb(var(--primary))" }}>
                    {back.has(m)
                      ? `RESET SINCE ${clock(until)}`
                      : `SPENT · ${spent.resetsAt ? "RESETS" : "ASSUMED BACK"} ${clock(until)}`}
                  </span>
                )}
                {back.has(m) && (
                  <button type="button" className="micro" disabled={busy !== null} onClick={() => move(m)}>
                    {busy === m ? "SWITCHING…" : "SWITCH BACK"}
                  </button>
                )}
              </li>
            );
          })}
        </ol>
      )}
      {state.last && (
        <span className="data text-[10.5px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Last: {describeSwitch(state.last)}
        </span>
      )}
      {error && (
        <span className="data text-[10.5px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </span>
      )}
      <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
        The host moves this session forward on a usage or rate limit, resumes the conversation and asks it to
        continue. It never goes back on its own — switching back relaunches the conversation on that account,
        and is offered once its reset time has passed.
      </span>
    </div>
  );
}
