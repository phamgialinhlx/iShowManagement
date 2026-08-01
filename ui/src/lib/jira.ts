import { api, isTauri, type JiraIssue } from "./api";

/**
 * The session's Jira issues.
 *
 * Two things here are deliberate.
 *
 * **Progress is counted from `statusCategory`, not from the status name.** Jira
 * workflow statuses are per-project and renameable — "Done", "Closed",
 * "Shipped", "Ready for QA" are all real and all mean different things in
 * different projects. The category is Jira's own three-bucket grouping
 * (`new` / `indeterminate` / `done`) and is the only part safe to reason about.
 * Matching on names would quietly report the wrong number on any board whose
 * workflow was customised, which is most of them.
 *
 * **These are the issues assigned to *you*, not a whole project.** That is what
 * the server offers session-independently (`/agency/missions`) and it is also
 * the more useful default for a rail widget: what a session wants on screen is
 * my work, not the org's backlog.
 */

export type Progress = {
  done: number;
  inProgress: number;
  todo: number;
  total: number;
};

export function summarise(issues: JiraIssue[]): Progress {
  let done = 0;
  let inProgress = 0;
  for (const issue of issues) {
    const category = (issue.statusCategory ?? "").toLowerCase();
    if (category === "done") done += 1;
    else if (category === "inprogress") inProgress += 1;
  }
  return { done, inProgress, todo: issues.length - done - inProgress, total: issues.length };
}

export type IssuesState =
  | { state: "loading" }
  | { state: "ready"; issues: JiraIssue[] }
  /** Not signed in, or the server has no Jira profile configured at all. */
  | { state: "unavailable" }
  | { state: "error"; message: string };

/**
 * Tell "there is nothing to talk to" apart from "something went wrong".
 *
 * Signing in is optional in rmux, so "sign in first" is an ordinary state and
 * not an error worth painting red. A server with no Jira profile is the same
 * kind of answer.
 */
export function classify(error: unknown): IssuesState {
  const message = error instanceof Error ? error.message : String(error);
  if (/sign in|no jira profile/i.test(message)) return { state: "unavailable" };
  return { state: "error", message };
}

/** Issues assigned to the signed-in account. */
export async function loadIssues(): Promise<IssuesState> {
  if (!isTauri()) return { state: "unavailable" };
  try {
    return { state: "ready", issues: await api.jiraMissions() };
  } catch (e) {
    return classify(e);
  }
}

/**
 * Only the ones in this session's project.
 *
 * Filtered on the key prefix, because that is what a Jira issue key *is* —
 * `RMX-42` belongs to `RMX`. Done here rather than server-side because the
 * route returns the account's whole assignment list; filtering it is cheaper
 * than a route that does not exist.
 */
export function inProject(issues: JiraIssue[], project: string): JiraIssue[] {
  if (!project) return issues;
  const prefix = `${project.toUpperCase()}-`;
  return issues.filter((i) => i.key.toUpperCase().startsWith(prefix));
}
