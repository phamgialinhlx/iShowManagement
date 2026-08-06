import { useMemo } from "react";

import { useWorkspace } from "../lib/workspace";
import { isFocusDeck, layoutPanes } from "../lib/grid";
import { basename, reattachName, type PaneRef, type SessionV3 } from "../lib/workspace-model";
import { ClaudeSessionPane } from "./ClaudeSessionPane";
import { TerminalView } from "./Terminal";
import { HostPanel } from "./HostPanel";
import { FilesPane } from "./FilesPane";
import { PaneHeader } from "./PaneHeader";

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

  // A shell says nothing about which session or machine it is — the prompt is
  // the *host's* idea of that, and in a grid of four they all look alike. The
  // header is the only thing that answers it.
  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* Folder first, session name second — the same order as the Claude pane's
          header, so a grid of mixed panes reads as one thing. */}
      <PaneHeader
        label={project ? basename(project.folder) : session.name}
        detail={project ? session.name : undefined}
      />
      <div className="min-h-0 flex-1">
        <TerminalView
          target={target}
          cwd={session.hostName ? undefined : project?.folder}
          session={session.hostName ?? reattachName(session)}
          sessionId={session.id}
          ptyId={live}
          onOpened={(id) => setLive(session.id, id)}
          onExit={() => clearLive(session.id)}
        />
      </div>
    </div>
  );
}

/** A server's host pane — processes, ports, metrics. */
function HostPane({ serverId }: { serverId: string }) {
  const server = useWorkspace((s) => s.servers.find((sv) => sv.id === serverId));
  if (!server) return <PaneMissing what="server" />;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <PaneHeader label={serverId} detail="host" />
      <div className="min-h-0 flex-1">
        <HostPanel target={server.target} />
      </div>
    </div>
  );
}

/** A project's files. The tab strip names *files*; this names the project. */
function FilesPaneTile({ projectId }: { projectId: string }) {
  const project = useWorkspace((s) => s.projects.find((p) => p.id === projectId));
  return (
    <div className="flex h-full min-h-0 flex-col">
      <PaneHeader label={project?.label ?? "files"} detail={project?.folder} />
      <div className="min-h-0 flex-1">
        <FilesPane projectId={projectId} />
      </div>
    </div>
  );
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
  return <FilesPaneTile projectId={pane.projectId} />;
}

export function WorkspaceDeck() {
  const sessions = useWorkspace((s) => s.sessions);
  const panes = useWorkspace((s) => s.panes);
  const active = useWorkspace((s) => s.activeSession);
  const deck = useWorkspace((s) => s.deck);
  const focusedCell = useWorkspace((s) => s.focusedCell);
  const focusCell = useWorkspace((s) => s.focusCell);
  const activate = useWorkspace((s) => s.activate);

  const cells = useMemo(
    () => layoutPanes(panes, sessions, active, deck),
    [panes, sessions, active, deck],
  );

  if (!isFocusDeck(deck)) {
    return (
      <div
        className="grid min-h-0 flex-1 gap-[1px]"
        style={{
          // Rows are stated explicitly rather than left to `gridAutoRows`.
          // A wide deck (1x2, 1x4) is one row of full-height panes, and letting
          // the browser infer rows from the child count would give a second row
          // the moment a cell wrapped — which is precisely the tall, short
          // terminal the wide decks exist to avoid.
          gridTemplateColumns: `repeat(${deck.cols}, minmax(0, 1fr))`,
          gridTemplateRows: `repeat(${deck.rows}, minmax(0, 1fr))`,
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
              // **An empty cell is a panel, not a hole.** This carried
              // `background: var(--app-bg)` — the app's *opaque* base — while
              // every filled cell is a translucent `.panel`. Over the desktop,
              // or under native glass, that is a solid black rectangle in the
              // middle of a translucent window, and it reads as exactly the
              // rendering fault it looks like. The `.panel` class on this
              // element already paints it, so there is nothing to substitute:
              // dropping the override lets it track the tint, the glass overlay
              // and everything else the appearance settings move.
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

  // Focus mode: the single effective pane for cell 0. The `key` forces a
  // remount when the pane changes — without it, switching between two Claude
  // sessions updates in place and the xterm stays bound to the first one (the
  // pane looked "stuck"). Remounting reattaches (scrollback replayed), so
  // nothing is lost. The wrapper carries `flex-1` because this parent is a flex
  // *row*: without it the pane sizes to its content width, not the deck.
  // TODO: a warm set (as the old deck kept) to make switches instant.
  const only = cells[0] ?? null;
  return (
    <div className="flex min-h-0 flex-1">
      {only ? (
        <div
          key={paneKey(only, 0)}
          className="relative flex min-h-0 flex-1 flex-col overflow-hidden"
        >
          <Pane pane={only} />
        </div>
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
