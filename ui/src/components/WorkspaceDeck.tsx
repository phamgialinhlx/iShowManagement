import { useMemo } from "react";

import { useWorkspace } from "../lib/workspace";
import { layoutPanes } from "../lib/grid";
import { reattachName, type PaneRef, type SessionV3 } from "../lib/workspace-model";
import { ClaudeSessionPane } from "./ClaudeSessionPane";
import { TerminalView } from "./Terminal";
import { HostPanel } from "./HostPanel";
import { FilesPane } from "./FilesPane";

/**
 * The workbench main area, v3 — a **pane manager** (ADR-002).
 *
 * The grid tiles Panes, and a Pane holds a *content source*: a session (a Claude
 * TUI or a shell), a Server's host panel, or a Project's files. Replaces
 * `SessionDeck`, whose cells could only hold sessions.
 *
 * Focus mode (1×1) shows a single tile; 2×2 / 3×3 / 4×4 tile several. Clicking a
 * tile focuses it, so the rail's "open into the focused tile" verb has a target.
 */

function paneKey(ref: PaneRef | null, index: number): string {
  if (!ref) return `empty-${index}`;
  if (ref.kind === "session") return `session:${ref.id}`;
  if (ref.kind === "host") return `host:${ref.serverId}`;
  return `files:${ref.projectId}`;
}

/** A single session pane — Claude (with its sub-tabs) or a shell, by kind. */
function SessionPane({ session }: { session: SessionV3 }) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const live = useWorkspace((s) => s.live[session.id]);
  const setLive = useWorkspace((s) => s.setLive);
  const clearLive = useWorkspace((s) => s.clearLive);

  if (session.kind === "claude") {
    return <ClaudeSessionPane session={session} />;
  }

  return (
    <TerminalView
      target={target}
      cwd={project?.folder}
      session={reattachName(session)}
      ptyId={live}
      onOpened={(id) => setLive(session.id, id)}
      onExit={() => clearLive(session.id)}
    />
  );
}

/** A server's host pane — processes, ports, metrics. */
function HostPane({ serverId }: { serverId: string }) {
  const server = useWorkspace((s) => s.servers.find((sv) => sv.id === serverId));
  if (!server) return <PaneMissing what="server" />;
  return <HostPanel target={server.target} />;
}

function PaneMissing({ what }: { what: string }) {
  return (
    <div className="grid h-full place-items-center">
      <span className="micro" style={{ color: "var(--text-faint)" }}>
        {what} no longer exists
      </span>
    </div>
  );
}

function Pane({ pane }: { pane: PaneRef }) {
  const session = useWorkspace((s) =>
    pane.kind === "session" ? s.sessions.find((x) => x.id === pane.id) : undefined,
  );
  if (pane.kind === "session") {
    return session ? <SessionPane session={session} /> : <PaneMissing what="session" />;
  }
  if (pane.kind === "host") return <HostPane serverId={pane.serverId} />;
  return <FilesPane projectId={pane.projectId} />;
}

export function WorkspaceDeck() {
  const sessions = useWorkspace((s) => s.sessions);
  const panes = useWorkspace((s) => s.panes);
  const active = useWorkspace((s) => s.activeSession);
  const grid = useWorkspace((s) => s.grid);
  const focusedCell = useWorkspace((s) => s.focusedCell);
  const focusCell = useWorkspace((s) => s.focusCell);
  const activate = useWorkspace((s) => s.activate);

  const cells = useMemo(
    () => layoutPanes(panes, sessions, active, grid),
    [panes, sessions, active, grid],
  );

  if (grid >= 2) {
    return (
      <div
        className="grid min-h-0 flex-1 gap-[1px]"
        style={{
          gridTemplateColumns: `repeat(${grid}, minmax(0, 1fr))`,
          gridAutoRows: "minmax(0, 1fr)",
        }}
      >
        {cells.map((pane, index) => (
          <div
            key={paneKey(pane, index)}
            className="panel relative flex min-h-0 flex-col overflow-hidden"
            onMouseDownCapture={() => {
              focusCell(index);
              if (pane?.kind === "session") activate(pane.id);
            }}
            style={{
              outline:
                focusedCell === index
                  ? "2px solid var(--text)"
                  : pane?.kind === "session" && pane.id === active
                    ? "1px solid color-mix(in srgb, var(--text) 45%, transparent)"
                    : "1px solid transparent",
              outlineOffset: -1,
              background: pane ? undefined : "var(--app-bg)",
            }}
          >
            {pane ? (
              <Pane pane={pane} />
            ) : (
              <div className="grid h-full place-items-center">
                <span className="micro" style={{ color: "var(--text-faint)" }}>
                  EMPTY — CLICK A NODE IN THE RAIL TO FILL
                </span>
              </div>
            )}
          </div>
        ))}
      </div>
    );
  }

  // Focus mode: the single effective pane for cell 0. Remounting on switch
  // reattaches (scrollback replayed), so nothing is lost. TODO: a warm set (as
  // the old deck kept) to make focus-mode switches instant rather than a reattach.
  const only = cells[0] ?? null;
  return (
    <div className="flex min-h-0 flex-1">
      {only ? (
        <Pane pane={only} />
      ) : (
        <div className="grid h-full flex-1 place-items-center">
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            nothing open — pick a session, or connect a server
          </span>
        </div>
      )}
    </div>
  );
}
