import { useMemo, useState } from "react";
import { invoke } from "../lib/bridge";

import { useWorkspace } from "../lib/workspace";
import { isFocusDeck, layoutPanes } from "../lib/grid";
import { basename, reattachName, type PaneRef, type SessionV3 } from "../lib/workspace-model";
import { ClaudeSessionPane } from "./ClaudeSessionPane";
import { PiSessionPane } from "./PiSessionPane";
import { TerminalView } from "./Terminal";
import { HostPanel } from "./HostPanel";
import { FilesPane } from "./FilesPane";
import { PaneHeader } from "./PaneHeader";
import { SessionSettings } from "./SessionSettings";
import { BackView, GEAR_PATH, Icon, IconButton, RELOAD_PATH } from "./SessionPaneChrome";

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

/**
 * A plain shell pane — the terminal, plus the one sub-view it has.
 *
 * **Settings is not a Claude luxury.** TRANSPORT is a property of the
 * connection, and `terminal_open` has carried `low_latency` from the start — but
 * the only way to reach the control was the Claude pane's gear, so a shell on a
 * 271ms link could not be switched to the low-latency transport at all. That is
 * backwards: a shell is precisely where each keystroke's round trip is felt.
 * `SessionSettings` renders only the sections that apply to the kind, so what a
 * shell gets here is TRANSPORT and nothing Claude-shaped.
 *
 * **The terminal is hidden, never unmounted.** The PTY outlives this component,
 * but tearing the view down on every glance at the settings would detach and
 * replay the scrollback each time; `display: none` is what the Claude and pi
 * panes already do with their TUIs, and it is what this follows. Coming back is
 * handled by `TerminalView`'s own `ResizeObserver`, which bails while the host
 * measures 0x0 and fires again with the real size the moment it is shown — so
 * the stale cell grid is re-fitted with no second mechanism invented here.
 */
function ShellSessionPane({ session }: { session: SessionV3 }) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const live = useWorkspace((s) => s.live[session.id]);
  const setLive = useWorkspace((s) => s.setLive);
  const clearLive = useWorkspace((s) => s.clearLive);
  // Ephemeral and local to the pane, like the agent panes' view: persisting it
  // would reopen someone into a settings screen they closed days ago, and a
  // shared one would switch all sixteen tiles at once.
  const [view, setView] = useState<"shell" | "settings">("shell");

  /**
   * RELOAD — force a reattach when the pane has gone quiet.
   *
   * `TerminalView` recovers on its own only when the `ssh` attach *exits*, and
   * the keepalive gives that 90 seconds (`ServerAliveInterval=15` x
   * `ServerAliveCountMax=6`). On a 271ms bufferbloated link a stall inside that
   * window is a pane fed by a stream that stopped: nothing arrives, typing goes
   * nowhere, and before this the only way out was closing the pane — which sends
   * `terminal_close`, and `terminal_close` carries an `agent kill`. That is the
   * difference this control exists to avoid.
   *
   * Three steps, and the order is the whole correctness of it:
   *
   * 1. **Let go of the local handle.** `live[session.id]` names an attachment
   *    inside the connection that just stalled; asking to reattach to it yields
   *    "no such session" forever, which is the shape of a pane that never comes
   *    back. It is also what makes the next attach take the *named* path.
   * 2. **`terminal_detach`, never `terminal_close`.** It takes neither a target
   *    nor a session name — there is nothing to say to the far side — so it has
   *    no path that can end a shell. Skipping it would abandon the wedged `ssh`
   *    child and leave one behind per press.
   * 3. **Bump the generation**, which rebuilds the terminal and attaches under
   *    `reattachName(session)` — the one name the daemon already holds. (Deriving
   *    a different one would *create* a second shell beside the one being
   *    adopted, with nothing pointing at the original.)
   *
   * Guarded by state rather than a ref, unlike the Claude pane's: there is no
   * poll re-reading a stale closure here, only a pointer, and the button is
   * `disabled` for as long as the flag is set — so a double-click cannot fire two
   * reattaches.
   */
  const [generation, setGeneration] = useState(0);
  const [reattaching, setReattaching] = useState(false);
  const reload = () => {
    if (reattaching) return;
    setReattaching(true);
    const stale = live;
    clearLive(session.id);
    if (stale) void invoke("terminal_detach", { id: stale }).catch(() => {});
    setGeneration((n) => n + 1);
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* A shell says nothing about which session or machine it is — the prompt
          is the *host's* idea of that, and in a grid of four they all look
          alike. Folder first, session name second — the same order as the
          Claude pane's header, so a grid of mixed panes reads as one thing.

          Dropped while the settings are open: `BackView` brings its own bar,
          and two bars stacked is the double-header the agent panes exist to
          avoid. */}
      {view === "shell" && (
        <PaneHeader
          label={project ? basename(project.folder) : session.name}
          detail={project ? session.name : undefined}
          actions={
            <>
              <IconButton label="Session settings" onClick={() => setView("settings")}>
                <Icon d={GEAR_PATH} />
              </IconButton>
              <IconButton
                label={
                  reattaching
                    ? "Reattaching…"
                    : `Reattach this session — reconnects to ${target.host ?? "this machine"} without ending your work`
                }
                disabled={reattaching}
                onClick={reload}
              >
                <Icon d={RELOAD_PATH} />
              </IconButton>
            </>
          }
        />
      )}
      <div className="relative min-h-0 flex-1">
        <div className="h-full" style={{ display: view === "shell" ? "block" : "none" }}>
          <TerminalView
            target={target}
            // Adopted sessions carry no folder of ours: the synthetic project
            // that holds them is a container, not a directory anyone chose.
            cwd={session.hostName ? undefined : project?.folder}
            session={reattachName(session)}
            sessionId={session.id}
            lowLatency={session.lowLatency}
            ptyId={live}
            generation={generation}
            onOpened={(id) => {
              setLive(session.id, id);
              // The attach landed, so the control is live again. Cleared on the
              // *handle*, not on a timer: a reattach over a 271ms link takes as
              // long as it takes, and a button that re-enables itself early
              // invites a second one on top of the first.
              setReattaching(false);
            }}
            onAttachFailed={() => setReattaching(false)}
            onExit={() => clearLive(session.id)}
          />
        </div>

        {view === "settings" && (
          <BackView label="SETTINGS" backLabel="Back to the terminal" onBack={() => setView("shell")}>
            <SessionSettings sessionId={session.id} />
          </BackView>
        )}
      </div>
    </div>
  );
}

/** A single session pane — Claude, pi or a shell, by kind. */
function SessionPane({ session }: { session: SessionV3 }) {
  if (session.kind === "claude") return <ClaudeSessionPane session={session} />;
  if (session.kind === "pi") return <PiSessionPane session={session} />;
  return <ShellSessionPane session={session} />;
}

/** A server's host pane — processes, ports, metrics. */
function HostPane({ serverId }: { serverId: string }) {
  const server = useWorkspace((s) => s.servers.find((sv) => sv.id === serverId));
  if (!server) return <PaneMissing what="server" />;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <PaneHeader label={serverId} detail="host" />
      <div className="min-h-0 flex-1">
        <HostPanel target={server.target} serverId={server.id} />
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
