import { useCallback, useEffect, useState } from "react";

import { api, isTauri, type JiraIssue, type JiraIssueDetail, type JiraTransition } from "../lib/api";
import { classify, inProject, loadIssues, summarise, type IssuesState } from "../lib/jira";
// The same sanitiser the `.docx` reader uses, and for the same reason.
import { sanitize } from "../lib/office";
import type { Session } from "../lib/sessions";

/**
 * The session's Jira work.
 *
 * ## What this talks to
 *
 * `/agency/missions*` on the Cowork server — routes that exist and are
 * deployed. An earlier version of this file called invented profile-level
 * endpoints and reported their 404 as "your server does not expose issues",
 * which was simply wrong: the server has a complete session-independent Jira
 * API, under a name I had not looked for.
 *
 * The one real constraint is what those routes are *scoped to*. Every
 * issue-listing route is either the signed-in account's own assignments
 * (`/agency/missions`) or session-bound (`/sessions/:id/jira/issues`, which
 * needs a row in the server's own sessions table — the record rmux does not
 * create, because terminals and Claude are a direct SSH connection). So this
 * shows **your** issues, filtered to this session's project by key prefix,
 * which is what an issue key is.
 *
 * Two things therefore are not available here and are not faked: **creating an
 * issue** and **editing a description**. Both exist only under the
 * session-bound routes. Commenting does the second job without rewriting what
 * someone else wrote, and it is what the deployed API allows.
 *
 * ## Conventions
 *
 * Transitions are asked for per issue, never assumed — a Jira workflow decides
 * which moves are legal from the current status, and that differs by project
 * and issue type. Every mutating control reports its outcome beside itself.
 */

const CATEGORY_TONE: Record<string, string> = {
  done: "var(--text)",
  inprogress: "rgb(var(--busy))",
};

export function JiraPanel({ session }: { session: Session }) {
  const project = session.jiraProject ?? "";
  const [state, setState] = useState<IssuesState>({ state: "loading" });

  const reload = useCallback(async () => {
    setState(await loadIssues());
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    void loadIssues().then((result) => !cancelled && setState(result));
    return () => {
      cancelled = true;
    };
  }, [project]);

  if (state.state === "loading") {
    return (
      <div className="grid h-full place-items-center">
        <span className="micro">reading the board…</span>
      </div>
    );
  }

  if (state.state === "unavailable") {
    return (
      <div className="flex h-full flex-col items-start gap-3 p-6">
        <span className="kicker">{project}</span>
        <p className="data max-w-[560px] text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Sign in to Cowork to see your issues. rmux itself needs no account — this tab is the one
          surface that does, because the Jira credential lives on the server and never comes here.
        </p>
      </div>
    );
  }

  if (state.state === "error") {
    return (
      <div className="flex h-full flex-col items-start gap-3 p-6">
        <span className="kicker">{project}</span>
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {state.message}
        </p>
      </div>
    );
  }

  const mine = inProject(state.issues, project);
  const progress = summarise(mine);

  return (
    <div className="flex h-full flex-col overflow-hidden">
      <header
        className="flex shrink-0 items-baseline gap-4 border-b px-4 py-2"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="kicker">{project}</span>
        <span className="micro">
          {progress.done} DONE · {progress.inProgress} IN PROGRESS · {progress.todo} TO DO
        </span>
        <span className="micro ml-auto" style={{ color: "var(--text-faint)" }}>
          ASSIGNED TO YOU
        </span>
        <button type="button" className="chip" onClick={() => void reload()}>
          REFRESH
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {mine.length === 0 ? (
          <p className="micro p-4">nothing assigned to you in {project}</p>
        ) : (
          mine.map((issue) => <IssueRow key={issue.key} issue={issue} onChanged={reload} />)
        )}
      </div>

      <footer className="shrink-0 border-t px-4 py-2" style={{ borderColor: "var(--border)" }}>
        {/* Stated rather than left to be discovered by a control that is not
            there. Both are session-bound on the server; see the file comment. */}
        <span className="micro" style={{ color: "var(--text-faint)" }}>
          CREATING ISSUES AND EDITING DESCRIPTIONS ARE SESSION-BOUND ON THE SERVER — COMMENT INSTEAD
        </span>
      </footer>
    </div>
  );
}

function IssueRow({ issue, onChanged }: { issue: JiraIssue; onChanged: () => void }) {
  const [open, setOpen] = useState(false);
  const [detail, setDetail] = useState<JiraIssueDetail | null>(null);
  const [transitions, setTransitions] = useState<JiraTransition[] | null>(null);
  const [comment, setComment] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [posted, setPosted] = useState(false);

  // Only once the row is expanded. A board of two hundred issues would
  // otherwise fire two hundred requests to fill panels nobody opened.
  useEffect(() => {
    if (!open || detail) return;
    let cancelled = false;

    void (async () => {
      try {
        const [d, t] = await Promise.all([
          api.jiraMission(issue.key),
          api.jiraTransitions(issue.key),
        ]);
        if (cancelled) return;
        setDetail(d);
        setTransitions(t);
      } catch (e) {
        if (!cancelled) setError(classify(e).state === "unavailable" ? "sign in first" : String(e));
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [open, detail, issue.key]);

  const move = async (transition: JiraTransition) => {
    setBusy(transition.id);
    setError(null);
    try {
      await api.jiraTransition(issue.key, transition.id);
      // Re-read rather than assume: the workflow may have run a post-function
      // that lands the issue somewhere other than the transition's stated `to`.
      setDetail(null);
      setTransitions(null);
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const send = async () => {
    setBusy("comment");
    setError(null);
    try {
      await api.jiraComment(issue.key, comment.trim());
      setComment("");
      setPosted(true);
      setTimeout(() => setPosted(false), 2500);
      setDetail(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const tone = CATEGORY_TONE[(issue.statusCategory ?? "").toLowerCase()] ?? "var(--text-faint)";

  return (
    <div className="border-b" style={{ borderColor: "var(--border)" }}>
      <button
        type="button"
        className="flex w-full items-baseline gap-3 px-4 py-[7px] text-left"
        onClick={() => setOpen((o) => !o)}
      >
        <span className="data shrink-0 text-[11px]" style={{ color: "var(--text-soft)" }}>
          {issue.key}
        </span>
        <span className="data min-w-0 flex-1 truncate text-[11.5px]" style={{ color: "var(--text)" }}>
          {issue.summary}
        </span>
        <span className="micro shrink-0" style={{ color: tone }}>
          {issue.status}
        </span>
      </button>

      {open && (
        <div className="flex flex-col gap-3 px-4 pb-4">
          {detail === null ? (
            <span className="micro">loading…</span>
          ) : (
            <>
              {detail.descriptionHtml ? (
                <div
                  className="data text-[11px] leading-relaxed"
                  style={{ color: "var(--text-soft)" }}
                  // Jira renders its own markup server-side, so this arrives as
                  // HTML — and it is written by whoever filed the ticket, in a
                  // webview that can reach Tauri IPC. Sanitised for the same
                  // reason `.docx` HTML is: the CSP is not the only line of
                  // defence, and this is untrusted input.
                  dangerouslySetInnerHTML={{ __html: sanitize(detail.descriptionHtml) }}
                />
              ) : (
                <span className="micro" style={{ color: "var(--text-faint)" }}>
                  NO DESCRIPTION
                </span>
              )}

              {detail.comments.length > 0 && (
                <div className="flex flex-col gap-2" style={{ borderTop: "1px solid var(--border)", paddingTop: 8 }}>
                  {detail.comments.slice(-4).map((c, i) => (
                    <div key={i} className="flex flex-col gap-[2px]">
                      <span className="micro">{c.author ?? "someone"}</span>
                      <div
                        className="data text-[11px] leading-relaxed"
                        style={{ color: "var(--text-soft)" }}
                        dangerouslySetInnerHTML={{ __html: sanitize(c.bodyHtml) }}
                      />
                    </div>
                  ))}
                </div>
              )}
            </>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <span className="micro">MOVE TO</span>
            {transitions === null ? (
              <span className="micro">…</span>
            ) : transitions.length === 0 ? (
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                NO MOVES AVAILABLE
              </span>
            ) : (
              transitions.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  className="micro px-2 py-[3px]"
                  style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
                  disabled={busy !== null}
                  onClick={() => void move(t)}
                >
                  {busy === t.id ? "…" : t.name.toUpperCase()}
                </button>
              ))
            )}
            {issue.url && (
              <button
                type="button"
                className="chip ml-auto"
                onClick={() => void api.openExternal(issue.url!)}
              >
                OPEN IN JIRA
              </button>
            )}
          </div>

          <label className="flex flex-col gap-1">
            <span className="micro">COMMENT</span>
            <textarea
              value={comment}
              rows={2}
              placeholder="what changed, what you found"
              className="data inset resize-y px-2 py-[5px] text-[11px] leading-relaxed outline-none"
              style={{ border: "1px solid var(--border-strong)", color: "var(--text)", background: "transparent" }}
              onChange={(e) => setComment(e.target.value)}
            />
            <div className="flex items-center gap-3">
              <button
                type="button"
                className="btn"
                disabled={busy !== null || !comment.trim()}
                onClick={() => void send()}
              >
                {busy === "comment" ? "Posting…" : "Comment"}
              </button>
              {posted && <span className="micro">POSTED</span>}
            </div>
          </label>

          {error && (
            <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
              {error}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
