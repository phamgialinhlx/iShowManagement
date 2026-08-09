import { useEffect, useRef, useState } from "react";

import { useWorkspace } from "../lib/workspace";
import { VIEW_EVENT } from "../lib/shortcuts";
import type { SessionV3 } from "../lib/workspace-model";
import { ClaudePanel } from "./ClaudePanel";
import { TranscriptView } from "./TranscriptView";
import { JiraPanel } from "./JiraPanel";
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
import { SessionSettings } from "./SessionSettings";

/**
 * A Claude session pane: the live TUI is the pane. **Transcript** and **Jira**
 * are not sibling tabs stacked above it — that cost a second header row. They
 * are icons on Claude's own status line (the "bottom line"), and opening one
 * swaps the body to a view with a `←` back to the conversation (ADR-002 — they
 * ride next to the conversation they belong to, not as their own grid panes).
 *
 * The Claude TUI stays **mounted** across the switch (hidden with `display`),
 * because unmounting it would tear down xterm and reattach on every glance at
 * the transcript — losing scrollback and costing a replay. Transcript and Jira
 * mount on demand and unmount when hidden, so their polling stops when they are
 * not on screen (the "a widget switched off must not run" rule).
 */
type View = "claude" | "transcript" | "jira" | "files" | "git" | "settings";

/** Rule 3: inline SVG, Lucide-style, square caps — no glyph font, no emoji. */
function Icon({ d, size = 14 }: { d: string; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="square"
      strokeLinejoin="miter"
      aria-hidden="true"
    >
      <path d={d} />
    </svg>
  );
}

/** A header icon button — soft at rest, full on hover (rule: legible controls). */
function IconButton({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className="flex shrink-0 items-center gap-1 px-1 opacity-70 hover:opacity-100"
      style={{ color: "var(--text-soft)" }}
    >
      {children}
    </button>
  );
}

const TRANSCRIPT_PATH = "M4 6h16M4 10h16M4 14h10M4 18h7"; // stacked lines = a transcript
const FOLDER_PATH = "M3 6v13h18V8h-9l-2-2H3z"; // folder = this project's files
const BOARD_PATH = "M4 4h16v16H4zM10 4v16M16 4v16"; // kanban columns = Jira
const SPLIT_PATH = "M4 4h16v16H4zM4 13h16"; // a pane divided = the companion shell
const GIT_PATH = "M7 4v16M7 9h6a4 4 0 014 4v3M17 4v3"; // a branch leaving the trunk
const GEAR_PATH = "M4 8h16M9 6v4M4 16h16M15 14v4"; // sliders = session settings
const BACK_PATH = "M15 5l-7 7 7 7"; // ← return to the conversation

export function ClaudeSessionPane({ session }: { session: SessionV3 }) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const folder = project?.folder ?? "";
  const [view, setView] = useState<View>("claude");

  /**
   * The shell that belongs to this conversation.
   *
   * Kept per session and remembered, because whether you work with a terminal
   * open is a habit rather than a per-visit decision — and switching sessions
   * must bring *that* session's shell, which is the whole point.
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
    // the only thing that survives both. Same reason the file tree's splitter
    // measures instead of assuming.
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
   * A keyboard shortcut asking this pane to change view.
   *
   * The view stays *here* rather than in the store: it is ephemeral, and
   * persisting it would restore someone into a transcript they closed days ago.
   * So the shortcut is a request addressed by session id, and a pane that is
   * not the addressee ignores it — which is also what stops a chord changing
   * the view of every tile in a 4×4 at once.
   */
  useEffect(() => {
    const onView = (e: Event) => {
      const detail = (e as CustomEvent<{ sessionId: string; view: View }>).detail;
      if (detail?.sessionId === session.id) setView(detail.view);
    };
    window.addEventListener(VIEW_EVENT, onView);
    return () => window.removeEventListener(VIEW_EVENT, onView);
  }, [session.id]);

  const hasJira = !!session.jiraProject;
  // A view whose target no longer applies falls back to Claude.
  const active: View =
    (view === "jira" && !hasJira) || ((view === "files" || view === "git") && !project)
      ? "claude"
      : view;

  // Entry points living on Claude's status line: transcript, this project's
  // files, and Jira. Each opens a sub-view with a `←` back to the conversation.
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
      {hasJira && (
        <IconButton label={`Jira · ${session.jiraProject}`} onClick={() => setView("jira")}>
          <Icon d={BOARD_PATH} />
        </IconButton>
      )}
      <IconButton label="Session settings" onClick={() => setView("settings")}>
        <Icon d={GEAR_PATH} />
      </IconButton>
    </>
  );

  // Only alongside the conversation. Transcript, files and git are full-pane
  // reading views — a shell wedged under one of them would take height from the
  // thing you opened, to show something you did not ask for.
  const splitting = companion && active === "claude";

  return (
    <div ref={tileRef} className="flex h-full min-h-0 flex-col">
      <div
        className="relative min-h-0"
        // A basis rather than a height, so the terminal below keeps its own
        // minimum and neither half can be squeezed out of existence.
        style={splitting ? { flex: `0 0 ${split * 100}%` } : { flex: "1 1 0%" }}
      >
        {/* Always mounted — hidden, never torn down. Its status line carries the
            transcript / Jira icons, so this is the only header in Claude view. */}
        <div className="h-full" style={{ display: active === "claude" ? "block" : "none" }}>
          <ClaudePanel
            sessionId={session.id}
            target={target}
            cwd={folder}
            resume={session.resume}
            fullscreen={session.fullscreen}
            skipPermissions={session.skipPermissions}
            modelProfile={session.modelProfile}
            headerActions={headerActions}
          />
        </div>

        {active === "transcript" && (
          <BackView label="TRANSCRIPT" onBack={() => setView("claude")}>
            <TranscriptView
              sessionId={session.id}
              target={target}
              folder={folder}
              resume={session.resume}
            />
          </BackView>
        )}

        {active === "files" && project && (
          <BackView label={`FILES · ${folder}`} onBack={() => setView("claude")}>
            <FilesPane projectId={project.id} />
          </BackView>
        )}

        {active === "git" && project && (
          <BackView label={`GIT · ${folder}`} onBack={() => setView("claude")}>
            <GitPane projectId={project.id} />
          </BackView>
        )}

        {active === "settings" && (
          <BackView label="SETTINGS" onBack={() => setView("claude")}>
            <SessionSettings sessionId={session.id} />
          </BackView>
        )}

        {active === "jira" && hasJira && (
          <BackView label={`JIRA · ${session.jiraProject}`} onBack={() => setView("claude")}>
            <JiraPanel project={session.jiraProject!} />
          </BackView>
        )}
      </div>

      {splitting && (
        <>
          {/* The handle is 5px of hit area over a 1px line: a hairline is the
              right *look* and the wrong target, and a divider you cannot grab
              reads as a fixed layout. */}
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

/** A body view with a thin `←`-back bar returning to the Claude conversation. */
function BackView({
  label,
  onBack,
  children,
}: {
  label: string;
  onBack: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className="flex shrink-0 items-center gap-2 border-b px-3 py-1"
        style={{ borderColor: "var(--border)" }}
      >
        <IconButton label="Back to the Claude conversation" onClick={onBack}>
          <Icon d={BACK_PATH} />
        </IconButton>
        <span className="micro" style={{ letterSpacing: "0.06em" }}>
          {label}
        </span>
      </div>
      <div className="min-h-0 flex-1">{children}</div>
    </div>
  );
}
