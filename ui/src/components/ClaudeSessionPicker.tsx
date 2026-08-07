import { useEffect, useState } from "react";

import { api, type ClaudeSessionInfo, type TargetRef } from "../lib/api";

/**
 * The Claude conversations already recorded in a folder.
 *
 * Resuming is offered **before** starting fresh, and deliberately so: the
 * context of the work you did here yesterday is almost always worth more than a
 * clean slate, and a picker that buries it trains you to lose it.
 */

/** "3 hours ago" — the form you actually reason about when choosing a session. */
function ago(unixSeconds: number): string {
  if (!unixSeconds) return "unknown";
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - unixSeconds);

  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "yesterday" : `${days}d ago`;
}

export function ClaudeSessionPicker({
  target,
  folder,
  busy,
  onChoose,
}: {
  target: TargetRef;
  folder: string;
  busy: boolean;
  /**
   * `undefined` means start a new conversation.
   *
   * The title comes back too: it becomes the session's name in the rail, so
   * resuming yesterday's work shows what that work *was* rather than the folder
   * it happened in — which is identical for every session in that folder.
   */
  onChoose: (resume?: string, title?: string) => void;
}) {
  const [sessions, setSessions] = useState<ClaudeSessionInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .claudeSessions(target, folder)
      .then((found) => {
        if (!cancelled) setSessions(Array.isArray(found) ? found : []);
      })
      .catch((e) => {
        // Not fatal — you can always start a new conversation. Say what happened
        // rather than showing an empty list, which would look like "no history"
        // when the truth is "could not look".
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
          setSessions([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [target, folder]);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <span className="micro truncate" title={folder}>
        {folder}
      </span>

      <div
        // **A ceiling of its own, not an inherited one.**
        //
        // `flex-1` only bounds this list when an ancestor has a definite
        // height. Inside the rail it did; inside the quick-add modal, which
        // sizes to its content, nothing capped it — so a host with forty
        // conversations grew the dialog past the bottom of the screen, taking
        // its own buttons with it and leaving no way to scroll to them.
        // A `max-height` here is true in both, and cannot be undone by a
        // parent that has not been laid out yet.
        className="inset min-h-0 flex-1 overflow-y-auto"
        style={{ border: "1px solid var(--border)", maxHeight: "46vh" }}
      >
        {sessions === null && <p className="micro p-2">looking for previous sessions…</p>}

        {sessions?.map((session) => (
          <button
            key={session.id}
            type="button"
            disabled={busy}
            onClick={() => onChoose(session.id, session.title ?? undefined)}
            className="flex w-full flex-col gap-[2px] px-2 py-[6px] text-left"
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            <span className="data truncate text-[12px]">
              {/* A conversation too short for Claude to have titled is still
                  resumable; naming it by id beats hiding it. */}
              {session.title ?? `untitled · ${session.id.slice(0, 8)}`}
            </span>
            <span className="micro">{ago(session.modified)}</span>
          </button>
        ))}

        {sessions?.length === 0 && (
          <p className="micro p-2 leading-relaxed">
            no previous Claude sessions in this folder
          </p>
        )}
      </div>

      {error && (
        <p className="micro" style={{ color: "var(--warn)" }}>
          could not read session history: {error}
        </p>
      )}

      <button
        type="button"
        className="btn btn-primary w-full"
        disabled={busy}
        onClick={() => onChoose(undefined)}
      >
        {busy ? "Opening…" : "New Claude session"}
      </button>
    </div>
  );
}
