import { useEffect, useState } from "react";
import { motion } from "motion/react";

import { isTauri } from "../../lib/api";
import { inProject, loadIssues, summarise, type IssuesState } from "../../lib/jira";

/**
 * How much of this session's project is done.
 *
 * A stacked bar rather than a donut, for a rail 244px wide: a donut has to be
 * big enough to read three arcs and then still needs its numbers printed beside
 * it, whereas a bar carries the same three quantities at 8px tall and lines up
 * with every other meter here.
 *
 * **Amber for in-progress, chalk for done, faint for the rest — and no red.**
 * Rule 0: red is for something the operator must act on, and a board with open
 * tickets is a board, not an alert.
 */
export function JiraProgress({ project }: { project: string }) {
  const [state, setState] = useState<IssuesState>({ state: "loading" });

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    void loadIssues().then((result) => !cancelled && setState(result));
    return () => {
      cancelled = true;
    };
  }, [project]);

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

  // Only this session's project — an issue key *is* its project prefix.
  const mine = inProject(state.issues, project);
  const { done, inProgress, todo, total } = summarise(mine);
  if (!total) return <span className="micro">nothing assigned to you in {project}</span>;

  const share = (n: number) => `${(n / total) * 100}%`;

  return (
    <div className="flex flex-col gap-[6px]">
      <div className="flex items-baseline justify-between gap-2">
        <span className="micro">DONE</span>
        <span className="data text-[11px]" style={{ color: "var(--text)" }}>
          {done} / {total}
        </span>
      </div>

      <div className="flex h-[8px] w-full overflow-hidden" style={{ background: "rgba(232,230,225,0.10)" }}>
        {/* Widths animate, the counts above do not — rule 2. */}
        <motion.div
          initial={false}
          animate={{ width: share(done) }}
          transition={{ duration: 0.3, ease: [0.2, 0.9, 0.3, 1] }}
          style={{ background: "var(--text)" }}
        />
        <motion.div
          initial={false}
          animate={{ width: share(inProgress) }}
          transition={{ duration: 0.3, ease: [0.2, 0.9, 0.3, 1] }}
          style={{ background: "rgb(var(--busy))" }}
        />
      </div>

      <div className="flex justify-between gap-2">
        <span className="micro">{inProgress} IN PROGRESS</span>
        <span className="micro" style={{ color: "var(--text-faint)" }}>
          {todo} TO DO
        </span>
      </div>
    </div>
  );
}
