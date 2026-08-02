import { useEffect, useMemo, useState } from "react";

import { api, type ClaudeSessionInfo, type TargetRef } from "../lib/api";

/**
 * Every Claude conversation on a host, wherever it was started.
 *
 * **The folder comes from the session, not the other way round.** Resuming used
 * to mean finding the project directory first, which is backwards: what the
 * operator remembers is the conversation — "the one about the offload bug" —
 * not which of forty checkouts it happened in. Being made to hunt through a file
 * tree before the list of conversations even appears is the slowest possible
 * route to the thing they already had in mind.
 *
 * Every transcript records its own `cwd`, so rmux reads where each session
 * belongs and sets the folder itself. That also sidesteps the project-slug
 * problem entirely: `project_slug` has to *guess* how Claude Code spelled a
 * directory name and the scheme changed between versions, whereas this reads the
 * answer out of the file.
 */
export function AllSessions({
  target,
  label,
  busy,
  onResume,
  onBack,
}: {
  target: TargetRef;
  label: string;
  busy: boolean;
  onResume: (session: ClaudeSessionInfo) => void;
  onBack: () => void;
}) {
  const [sessions, setSessions] = useState<ClaudeSessionInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    let cancelled = false;
    api
      .claudeListAllSessions(target)
      .then((list) => !cancelled && setSessions(list))
      .catch((e) => !cancelled && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [target]);

  const shown = useMemo(() => {
    const all = sessions ?? [];
    if (!filter.trim()) return all;
    const needle = filter.toLowerCase();
    return all.filter(
      (s) =>
        s.title?.toLowerCase().includes(needle) ||
        s.folder?.toLowerCase().includes(needle) ||
        s.id.toLowerCase().includes(needle),
    );
  }, [sessions, filter]);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div className="flex items-baseline gap-3">
        <span className="micro">
          on <span style={{ color: "var(--text)" }}>{label}</span>
        </span>
        <button type="button" className="micro ml-auto" onClick={onBack}>
          ← BROWSE FOLDERS INSTEAD
        </button>
      </div>

      <input
        value={filter}
        spellCheck={false}
        placeholder="filter by title or folder"
        className="data inset shrink-0 px-2 py-[5px] text-[12px] outline-none"
        style={{ border: "1px solid var(--border-strong)", color: "var(--text)", background: "transparent" }}
        onChange={(e) => setFilter(e.target.value)}
      />

      {error && (
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}

      {sessions === null && !error ? (
        <span className="micro">reading every transcript on {label}…</span>
      ) : shown.length === 0 ? (
        <span className="micro">
          {sessions?.length ? "nothing matches" : `no Claude sessions found on ${label}`}
        </span>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto" style={{ border: "1px solid var(--border)" }}>
          {shown.map((session) => (
            <button
              key={session.id}
              type="button"
              disabled={busy}
              onClick={() => onResume(session)}
              className="flex w-full flex-col gap-[2px] border-b px-3 py-2 text-left"
              style={{ borderColor: "var(--border)" }}
            >
              <span className="flex items-baseline gap-2">
                <span className="data flex-1 truncate text-[12px]" style={{ color: "var(--text)" }}>
                  {session.title || "untitled conversation"}
                </span>
                <span className="micro shrink-0">{ago(session.modified)}</span>
              </span>
              {/* The folder is shown, not hidden, even though it is chosen
                  automatically: two conversations can share a title, and this is
                  what tells them apart. */}
              <span className="micro truncate" style={{ letterSpacing: "0.1em" }}>
                {session.folder}
              </span>
            </button>
          ))}
        </div>
      )}

      {sessions !== null && sessions.length > 0 && (
        <span className="micro" style={{ color: "var(--text-faint)" }}>
          {sessions.length} CONVERSATION{sessions.length === 1 ? "" : "S"} · NEWEST FIRST · THE
          FOLDER IS SET FOR YOU
        </span>
      )}
    </div>
  );
}

/** "3h ago". Coarse on purpose — the exact minute never matters here. */
function ago(unixSeconds: number): string {
  const seconds = Math.max(0, Date.now() / 1000 - unixSeconds);
  if (seconds < 90) return "just now";
  const minutes = seconds / 60;
  if (minutes < 90) return `${Math.round(minutes)}m ago`;
  const hours = minutes / 60;
  if (hours < 36) return `${Math.round(hours)}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}
