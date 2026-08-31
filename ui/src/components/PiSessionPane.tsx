import { useEffect, useRef, useState } from "react";

import { useWorkspace } from "../lib/workspace";
import { VIEW_EVENT } from "../lib/shortcuts";
import { reattachName, type SessionV3 } from "../lib/workspace-model";
import { ClaudePanel } from "./ClaudePanel";
import { TranscriptView } from "./TranscriptView";
import { FilesPane } from "./FilesPane";
import { GitPane } from "./GitPane";
import { TerminalView } from "./Terminal";
import {
  companionKey,
  companionName,
  readOpen,
  readSplit,
  splitFromPointer,
  writeOpen,
  writeSplit,
} from "../lib/companion";
import {
  Icon,
  IconButton,
  BackView,
  TRANSCRIPT_PATH,
  FOLDER_PATH,
  SPLIT_PATH,
  GIT_PATH,
} from "./SessionPaneChrome";

/**
 * A pi session pane — the live pi TUI, rendered as an interactive xterm.
 *
 * It reuses `ClaudePanel` with `provider="pi"` deliberately: everything that
 * makes that panel a good terminal — the OSC 52 clipboard bridge, the
 * alternate-screen scrollbar toggle, `WebglAddon`, the font remeasure, the
 * focus-restore-on-click and IME handling — is provider-blind, so pi inherits
 * all of it for free. What `ClaudePanel` forks on `provider` is only the two
 * things that genuinely differ: a fresh start goes through `pi_start` (which
 * has no fullscreen / skip-permissions / model-profile), and the Claude-only
 * status/transcript polls are skipped because pi exposes neither.
 *
 * The sub-views mirror the Claude pane's, minus the ones pi has no notion of
 * (Jira, model, permissions, session settings): **transcript, files, git** and
 * the **companion terminal split**. Files and git are agent-agnostic and reuse
 * the same components verbatim; the transcript reads through `pi_transcript`
 * via `TranscriptView`'s provider seam. The chrome (icons, back bar) is the
 * shared `SessionPaneChrome`, so the two panes cannot drift.
 */
type View = "pi" | "transcript" | "files" | "git";

export function PiSessionPane({ session }: { session: SessionV3 }) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const folder = project?.folder ?? "";
  const [view, setView] = useState<View>("pi");

  /**
   * The shell that belongs to this conversation — kept per session and
   * remembered, exactly as the Claude pane does it.
   */
  const [companion, setCompanion] = useState(() => readOpen(session.id));
  const [split, setSplit] = useState(() => readSplit(session.id));
  const [dragging, setDragging] = useState(false);
  const tileRef = useRef<HTMLDivElement>(null);
  const live = useWorkspace((s) => s.live[companionKey(session.id)]);
  const setLive = useWorkspace((s) => s.setLive);
  const clearLive = useWorkspace((s) => s.clearLive);

  useEffect(() => {
    if (!dragging) return;
    const el = tileRef.current;
    if (!el) return;
    // Measured rather than read from `--ui-zoom`: `getBoundingClientRect` is in
    // viewport pixels while the layout is in the zoomed space, and the ratio is
    // the only thing that survives both.
    const move = (e: MouseEvent) => {
      const r = el.getBoundingClientRect();
      setSplit(splitFromPointer(e.clientY, r.top, r.height));
    };
    const up = () => setDragging(false);
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
  }, [dragging]);

  useEffect(() => writeSplit(session.id, split), [session.id, split]);

  /**
   * A keyboard shortcut asking this pane to change view. The view stays *here*
   * rather than in the store — it is ephemeral, and persisting it would restore
   * someone into a transcript they closed days ago. Addressed by session id, so
   * a chord does not switch every tile in a 4×4 at once.
   */
  useEffect(() => {
    const onView = (e: Event) => {
      const detail = (e as CustomEvent<{ sessionId: string; view: View }>).detail;
      if (detail?.sessionId === session.id) setView(detail.view);
    };
    window.addEventListener(VIEW_EVENT, onView);
    return () => window.removeEventListener(VIEW_EVENT, onView);
  }, [session.id]);

  // A view whose target no longer applies falls back to the conversation.
  const active: View = (view === "files" || view === "git") && !project ? "pi" : view;

  // Entry points living on pi's status line: transcript, this project's files,
  // git, and the companion shell. Each sub-view opens with a `←` back to pi.
  const headerActions = (
    <>
      <IconButton label="Open the transcript" onClick={() => setView("transcript")}>
        <Icon d={TRANSCRIPT_PATH} />
      </IconButton>
      {project && (
        <IconButton label={`Open files · ${folder}`} onClick={() => setView("files")}>
          <Icon d={FOLDER_PATH} />
        </IconButton>
      )}
      <IconButton
        label={companion ? "Hide this session's terminal" : "Open a terminal beside this session"}
        onClick={() => {
          const next = !companion;
          setCompanion(next);
          writeOpen(session.id, next);
        }}
      >
        <Icon d={SPLIT_PATH} />
      </IconButton>
      {project && (
        <IconButton label="Git — what changed" onClick={() => setView("git")}>
          <Icon d={GIT_PATH} />
        </IconButton>
      )}
    </>
  );

  // Only alongside the conversation. Transcript, files and git are full-pane
  // reading views — a shell wedged under one would take height from the thing
  // you opened, to show something you did not ask for.
  const splitting = companion && active === "pi";

  return (
    <div ref={tileRef} className="flex h-full min-h-0 flex-col">
      <div
        className="relative min-h-0"
        // A basis rather than a height, so the terminal below keeps its own
        // minimum and neither half can be squeezed out of existence.
        style={splitting ? { flex: `0 0 ${split * 100}%` } : { flex: "1 1 0%" }}
      >
        {/* Always mounted — hidden, never torn down. Its status line carries the
            transcript / files / git icons, so this is the only header in pi view. */}
        <div className="h-full" style={{ display: active === "pi" ? "block" : "none" }}>
          <ClaudePanel
            provider="pi"
            sessionId={session.id}
            target={target}
            // An adopted session's folder belongs to whoever started it, and the
            // synthetic project holding it has none worth passing. A *resumed* pi
            // session carries its own `cwd` (pi locates its sessions under a
            // cwd-encoded dir); a fresh one falls back to the project folder.
            cwd={session.hostName ? undefined : (session.cwd ?? folder)}
            agentSession={reattachName(session)}
            resume={session.resume}
            headerActions={headerActions}
          />
        </div>

        {active === "transcript" && (
          <BackView label="TRANSCRIPT" onBack={() => setView("pi")}>
            <TranscriptView
              sessionId={session.id}
              target={target}
              // pi uses the conversation's own cwd as the transcript directory.
              folder={session.cwd ?? folder}
              resume={session.resume}
              provider="pi"
            />
          </BackView>
        )}

        {active === "files" && project && (
          <BackView label={`FILES · ${folder}`} onBack={() => setView("pi")}>
            <FilesPane projectId={project.id} />
          </BackView>
        )}

        {active === "git" && project && (
          <BackView label={`GIT · ${folder}`} onBack={() => setView("pi")}>
            <GitPane projectId={project.id} />
          </BackView>
        )}
      </div>

      {splitting && (
        <>
          {/* 5px of hit area over a 1px line — a divider you cannot grab reads
              as a fixed layout. */}
          <div
            role="separator"
            aria-orientation="horizontal"
            onMouseDown={(e) => {
              e.preventDefault();
              setDragging(true);
            }}
            className="relative shrink-0"
            style={{ height: 5, cursor: "row-resize", background: "var(--border)" }}
            title="Drag to resize"
          />
          <div className="min-h-0 flex-1">
            <TerminalView
              target={target}
              cwd={folder}
              // Derived from the session id, so the same shell is reattached
              // after a restart rather than a second one being spawned.
              session={companionName(session.id)}
              sessionId={session.id}
              // Shares the conversation's id, so it would answer the same focus
              // request. Switching to the pi pane means the conversation, not
              // the shell beside it.
              answersFocusRequests={false}
              ptyId={live}
              onOpened={(id) => setLive(companionKey(session.id), id)}
              onExit={() => clearLive(companionKey(session.id))}
            />
          </div>
        </>
      )}
    </div>
  );
}
