import { useEffect, useMemo, useState } from "react";

import { SessionSettings } from "./SessionSettings";
import { JiraPanel } from "./JiraPanel";
import { HostPanel } from "./HostPanel";
import { invoke } from "@tauri-apps/api/core";

import { ClaudePanel } from "./ClaudePanel";
import { CodeEditor, disposeBufferModel } from "./CodeEditor";
import { FileTree } from "./FileTree";
import { BinaryPreview, MarkdownPreview, previewKind } from "./FilePreview";
import { TerminalView } from "./Terminal";
import { TranscriptView } from "./TranscriptView";
import { useSessions, isDirty, type Buffer, type Session } from "../lib/sessions";
import { gridLayout } from "../lib/grid";

/**
 * One session's workspace.
 *
 * **Claude is the default and gets the whole area.** That is the product: the
 * reason to run several sessions at once is to keep several Claudes working, and
 * the thing you look at is the conversation. Files and terminals matter, but they
 * are what you reach for occasionally — giving them permanent screen real estate
 * shrinks the one view you actually watch. So they are tabs, not panels.
 *
 * Every view stays **mounted** once opened and is hidden with `display`.
 * Switching tabs, or switching sessions, is therefore instant and lossless: a
 * running build keeps its scrollback, Claude keeps its screen, an unsaved edit
 * keeps its cursor.
 */

const TREE_KEY = "rmux.treeWidth";

type View = "claude" | "transcript" | "files" | "terminal" | "host" | "jira" | "settings";

const readSize = (key: string, fallback: number) => {
  const raw = Number(localStorage.getItem(key));
  return Number.isFinite(raw) && raw > 0 ? raw : fallback;
};

function EditorTabs({ session }: { session: Session }) {
  const openOrder = useSessions((s) => s.openOrder[session.id]);
  const buffers = useSessions((s) => s.buffers);
  const active = useSessions((s) => s.activeBuffer[session.id]);
  const activate = useSessions((s) => s.activateBuffer);
  const close = useSessions((s) => s.closeBuffer);

  if (!openOrder?.length) return null;

  return (
    <div className="flex shrink-0 overflow-x-auto border-b" style={{ borderColor: "var(--border)" }}>
      {openOrder.map((key) => {
        const buffer = buffers[key];
        if (!buffer) return null;
        const dirty = isDirty(buffer);
        const isActive = key === active;
        const label = buffer.path.split("/").filter(Boolean).pop() ?? buffer.path;

        return (
          <div
            key={key}
            className="flex shrink-0 items-center gap-2 border-r px-3 py-[6px]"
            style={{
              borderColor: "var(--border)",
              background: isActive ? "var(--hover)" : "transparent",
              boxShadow: isActive ? "inset 0 -1px 0 var(--text)" : "none",
            }}
          >
            <button
              type="button"
              onClick={() => activate(session.id, key)}
              title={buffer.path}
              className="data text-[11px]"
              style={{ color: isActive ? "var(--text)" : "var(--text-soft)" }}
            >
              {label}
            </button>
            <button
              type="button"
              aria-label={`Close ${label}`}
              className="grid h-[12px] w-[12px] place-items-center"
              style={{ color: dirty ? "rgb(var(--busy))" : "var(--text-faint)" }}
              onClick={() => {
                close(key);
                disposeBufferModel(key);
              }}
            >
              {dirty ? (
                <svg width="7" height="7" viewBox="0 0 8 8" aria-hidden="true">
                  <circle cx="4" cy="4" r="4" fill="currentColor" />
                </svg>
              ) : (
                <svg
                  width="9"
                  height="9"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2.5"
                  strokeLinecap="square"
                  aria-hidden="true"
                >
                  <path d="M18 6L6 18M6 6l12 12" />
                </svg>
              )}
            </button>
          </div>
        );
      })}
    </div>
  );
}

/** The Files view: tree on the left, editor on the right. */
function FilesView({ session }: { session: Session }) {
  const buffers = useSessions((s) => s.buffers);
  const activeKey = useSessions((s) => s.activeBuffer[session.id]);
  const openFile = useSessions((s) => s.openFile);
  const restoreFiles = useSessions((s) => s.restoreFiles);

  // Reopen whatever was open when the app last closed. Deferred to here rather
  // than run at boot so it costs a read only for sessions actually looked at,
  // and only once — the store drops the record after the first call.
  useEffect(() => restoreFiles(session.id), [session.id, restoreFiles]);

  const [treeWidth, setTreeWidth] = useState(() => readSize(TREE_KEY, 260));
  const [dragging, setDragging] = useState(false);

  const active = activeKey ? buffers[activeKey] : null;

  useEffect(() => {
    if (!dragging) return;
    const onMove = (e: MouseEvent) => setTreeWidth(Math.min(Math.max(e.clientX - 220, 150), 560));
    const onUp = () => setDragging(false);
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [dragging]);

  useEffect(() => localStorage.setItem(TREE_KEY, String(treeWidth)), [treeWidth]);

  return (
    <div className="flex h-full min-h-0">
      <div className="flex shrink-0 flex-col overflow-hidden py-2" style={{ width: treeWidth }}>
        <FileTree
          target={session.target}
          root={session.folder}
          selected={active?.path ?? null}
          onSelect={(path) => void openFile(session.id, path)}
        />
      </div>

      <div
        role="separator"
        aria-orientation="vertical"
        className="w-[4px] shrink-0 cursor-col-resize"
        onMouseDown={() => setDragging(true)}
        style={{ background: dragging ? "rgb(var(--primary))" : "var(--border)" }}
      />

      <div className="flex min-w-0 flex-1 flex-col">
        <EditorTabs session={session} />
        <div className="min-h-0 flex-1">
          {active ? (
            <FileBody session={session} buffer={active} />
          ) : (
            <div className="grid h-full place-items-center">
              <span className="micro">select a file</span>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * Show a file the way it wants to be shown.
 *
 * Source is still one click away for anything previewable — a rendered README is
 * usually what you want, but not when you are the one editing it.
 */
function FileBody({ session, buffer }: { session: Session; buffer: Buffer }) {
  const save = useSessions((s) => s.save);
  const edit = useSessions((s) => s.edit);
  const [showSource, setShowSource] = useState(false);

  const kind = previewKind(buffer.path);

  // Markdown renders from the text the buffer already holds, so it can only be
  // previewed once that text has actually arrived. Falling through to the editor
  // while loading — or when the file was too large to read — is what keeps a
  // half-loaded document from rendering as a convincingly blank page.
  const canPreview =
    kind !== "none" &&
    !buffer.error &&
    (kind !== "markdown" || (!buffer.loading && buffer.content?.kind === "text"));

  // Binary formats have no source view worth showing — an image's "source" is
  // base64, which helps nobody — so the toggle only appears for markdown.
  const body =
    canPreview && !showSource ? (
      kind === "markdown" ? (
        <MarkdownPreview text={buffer.text} />
      ) : (
        <BinaryPreview target={session.target} path={buffer.path} />
      )
    ) : (
      <CodeEditor buffer={buffer} onSave={() => void save(buffer.key)} onEdit={edit} />
    );

  return (
    <div className="flex h-full flex-col">
      {canPreview && (
        <div
          className="flex shrink-0 items-center gap-1 border-b px-2 py-1"
          style={{ borderColor: "var(--border)" }}
        >
          {(["preview", "source"] as const).map((mode) => {
            const isSource = mode === "source";
            if (isSource && kind !== "markdown") return null;
            return (
              <button
                key={mode}
                type="button"
                className="micro px-2 py-[2px]"
                onClick={() => setShowSource(isSource)}
                style={{
                  color: showSource === isSource ? "var(--text)" : "var(--text-faint)",
                  background: showSource === isSource ? "var(--hover)" : "transparent",
                }}
              >
                {mode}
              </button>
            );
          })}
          <span className="micro ml-auto">{kind}</span>
        </div>
      )}
      <div className="min-h-0 flex-1">{body}</div>
    </div>
  );
}

/** The Terminal view: tabs of shells, all kept alive. */
function TerminalsView({ session }: { session: Session }) {
  // Select the raw array and narrow with useMemo — filtering inside the selector
  // returns a new array each call and loops forever.
  const allTerminals = useSessions((s) => s.terminals);
  const terminals = useMemo(
    () => allTerminals.filter((t) => t.sessionId === session.id),
    [allTerminals, session.id],
  );
  const active = useSessions((s) => s.activeTerminal[session.id]);
  const activate = useSessions((s) => s.activateTerminal);
  const add = useSessions((s) => s.addTerminal);
  const close = useSessions((s) => s.closeTerminal);
  const setPty = useSessions((s) => s.setTerminalPty);
  const renameTerminal = useSessions((s) => s.renameTerminal);
  const [editing, setEditing] = useState<string | null>(null);

  /**
   * Close a tab and destroy its shell.
   *
   * The view no longer kills the PTY on unmount — that is what made switching
   * tabs reset the terminal — so closing has to do it explicitly, or the shell
   * would keep running with nothing attached.
   */
  const closeTab = (terminalId: string, ptyId?: string) => {
    // The target and name go too: the shell lives in the agent on the far side,
    // so closing the local attachment alone would leave it running forever.
    void invoke("terminal_close", {
      id: ptyId ?? "",
      target: session.target,
      session: terminalId,
    });
    close(terminalId);
  };

  return (
    <div className="flex h-full flex-col">
      <div
        className="flex shrink-0 items-center gap-1 border-b px-2 py-1"
        style={{ borderColor: "var(--border)" }}
      >
        {terminals.map((t) =>
          editing === t.id ? (
            <input
              key={t.id}
              autoFocus
              defaultValue={t.title}
              aria-label="Terminal name"
              className="data inset shrink-0 px-1 py-[1px] text-[10px] outline-none"
              style={{ width: 90, border: "1px solid var(--border-strong)", color: "var(--text)" }}
              onFocus={(e) => e.currentTarget.select()}
              onBlur={(e) => {
                renameTerminal(t.id, e.currentTarget.value);
                setEditing(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
                if (e.key === "Escape") {
                  e.currentTarget.value = t.title;
                  e.currentTarget.blur();
                }
              }}
            />
          ) : (
            <button
              key={t.id}
              type="button"
              onClick={() => activate(session.id, t.id)}
              onDoubleClick={() => setEditing(t.id)}
              title={`${t.title}\nDouble-click to rename`}
              className="data shrink-0 px-2 py-[2px] text-[10px]"
              style={{
                color: t.id === active ? "var(--text)" : "var(--text-faint)",
                background: t.id === active ? "var(--hover)" : "transparent",
              }}
            >
              {t.title}
              <span
                role="button"
                tabIndex={0}
                aria-label={`Close ${t.title}`}
                className="ml-2"
                onClick={(e) => {
                  e.stopPropagation();
                  closeTab(t.id, t.ptyId);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") closeTab(t.id, t.ptyId);
                }}
              >
                ×
              </span>
            </button>
          ),
        )}
        <button type="button" className="micro ml-auto px-2" onClick={() => add(session.id)}>
          + new
        </button>
      </div>

      <div className="relative min-h-0 flex-1">
        {terminals.length === 0 && (
          <div className="grid h-full place-items-center">
            <button type="button" className="btn" onClick={() => add(session.id)}>
              New terminal
            </button>
          </div>
        )}
        {terminals.map((t) => (
          <div
            key={t.id}
            className="absolute inset-0 p-2"
            style={{ display: t.id === active ? "block" : "none" }}
          >
            <TerminalView
              target={session.target}
              cwd={session.folder}
              // The tab's own id names the shell on the target, and it is
              // persisted — which is what brings the shell back after a restart.
              session={t.id}
              ptyId={t.ptyId}
              onOpened={(ptyId) => setPty(t.id, ptyId)}
              onExit={() => close(t.id)}
            />
          </div>
        ))}
      </div>
    </div>
  );
}

export function SessionView({ session }: { session: Session }) {
  const [view, setView] = useState<View>("claude");
  // Views mount on first visit and then stay, so switching never reloads.
  const [visited, setVisited] = useState<Set<View>>(new Set(["claude"]));

  const addTerminal = useSessions((s) => s.addTerminal);
  const dirtyCount = useSessions(
    (s) => Object.values(s.buffers).filter((b) => b.sessionId === session.id && isDirty(b)).length,
  );

  const show = (next: View) => {
    setView(next);
    setVisited((v) => (v.has(next) ? v : new Set(v).add(next)));
  };

  // A session restored from a previous run has no terminals — they are not
  // persisted, and cannot be, since a PTY does not survive the app closing.
  // Without this, reopening rmux leaves every session showing an empty panel
  // with a button, which reads as the terminal being broken.
  useEffect(() => {
    const state = useSessions.getState();
    if (!state.terminals.some((t) => t.sessionId === session.id)) {
      addTerminal(session.id);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session.id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;
      // ⌘I / ⌘R / ⌘E / ⌘` — mirroring how often each is used.
      if (e.key.toLowerCase() === "i") {
        e.preventDefault();
        show("claude");
      }
      if (e.key.toLowerCase() === "r") {
        e.preventDefault();
        show("transcript");
      }
      if (e.key.toLowerCase() === "e") {
        e.preventDefault();
        show("files");
      }
      if (e.key === "`") {
        e.preventDefault();
        show("terminal");
      }
      if (e.key.toLowerCase() === "h") {
        e.preventDefault();
        show("host");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const tabs: { id: View; label: string; hint: string }[] = [
    { id: "claude", label: "Claude", hint: "⌘I" },
    { id: "transcript", label: "Transcript", hint: "⌘R" },
    { id: "files", label: "Files", hint: "⌘E" },
    { id: "terminal", label: "Terminal", hint: "⌘`" },
    { id: "host", label: "Host", hint: "⌘H" },
    // Only once a project is assigned. A tab that is always there but empty for
    // most sessions is a tab everyone learns to ignore.
    ...(session.jiraProject
      ? [{ id: "jira" as View, label: session.jiraProject, hint: "" }]
      : []),
    { id: "settings", label: "Settings", hint: "" },
  ];

  return (
    <div className="panel flex min-h-0 flex-1 flex-col overflow-hidden">
      <div
        className="flex shrink-0 items-center gap-1 border-b px-2 py-1"
        style={{ borderColor: "var(--border)" }}
      >
        {tabs.map((tab) => (
          <button
            key={tab.id}
            type="button"
            onClick={() => show(tab.id)}
            className="flex items-center gap-2 px-3 py-[3px]"
            style={{
              background: view === tab.id ? "var(--hover)" : "transparent",
              boxShadow: view === tab.id ? "inset 0 -1px 0 var(--text)" : "none",
            }}
          >
            <span
              className="data text-[11px]"
              style={{ color: view === tab.id ? "var(--text)" : "var(--text-soft)" }}
            >
              {tab.label}
            </span>
            {/* Unsaved work is worth surfacing from the tab you are not looking at. */}
            {tab.id === "files" && dirtyCount > 0 && (
              <span className="micro" style={{ color: "rgb(var(--busy))" }}>
                {dirtyCount}
              </span>
            )}
            <span className="micro" style={{ opacity: 0.55 }}>
              {tab.hint}
            </span>
          </button>
        ))}

        <span className="micro ml-auto truncate" title={session.folder}>
          {session.folder}
        </span>
      </div>

      <div className="relative min-h-0 flex-1">
        {/* Claude is mounted from the start; the others on first visit. Each stays
            mounted so switching is instant and nothing is torn down. */}
        <div className="absolute inset-0" style={{ display: view === "claude" ? "block" : "none" }}>
          <ClaudePanel
            // Remounts when the render mode changes, which is what restarts
            // Claude under the new one. The conversation is unaffected: it
            // reattaches by name, or resumes from its transcript.
            key={session.fullscreen ? "fullscreen" : "inline"}
            sessionId={session.id}
            target={session.target}
            cwd={session.folder}
            resume={session.resume}
            fullscreen={session.fullscreen}
          />
        </div>

        {visited.has("transcript") && (
          <div
            className="absolute inset-0"
            style={{ display: view === "transcript" ? "block" : "none" }}
          >
            <TranscriptView session={session} />
          </div>
        )}

        {visited.has("files") && (
          <div className="absolute inset-0" style={{ display: view === "files" ? "block" : "none" }}>
            <FilesView session={session} />
          </div>
        )}

        {visited.has("terminal") && (
          <div
            className="absolute inset-0"
            style={{ display: view === "terminal" ? "block" : "none" }}
          >
            <TerminalsView session={session} />
          </div>
        )}

        {visited.has("host") && (
          <div
            className="absolute inset-0"
            style={{ display: view === "host" ? "block" : "none" }}
          >
            <HostPanel session={session} />
          </div>
        )}

        {visited.has("settings") && (
          <div
            className="absolute inset-0 overflow-auto"
            style={{ display: view === "settings" ? "block" : "none" }}
          >
            <SessionSettings session={session} />
          </div>
        )}

        {session.jiraProject && visited.has("jira") && (
          <div className="absolute inset-0" style={{ display: view === "jira" ? "block" : "none" }}>
            <JiraPanel session={session} />
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Every session's view, with only the active one displayed.
 *
 * Views are created on first activation and kept, which is what makes switching
 * sessions instant. The cost is memory per session; the alternative is losing a
 * running build's output every time you look away, which is worse.
 */
/**
 * Every session's view, laid out.
 *
 * Two modes. **Focus** shows one at a time; **grid** shows several at once, the
 * way you would watch several cameras. Both keep every visited session mounted
 * and merely change which are displayed — a session that unmounted would lose
 * its terminal scrollback and its Claude pane's scroll position, and switching
 * back would feel like reopening rather than returning.
 *
 * In grid mode the instruments follow the **last cell you clicked**, not the
 * one under the pointer. Following hover would make the whole right-hand rail
 * flicker as the mouse crossed the grid, which is unreadable precisely when you
 * are trying to compare hosts.
 */
export function SessionDeck() {
  const sessions = useSessions((s) => s.sessions);
  const active = useSessions((s) => s.activeSession);
  const grid = useSessions((s) => s.grid);
  const activate = useSessions((s) => s.activate);
  const slots = useSessions((s) => s.gridSlots);
  const focusedCell = useSessions((s) => s.focusedCell);
  const focusCell = useSessions((s) => s.focusCell);
  const [mounted, setMounted] = useState<string[]>([]);

  // Which session is in which cell. Assignments win, empty cells auto-fill —
  // see `gridLayout`, where the rules are stated and tested.
  const cells = useMemo(() => gridLayout(sessions, slots, grid), [sessions, slots, grid]);

  useEffect(() => {
    if (!active) return;
    // The membership check must be inside the updater: reading `mounted` from
    // the closure lets React's double-invoked effects mount a session twice.
    setMounted((m) => (m.includes(active) ? m : [...m, active]));
  }, [active]);

  // In grid mode every session on screen has to be mounted, not just the ones
  // that have been visited — otherwise the cells come up empty until clicked.
  useEffect(() => {
    if (grid < 2) return;
    const wanted = cells.filter((c): c is Session => !!c).map((s) => s.id);
    setMounted((m) => [...m, ...wanted.filter((id) => !m.includes(id))]);
  }, [grid, cells]);

  const live = mounted.filter((id) => sessions.some((s) => s.id === id));

  if (grid >= 2) {
    return (
      <div
        className="grid min-h-0 flex-1 gap-[1px]"
        style={{
          gridTemplateColumns: `repeat(${grid}, minmax(0, 1fr))`,
          gridAutoRows: "minmax(0, 1fr)",
          background: "var(--border)",
        }}
      >
        {cells.map((session, index) => (
          <div
            key={session ? session.id : `empty-${index}`}
            className="relative flex min-h-0 flex-col overflow-hidden"
            // Selects the *cell*, not only the session in it. That is what
            // makes the rail able to fill it — see `focusedCell`. An empty
            // cell is selectable too; it is the one you most want to fill.
            onMouseDownCapture={() => {
              focusCell(index);
              if (session) activate(session.id);
            }}
            style={{
              // Chalk, not red. Red is reserved for "the operator must act", and
              // spending it on "this is the cell you clicked" is exactly the
              // dilution the design system warns about — every cell you touch
              // would look like an alert.
              // Two different things, two different weights. A *selected* cell
              // is the one the rail will fill, so it has to be unmistakable;
              // the merely-active one only tells the instruments where to look.
              outline:
                focusedCell === index
                  ? "2px solid var(--text)"
                  : session && session.id === active
                    ? "1px solid rgba(232,230,225,0.45)"
                    : "1px solid transparent",
              outlineOffset: -1,
              background: session ? undefined : "var(--app-bg)",
            }}
          >
            {session ? (
              <SessionView session={session} />
            ) : (
              <div className="grid h-full place-items-center">
                <span className="micro" style={{ color: "var(--text-faint)" }}>
                  {focusedCell === index ? "PICK A SESSION IN THE RAIL" : "EMPTY — CLICK TO FILL"}
                </span>
              </div>
            )}
          </div>
        ))}
      </div>
    );
  }

  return (
    <>
      {live.map((id) => {
        const session = sessions.find((s) => s.id === id)!;
        return (
          <div
            key={id}
            className="flex min-h-0 flex-1"
            style={{ display: id === active ? "flex" : "none" }}
          >
            <SessionView session={session} />
          </div>
        );
      })}
    </>
  );
}
