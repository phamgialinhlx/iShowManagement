import { useCallback, useEffect, useRef, useState } from "react";
import { motion } from "motion/react";

import { api, isTauri, type JiraIssue, type JiraTransition } from "../../lib/api";
import { inProject, loadIssues, type IssuesState } from "../../lib/jira";
import {
  JIRA_EVENT,
  focusOf,
  isDone,
  isInProgress,
  planFor,
  planIssues,
  planProgress,
  setFocus,
  togglePlan,
} from "../../lib/jira-focus";

/**
 * The board, what you are on, and what you meant to finish today.
 *
 * ## Why it is more than a bar
 *
 * A progress bar answers "how is the project going", which nobody needs four
 * times a day. In a grid of sessions the question that actually comes up is
 * *"which ticket is this pane?"* — and the honest answer used to live only in
 * the operator's head, one per pane, which is exactly the thing a workbench
 * should be holding for them.
 *
 * ## The three things it does, and the rule each one keeps
 *
 *  - **Selection is per session and it persists.** Same reasoning as the model
 *    profile: which ticket a piece of work belongs to is a property of that
 *    work, and a pane that comes back tomorrow pointed at nothing has lost
 *    something the operator now has to remember instead.
 *  - **Transitions are asked for, never assumed.** Jira workflows are
 *    per-project and can forbid any move; the buttons here are literally what
 *    `/transitions` returned for that issue at that moment. Guessing "To Do →
 *    In Progress → Done" produces controls that fail on half the boards they
 *    appear on.
 *  - **Finishing one prompts for the next, in place.** It does *not* open a
 *    modal over the workbench: a dialog appearing while somebody is typing in a
 *    terminal takes their keystrokes, and the rule here is that nothing moves
 *    under the operator's hands. The panel opens inside the widget, where the
 *    change happened, and waits.
 *
 * ## What it deliberately does not do
 *
 * **It cannot create an issue**, because the server exposes transitions and
 * comments and no route that creates one. Rather than a button that fails, the
 * next-up panel offers to open Jira's own create screen in the real browser.
 * That works today; a create button would be a control that cannot work, which
 * the interface rules forbid outright.
 */

/** How often the board is re-read. Slow, deliberately: it is somebody else's system. */
const POLL_MS = 60_000;

type Panel = "none" | "pick" | "next";

export function JiraProgress({ project, sessionId }: { project: string; sessionId: string }) {
  const [state, setState] = useState<IssuesState>({ state: "loading" });
  const [focus, setFocusKey] = useState<string | undefined>(() => focusOf(sessionId));
  const [plan, setPlan] = useState<string[]>(() => planFor());
  const [panel, setPanel] = useState<Panel>("none");
  const [moves, setMoves] = useState<JiraTransition[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  /**
   * Transitions are revealed, not left lying around.
   *
   * Measured on a real board: ten of them — `TO DO`, `IN PROGRESS`, `DONE`,
   * `TASK PENDING`, `READY FOR QA`, `STAGING`, `CLOSED`, `APPROVED`,
   * `NEED CLARIFICATION`, `READY TO CHECK`. Left on screen that is five rows of
   * identical buttons pushing every instrument below out of the rail, and one
   * of them is `CLOSED`: a stray click moves a ticket in a system the whole
   * team reads. One deliberate step first, and the status stays visible either
   * way, because *reading* the state is the common case and changing it is not.
   */
  const [moving, setMoving] = useState(false);

  /** The last category we *observed* for the focused issue, to spot the edge. */
  const wasDone = useRef<boolean | null>(null);

  const refresh = useCallback(async () => {
    const result = await loadIssues();
    setState(result);
    return result;
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    const tick = () => {
      void loadIssues().then((r) => !cancelled && setState(r));
    };
    tick();
    const timer = window.setInterval(tick, POLL_MS);
    // Coming back to the app is the moment a board is most likely to have moved
    // under you — somebody else closed your ticket while you were away.
    window.addEventListener("focus", tick);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
      window.removeEventListener("focus", tick);
    };
  }, [project]);

  /** Selections can change from the Progress page too. */
  useEffect(() => {
    const sync = () => {
      setFocusKey(focusOf(sessionId));
      setPlan(planFor());
    };
    window.addEventListener(JIRA_EVENT, sync);
    window.addEventListener("storage", sync);
    return () => {
      window.removeEventListener(JIRA_EVENT, sync);
      window.removeEventListener("storage", sync);
    };
  }, [sessionId]);

  const issues = state.state === "ready" ? state.issues : [];
  const current = focus ? issues.find((i) => i.key === focus) : undefined;

  /** Load the moves this issue actually permits, whenever it changes. */
  useEffect(() => {
    if (!focus || !isTauri()) {
      setMoves(null);
      return;
    }
    let cancelled = false;
    setMoves(null);
    // A different ticket's moves are a different list; carrying the open state
    // across would put an unread set of buttons under the cursor.
    setMoving(false);
    void api
      .jiraTransitions(focus)
      .then((t) => !cancelled && setMoves(t))
      .catch(() => !cancelled && setMoves([]));
    return () => {
      cancelled = true;
    };
  }, [focus, state]);

  /**
   * The done edge — from either side.
   *
   * Watching the *observed* category rather than only our own transition call
   * means a ticket closed by somebody else in Jira prompts here too, which is
   * the case where the operator is least likely to notice on their own. The
   * `null` start is what stops it firing merely because the app was opened on
   * a session already pointed at a finished ticket.
   */
  useEffect(() => {
    if (!current) {
      wasDone.current = null;
      return;
    }
    const now = isDone(current);
    if (wasDone.current === false && now) setPanel("next");
    wasDone.current = now;
  }, [current]);

  if (state.state === "loading") return <span className="micro">reading the board…</span>;

  if (state.state === "unavailable") {
    return (
      <span className="micro leading-relaxed" style={{ color: "var(--text-soft)" }}>
        {/* Said outright. An empty bar would read as "nothing to do". */}
        SIGN IN TO COWORK TO SEE YOUR BOARD
      </span>
    );
  }

  if (state.state === "error") {
    return (
      <span className="micro" style={{ color: "rgb(var(--primary))" }}>
        {state.message}
      </span>
    );
  }

  const mine = inProject(issues, project);
  const chosen = planIssues(issues, plan);
  // Today's plan wins when there is one: it is a smaller, sharper question than
  // "how is the project going", and it is the one the operator just answered.
  const tracked = chosen.length ? chosen : mine;
  const { done, total } = planProgress(tracked);
  const inFlight = tracked.filter(isInProgress).length;

  const apply = async (transition: JiraTransition) => {
    if (!focus) return;
    setBusy(transition.id);
    setError(null);
    try {
      await api.jiraTransition(focus, transition.id);
      await refresh();
      // Collapse on success: the move is made, and leaving the list open invites
      // a second one nobody meant.
      setMoving(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const choose = (key: string | null) => {
    setFocus(sessionId, key);
    setFocusKey(key ?? undefined);
    // A freshly chosen issue starts a new observation; without this, picking one
    // that is already done would fire the next-up panel straight back at you.
    wasDone.current = null;
    setPanel("none");
  };

  return (
    <div className="flex flex-col gap-[8px]">
      {/* ── the bar ─────────────────────────────────────────────────────── */}
      <div className="flex items-baseline justify-between gap-2">
        <span className="micro">{chosen.length ? "TODAY" : "DONE"}</span>
        <span className="data text-[11px]" style={{ color: "var(--text)" }}>
          {done} / {total}
        </span>
      </div>

      {total > 0 ? (
        <>
          <div
            className="flex h-[8px] w-full overflow-hidden"
            style={{ background: "color-mix(in srgb, var(--text) 10%, transparent)" }}
          >
            {/* Widths animate, the counts above do not — rule 2. */}
            <motion.div
              initial={false}
              animate={{ width: `${(done / total) * 100}%` }}
              transition={{ duration: 0.3, ease: [0.2, 0.9, 0.3, 1] }}
              style={{ background: "var(--text)" }}
            />
            <motion.div
              initial={false}
              animate={{ width: `${(inFlight / total) * 100}%` }}
              transition={{ duration: 0.3, ease: [0.2, 0.9, 0.3, 1] }}
              style={{ background: "rgb(var(--busy))" }}
            />
          </div>
          <div className="flex justify-between gap-2">
            <span className="micro">{inFlight} IN PROGRESS</span>
            <span className="micro" style={{ color: "var(--text-faint)" }}>
              {total - done - inFlight} TO DO
            </span>
          </div>
        </>
      ) : (
        <span className="micro" style={{ color: "var(--text-faint)" }}>
          nothing assigned to you in {project}
        </span>
      )}

      {/* ── what this session is on ─────────────────────────────────────── */}
      <div className="flex flex-col gap-[4px] border-t pt-[6px]" style={{ borderColor: "var(--border)" }}>
        <div className="flex items-baseline justify-between gap-2">
          <span className="micro">WORKING ON</span>
          <button
            type="button"
            className="micro link"
            style={{ color: "var(--text-faint)" }}
            onClick={() => setPanel(panel === "pick" ? "none" : "pick")}
          >
            {current ? "CHANGE" : "PICK"}
          </button>
        </div>

        {current ? (
          <>
            <div className="flex items-baseline gap-2">
              <span className="data shrink-0 text-[11px]" style={{ color: "rgb(var(--busy))" }}>
                {current.key}
              </span>
              <span className="data flex-1 truncate text-[11px]" style={{ color: "var(--text)" }}>
                {current.summary}
              </span>
            </div>
            <div className="flex items-baseline justify-between gap-2">
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                {current.status}
              </span>
              {/* Absent, not disabled, when the workflow offers nothing. */}
              {moves && moves.length > 0 && (
                <button
                  type="button"
                  className="micro link"
                  style={{ color: "var(--text-faint)" }}
                  onClick={() => setMoving((m) => !m)}
                >
                  {moving ? "CANCEL" : `MOVE (${moves.length})`}
                </button>
              )}
            </div>
          </>
        ) : (
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            no ticket chosen for this session
          </span>
        )}

        {/* Only the moves this board actually permits, right now. */}
        {current && moving && moves && moves.length > 0 && (
          <div className="mt-[2px] flex flex-wrap gap-[4px]">
            {moves.map((m) => (
              <button
                key={m.id}
                type="button"
                className="chip"
                disabled={busy !== null}
                onClick={() => void apply(m)}
              >
                {busy === m.id ? "…" : m.name}
              </button>
            ))}
          </div>
        )}
        {current && moves?.length === 0 && (
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            this workflow permits no moves from here
          </span>
        )}
        {error && (
          <span className="micro" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </span>
        )}
      </div>

      {panel !== "none" && (
        <Picker
          heading={
            panel === "next" && current
              ? `${current.key} IS DONE · WHAT NEXT?`
              : "CHOOSE THIS SESSION'S TICKET"
          }
          issues={mine}
          project={project}
          plan={plan}
          current={focus}
          onChoose={choose}
          onTogglePlan={(key) => setPlan(togglePlan(key))}
          onCreated={(issue) => {
            // Straight onto today's list and into this session — you would not
            // have typed it otherwise, and making someone find their own new
            // task in the list to tick it is a step with no decision in it.
            setPlan(togglePlan(issue.key));
            void refresh();
            choose(issue.key);
          }}
          onClose={() => setPanel("none")}
        />
      )}
    </div>
  );
}

/**
 * The list you pick from — also where today's plan is set.
 *
 * One list rather than two screens, because these are the same set of tickets
 * asked two questions. Tapping a row says "I am on this"; the square says
 * "this is for today". Splitting them would mean scrolling the same list twice.
 *
 * Done issues sink to the bottom rather than being hidden: you may still want
 * to reopen one, and a list that silently omits rows makes people hunt.
 */
function Picker({
  heading,
  issues,
  project,
  plan,
  current,
  onChoose,
  onTogglePlan,
  onCreated,
  onClose,
}: {
  heading: string;
  issues: JiraIssue[];
  project: string;
  plan: string[];
  current?: string;
  onChoose: (key: string | null) => void;
  onTogglePlan: (key: string) => void;
  onCreated: (issue: JiraIssue) => void;
  onClose: () => void;
}) {
  const [filter, setFilter] = useState("");
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const create = async () => {
    const summary = name.trim();
    if (!summary || saving) return;
    setSaving(true);
    setFailed(null);
    try {
      const issue = await api.jiraCreate(project, summary);
      setName("");
      setAdding(false);
      onCreated(issue);
    } catch (e) {
      // Kept until the next attempt. A create that failed silently is a task
      // the operator believes exists and never does again.
      setFailed(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const needle = filter.trim().toLowerCase();
  const shown = issues
    .filter((i) => !needle || `${i.key} ${i.summary}`.toLowerCase().includes(needle))
    .slice()
    .sort((a, b) => Number(isDone(a)) - Number(isDone(b)));

  return (
    <div className="flex flex-col gap-[6px] border-t pt-[6px]" style={{ borderColor: "var(--border)" }}>
      <div className="flex items-baseline justify-between gap-2">
        <span className="micro" style={{ color: "var(--text-soft)" }}>
          {heading}
        </span>
        <button type="button" className="micro link" style={{ color: "var(--text-faint)" }} onClick={onClose}>
          CLOSE
        </button>
      </div>

      <input
        className="field"
        placeholder="filter"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
      />

      {/* Bounded height: the rail is a column of instruments, and a list of
          forty tickets would push every widget below it off the screen. */}
      <ul className="flex max-h-[180px] flex-col gap-[2px] overflow-y-auto">
        {shown.map((issue) => (
          <li key={issue.key} className="flex items-start gap-[6px]">
            <button
              type="button"
              title={plan.includes(issue.key) ? "on today's list" : "add to today"}
              aria-pressed={plan.includes(issue.key)}
              onClick={() => onTogglePlan(issue.key)}
              className="mt-[3px] shrink-0"
              style={{
                width: 9,
                height: 9,
                border: "1px solid var(--border)",
                background: plan.includes(issue.key) ? "rgb(var(--busy))" : "transparent",
              }}
            />
            <button
              type="button"
              className="flex min-w-0 flex-1 items-baseline gap-[6px] text-left"
              onClick={() => onChoose(issue.key)}
            >
              <span
                className="data shrink-0 text-[10px]"
                style={{ color: issue.key === current ? "rgb(var(--busy))" : "var(--text-faint)" }}
              >
                {issue.key}
              </span>
              <span
                className="data flex-1 truncate text-[11px]"
                style={{
                  color: isDone(issue) ? "var(--text-faint)" : "var(--text)",
                  textDecoration: isDone(issue) ? "line-through" : "none",
                }}
              >
                {issue.summary}
              </span>
            </button>
          </li>
        ))}
        {!shown.length && (
          <li className="micro" style={{ color: "var(--text-faint)" }}>
            nothing matches
          </li>
        )}
      </ul>

      {/* ── quick add ────────────────────────────────────────────────────
          One field, because one field is the whole point: a name is the only
          thing the operator knows at the moment they think of a task. Assignee
          and sprint are the server's job (`POST /agency/missions`) — it assigns
          to the calling account's own Jira user, so the task appears in the
          list it was created from, and finds the project's active sprint
          itself. Asking here would be two more decisions for no more truth. */}
      {adding ? (
        <div className="flex flex-col gap-[4px]">
          <input
            className="field"
            placeholder={`new task in ${project}`}
            value={name}
            autoFocus
            disabled={saving}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              // Enter submits, Escape backs out — a one-field form where the
              // hands have to leave the keyboard is not a quick add.
              if (e.key === "Enter") {
                e.preventDefault();
                void create();
              } else if (e.key === "Escape") {
                e.preventDefault();
                setAdding(false);
                setFailed(null);
              }
            }}
          />
          <div className="flex items-center gap-[4px]">
            <button type="button" className="chip" disabled={saving || !name.trim()} onClick={() => void create()}>
              {saving ? "ADDING…" : "ADD"}
            </button>
            <button
              type="button"
              className="chip"
              disabled={saving}
              onClick={() => {
                setAdding(false);
                setFailed(null);
              }}
            >
              CANCEL
            </button>
            <span className="micro" style={{ color: "var(--text-faint)" }}>
              assigned to you · current sprint
            </span>
          </div>
          {failed && (
            <span className="micro leading-relaxed" style={{ color: "rgb(var(--primary))" }}>
              {failed}
            </span>
          )}
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-[4px]">
          {current && (
            <button type="button" className="chip" onClick={() => onChoose(null)}>
              CLEAR
            </button>
          )}
          <button type="button" className="chip" onClick={() => setAdding(true)}>
            NEW TASK
          </button>
        </div>
      )}
    </div>
  );
}
