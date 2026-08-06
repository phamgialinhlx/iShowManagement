import { useEffect, useMemo, useState } from "react";

import { isTauri, type JiraIssue } from "../lib/api";
import { loadIssues, type IssuesState } from "../lib/jira";
import {
  JIRA_EVENT,
  isDone,
  isInProgress,
  planFor,
  planIssues,
  readFocus,
  togglePlan,
} from "../lib/jira-focus";

/**
 * Today's Jira, on the Progress page.
 *
 * The rail widget answers "what am I on, in *this* pane". This answers the two
 * questions that only make sense across the whole day: **is the list I set this
 * morning going anywhere**, and **which session is on which ticket** — the
 * second being unanswerable from any single pane, which is the entire reason it
 * belongs here.
 *
 * ## It renders nothing at all when there is no Jira
 *
 * Not an empty state, not a "connect Jira" panel: nothing. Signing in is
 * optional in rmux and most of this page works without it, so an
 * unconfigurable-from-here prompt on every visit would be an advert. The rail
 * widget already says "sign in to see your board" in the one place where
 * somebody has asked for a board.
 */
export function JiraToday({
  sessions,
  onOpen,
}: {
  sessions: readonly { id: string; name: string }[];
  onOpen: (sessionId: string) => void;
}) {
  const [state, setState] = useState<IssuesState>({ state: "loading" });
  const [plan, setPlan] = useState<string[]>(() => planFor());
  const [focus, setFocus] = useState(() => readFocus());

  useEffect(() => {
    if (!isTauri()) {
      setState({ state: "unavailable" });
      return;
    }
    let cancelled = false;
    void loadIssues().then((r) => !cancelled && setState(r));
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const sync = () => {
      setPlan(planFor());
      setFocus(readFocus());
    };
    window.addEventListener(JIRA_EVENT, sync);
    window.addEventListener("storage", sync);
    return () => {
      window.removeEventListener(JIRA_EVENT, sync);
      window.removeEventListener("storage", sync);
    };
  }, []);

  const issues = state.state === "ready" ? state.issues : [];

  /** Sessions that have picked a ticket, with the ticket resolved. */
  const working = useMemo(() => {
    const byKey = new Map(issues.map((i) => [i.key.toUpperCase(), i]));
    return sessions
      .map((s) => ({ session: s, issue: byKey.get((focus[s.id] ?? "").toUpperCase()) }))
      .filter((row): row is { session: { id: string; name: string }; issue: JiraIssue } => !!row.issue);
  }, [sessions, focus, issues]);

  const chosen = planIssues(issues, plan);

  // Nothing to say and no way to fix it from here.
  if (state.state !== "ready") return null;
  if (!chosen.length && !working.length) return null;

  const done = chosen.filter(isDone).length;

  return (
    <section className="flex flex-col gap-2">
      <span className="micro">JIRA · TODAY</span>

      <div className="grid gap-5" style={{ gridTemplateColumns: "repeat(auto-fit, minmax(320px, 1fr))" }}>
        {chosen.length > 0 && (
          <Card>
            <div className="flex items-baseline justify-between">
              <span className="micro">ON TODAY&rsquo;S LIST</span>
              <span className="data text-[11px] tabular-nums" style={{ color: "var(--text)" }}>
                {done} / {chosen.length}
              </span>
            </div>

            <div
              className="mt-2 flex h-[8px] w-full overflow-hidden"
              style={{ background: "color-mix(in srgb, var(--text) 10%, transparent)" }}
            >
              <div
                style={{
                  width: `${(done / chosen.length) * 100}%`,
                  background: "var(--text)",
                  transition: "width var(--dur) var(--ease)",
                }}
              />
              <div
                style={{
                  width: `${(chosen.filter(isInProgress).length / chosen.length) * 100}%`,
                  background: "rgb(var(--busy))",
                  transition: "width var(--dur) var(--ease)",
                }}
              />
            </div>

            <ul className="mt-2 flex max-h-[260px] flex-col overflow-y-auto">
              {chosen.map((issue) => (
                <li
                  key={issue.key}
                  className="flex items-center gap-2 border-b py-[5px]"
                  style={{ borderColor: "var(--border)" }}
                >
                  <span
                    className="data shrink-0 text-[10px]"
                    style={{ color: isDone(issue) ? "var(--text-faint)" : "rgb(var(--busy))" }}
                  >
                    {issue.key}
                  </span>
                  <span
                    className="data flex-1 truncate text-[12px]"
                    style={{
                      color: isDone(issue) ? "var(--text-soft)" : "var(--text)",
                      textDecoration: isDone(issue) ? "line-through" : "none",
                    }}
                  >
                    {issue.summary}
                  </span>
                  <span className="micro shrink-0" style={{ color: "var(--text-faint)" }}>
                    {issue.status}
                  </span>
                  {/* Taking something off today's list is not the same as
                      closing it, so this removes it from the plan and never
                      touches Jira. */}
                  <button
                    type="button"
                    className="micro link shrink-0"
                    style={{ color: "var(--text-faint)" }}
                    title="Take off today's list"
                    onClick={() => setPlan(togglePlan(issue.key))}
                  >
                    REMOVE
                  </button>
                </li>
              ))}
            </ul>
          </Card>
        )}

        {working.length > 0 && (
          <Card>
            <span className="micro">WHO IS ON WHAT</span>
            <ul className="mt-2 flex flex-col">
              {working.map(({ session, issue }) => (
                <li
                  key={session.id}
                  className="flex items-center gap-2 border-b py-[5px]"
                  style={{ borderColor: "var(--border)" }}
                >
                  <button
                    type="button"
                    className="data link min-w-0 flex-1 truncate text-left text-[12px]"
                    style={{ color: "var(--text)" }}
                    onClick={() => onOpen(session.id)}
                    title={`Go to ${session.name}`}
                  >
                    {session.name}
                  </button>
                  <span
                    className="data shrink-0 text-[10px]"
                    style={{ color: isDone(issue) ? "var(--text-faint)" : "rgb(var(--busy))" }}
                  >
                    {issue.key}
                  </span>
                  <span
                    className="data w-[150px] shrink-0 truncate text-right text-[11px]"
                    style={{ color: "var(--text-soft)" }}
                  >
                    {issue.summary}
                  </span>
                </li>
              ))}
            </ul>
          </Card>
        )}
      </div>
    </section>
  );
}

/** Matches the Progress page's own card, so the section does not read as bolted on. */
function Card({ children }: { children: React.ReactNode }) {
  return (
    <div className="inset px-3 py-3" style={{ border: "1px solid var(--border)" }}>
      {children}
    </div>
  );
}
