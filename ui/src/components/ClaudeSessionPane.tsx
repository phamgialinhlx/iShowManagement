import { useState } from "react";

import { useWorkspace } from "../lib/workspace";
import type { SessionV3 } from "../lib/workspace-model";
import { ClaudePanel } from "./ClaudePanel";
import { TranscriptView } from "./TranscriptView";
import { JiraPanel } from "./JiraPanel";

/**
 * A Claude session pane: the live TUI, with **Transcript** and **Jira** as
 * sub-tabs (ADR-002 — they ride next to the conversation they belong to, not as
 * their own grid panes).
 *
 * The Claude TUI stays **mounted** across tab switches (hidden with `display`),
 * because unmounting it would tear down xterm and reattach on every glance at
 * the transcript — losing scrollback and costing a replay. Transcript and Jira
 * mount on demand and unmount when hidden, so their polling stops when they are
 * not on screen (the "a widget switched off must not run" rule).
 */
type Tab = "claude" | "transcript" | "jira";

export function ClaudeSessionPane({ session }: { session: SessionV3 }) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const folder = project?.folder ?? "";
  const [tab, setTab] = useState<Tab>("claude");

  const hasJira = !!session.jiraProject;
  // A saved Jira tab that no longer applies falls back to Claude.
  const active: Tab = tab === "jira" && !hasJira ? "claude" : tab;

  const tabs: { id: Tab; label: string }[] = [
    { id: "claude", label: "CLAUDE" },
    { id: "transcript", label: "TRANSCRIPT" },
    ...(hasJira ? [{ id: "jira" as Tab, label: `JIRA · ${session.jiraProject}` }] : []),
  ];

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className="flex shrink-0 items-center gap-1 border-b px-2 py-[3px]"
        style={{ borderColor: "var(--border)" }}
      >
        {tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            onClick={() => setTab(t.id)}
            aria-pressed={active === t.id}
            className="data px-2 py-[2px] text-[10px]"
            style={{
              color: active === t.id ? "var(--text)" : "var(--text-soft)",
              boxShadow: active === t.id ? "inset 0 -1px 0 var(--text)" : "none",
              letterSpacing: "0.06em",
            }}
          >
            {t.label}
          </button>
        ))}
      </div>

      <div className="relative min-h-0 flex-1">
        {/* Always mounted — hidden, never torn down. */}
        <div className="h-full" style={{ display: active === "claude" ? "block" : "none" }}>
          <ClaudePanel
            sessionId={session.id}
            target={target}
            cwd={folder}
            resume={session.resume}
            fullscreen={session.fullscreen}
            skipPermissions={session.skipPermissions}
            modelProfile={session.modelProfile}
          />
        </div>

        {active === "transcript" && (
          <div className="h-full">
            <TranscriptView target={target} folder={folder} resume={session.resume} />
          </div>
        )}

        {active === "jira" && hasJira && (
          <div className="h-full">
            <JiraPanel project={session.jiraProject!} />
          </div>
        )}
      </div>
    </div>
  );
}
