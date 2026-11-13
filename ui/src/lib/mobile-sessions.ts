/**
 * The mobile session list: every agent session grouped by server, then project,
 * with terminals as a secondary group under each project.
 *
 * The state of each row is `agent-state.ts`'s `agentState` — the one mapping
 * from runtime to herdr's five states — and every roll-up is its `rollup`, so the
 * phone cannot disagree with the desktop rail about what needs attention.
 *
 * DeepSeek Harness (`dsh`) sessions are left out: their pane is a native child
 * webview the browser cannot host, and a row that opens nothing is a control that
 * cannot work.
 *
 *     node --experimental-strip-types ui/mobile-check.ts
 */
import { agentState, needsYou, rollup, type AgentState, type RuntimeLike } from "./agent-state.ts";

type TargetLike = { host?: string; user?: string; port?: number };
export type ServerLike = { id: string; target: TargetLike };
export type ProjectLike = { id: string; serverId: string; folder: string; label: string; running?: boolean };
export type SessionLike = { id: string; projectId: string; kind: string; name: string };

export type Agent = "claude" | "pi" | "codex";

export type Row = {
  id: string;
  name: string;
  kind: string;
  /** `null` for a terminal — it is not an agent and carries no state. */
  state: AgentState | null;
  lastStatusAt?: number;
};

export type ProjectGroup = { id: string; label: string; folder: string; agents: Row[]; terminals: Row[]; state: AgentState | null };
export type ServerGroup = { id: string; label: string; projects: ProjectGroup[]; state: AgentState | null; needsYou: number };

/** How a server is named on a phone: its ssh alias (never a resolved address). */
export function serverLabel(target: TargetLike): string {
  if (!target.host) return "this host";
  const who = target.user ? `${target.user}@${target.host}` : target.host;
  return target.port ? `${who}:${target.port}` : who;
}

export function isChatKind(kind: string): kind is Agent {
  return kind === "claude" || kind === "pi" || kind === "codex";
}

const SHOWN = new Set(["claude", "pi", "codex", "terminal"]);

export function groupSessions(input: {
  servers: readonly ServerLike[];
  projects: readonly ProjectLike[];
  sessions: readonly SessionLike[];
  runtime: Record<string, RuntimeLike | undefined>;
  seen: Record<string, number | undefined>;
}): ServerGroup[] {
  const out: ServerGroup[] = [];
  for (const server of input.servers) {
    const projects: ProjectGroup[] = [];
    for (const project of input.projects) {
      if (project.serverId !== server.id) continue;
      const agents: Row[] = [];
      const terminals: Row[] = [];
      for (const s of input.sessions) {
        if (s.projectId !== project.id || !SHOWN.has(s.kind)) continue;
        const runtime = input.runtime[s.id];
        const row: Row = {
          id: s.id,
          name: s.name,
          kind: s.kind,
          state: agentState(s.kind, runtime, input.seen[s.id]),
          lastStatusAt: runtime?.lastStatusAt,
        };
        (isChatKind(s.kind) ? agents : terminals).push(row);
      }
      if (!agents.length && !terminals.length) continue;
      projects.push({
        id: project.id,
        label: project.running ? "running on the host" : project.label,
        folder: project.folder,
        agents,
        terminals,
        state: rollup(agents.map((a) => a.state)),
      });
    }
    if (!projects.length) continue;
    const rows = projects.flatMap((p) => p.agents);
    out.push({
      id: server.id,
      label: serverLabel(server.target),
      projects,
      state: rollup(projects.map((p) => p.state)),
      needsYou: rows.filter((r) => needsYou(r.state)).length,
    });
  }
  return out;
}

/** The number of sessions waiting on the operator, across every server. */
export function totalNeedsYou(groups: readonly ServerGroup[]): number {
  return groups.reduce((n, g) => n + g.needsYou, 0);
}
