import { useEffect, useRef, useState } from "react";
import { motion, AnimatePresence } from "motion/react";
import { Terminal as Xterm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Channel, invoke } from "@tauri-apps/api/core";

import { api, isTauri, type ClaudeStatus, type TargetRef } from "../lib/api";
import { contextLimit, sniffWindow } from "../lib/context-window";
import { useSessions } from "../lib/sessions";
import { CLAUDE_THEME } from "../lib/terminal-theme";
import { ContextMeter } from "./ContextMeter";
import { BrowserReports } from "./BrowserReports";
import { attachClipboard, copyAll, copySelection, copyViewport } from "../lib/terminal-clipboard";
import { MouseModeTracker } from "../lib/mouse-modes";
import { PanelLoader } from "./PanelLoader";

/**
 * A Claude Code session.
 *
 * **This is a terminal, not a chat widget.** Claude's own TUI is rendered
 * verbatim and you type straight into it, so every feature of the real CLI —
 * slash commands, modes, editing, its own history — works exactly as it does in
 * a shell. A bespoke chat box would be a worse copy of an interface people
 * already know.
 *
 * The one thing layered on top is a decision card: when Claude asks a question,
 * the same screen is also offered as buttons. The card carries the prompt's
 * **fingerprint**, and answering sends it back; if the screen has moved on, Rust
 * refuses rather than delivering the keystroke to whatever replaced the question.
 * That is the guard against the two failures that plagued the previous
 * generation — a card shown after its question was resolved, and an answer
 * landing on the wrong screen. The terminal remains authoritative; the card is
 * a shortcut, never the only way through.
 */

/**
 * Bytes to a string, one char per byte.
 *
 * Only ever used to look for ASCII escape sequences, so the encoding does not
 * need to be correct for text — it needs to be cheap and to never merge or split
 * bytes, which a UTF-8 decode across chunk boundaries can do.
 */
function latin1(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 1) out += String.fromCharCode(bytes[i]!);
  return out;
}

/**
 * Does this chunk mention a context window, without decoding it?
 *
 * The output handler is the hottest path in the app, so the sniff below must
 * not turn every chunk into a string. Claude redraws its whole TUI many times a
 * second; `context` appears in a handful of those frames and in none of the
 * rest, so a byte scan pays for itself immediately.
 */
const NEEDLE = [0x63, 0x6f, 0x6e, 0x74, 0x65, 0x78, 0x74]; // "context"

function mentionsContext(bytes: Uint8Array): boolean {
  outer: for (let i = 0; i + NEEDLE.length <= bytes.length; i += 1) {
    for (let j = 0; j < NEEDLE.length; j += 1) {
      // Case-insensitive on ASCII letters only, which is all the needle is.
      if ((bytes[i + j]! | 0x20) !== NEEDLE[j]!) continue outer;
    }
    return true;
  }
  return false;
}

/** Mode, permissions, model and context — what the TUI's status line showed. */
function ClaudeStatusStrip({
  status,
  window: configured,
}: {
  status: ClaudeStatus | null;
  window?: number;
}) {
  if (!status) return null;

  const context = status.contextTokens ?? 0;
  const limit = contextLimit(status.model, context, configured);

  const bits: string[] = [];
  if (status.mode) bits.push(status.mode);
  // Only worth saying when it is not the ordinary one — a permanent "default"
  // is noise, whereas "bypassPermissions" is something you want to notice.
  if (status.permissionMode && status.permissionMode !== "default") {
    bits.push(status.permissionMode);
  }
  if (status.model) bits.push(status.model.replace(/^claude-/, ""));

  if (!bits.length && context === 0) return null;

  return (
    <span className="flex min-w-0 items-center gap-2">
      <span className="micro truncate" style={{ color: "var(--text-faint)" }}>
        {bits.join(" · ")}
      </span>
      {/* The bar rather than another word in that list. "How much room is
          left" is the one question here that is answered faster by a shape. */}
      {context > 0 && <ContextMeter tokens={context} limit={limit} variant="strip" />}
    </span>
  );
}

type Choice = { key: string; label: string; selected: boolean };
type Prompt = { question: string; choices: Choice[]; fingerprint: string };
type ClaudeState = { prompt: Prompt | null; working: boolean };

export function ClaudePanel({
  sessionId,
  target,
  cwd,
  resume,
  fullscreen,
}: {
  sessionId: string;
  target: TargetRef;
  cwd?: string;
  /** Conversation to continue instead of starting a new one. */
  resume?: string;
  /** Let Claude use its fullscreen TUI. Off by default — see `Rendering`. */
  fullscreen?: boolean;
}) {
  const setStatus = useSessions((s) => s.setStatus);
  const setClaudeSession = useSessions((s) => s.setClaudeSession);
  const adoptTitle = useSessions((s) => s.adoptClaudeTitle);
  const clearClaudeSession = useSessions((s) => s.clearClaudeSession);
  const setFullscreen = useSessions((s) => s.setFullscreen);
  const setResume = useSessions((s) => s.setResume);
  const configure = useSessions((s) => s.configureSession);
  // The window Claude itself printed. Read from the pane's own output rather
  // than inferred, which is the only way anyone actually knows it: the
  // transcript records `claude-opus-5` whether that is a 200k or a 1M window.
  const contextWindow = useSessions((s) =>
    s.sessions.find((x) => x.id === sessionId)?.contextWindow,
  );
  // Read by the output handler, which is installed once and never re-created —
  // so it cannot close over `contextWindow` and see anything but its first
  // value. A ref is the only thing that stays current there.
  const windowRef = useRef(contextWindow);
  windowRef.current = contextWindow;
  const [switching, setSwitching] = useState(false);
  // The running session, remembered across remounts so the view reattaches
  // rather than launching a second Claude in the same folder.
  const runningId = useSessions((s) => s.claudeSessions[sessionId]);
  const runningRef = useRef(runningId);
  runningRef.current = runningId;
  const hostRef = useRef<HTMLDivElement>(null);
  // Held so the header's copy controls can reach the live terminal. Claude's TUI
  // captures the mouse, so without these there is no discoverable way to get
  // text out of this pane at all.
  const xtermRef = useRef<Xterm | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  // Select mode turns Claude's mouse reporting off *locally*, so a drag selects
  // instead of being sent to Claude. See lib/mouse-modes.ts.
  const mouseModes = useRef(new MouseModeTracker());
  const [selectMode, setSelectMode] = useState(false);
  // A black rectangle is indistinguishable from a crash, so the pane says what
  // it is doing until Claude's first byte arrives.
  const [ready, setReady] = useState(false);
  const [phase, setPhase] = useState<"connecting" | "starting">("connecting");
  // Inline rendering draws no status line, so rmux reports mode, model and
  // context itself — read from the transcript, never guessed.
  const [claudeStatus, setClaudeStatus] = useState<ClaudeStatus | null>(null);
  const [claudeId, setClaudeId] = useState<string | null>(null);
  const [state, setState] = useState<ClaudeState>({ prompt: null, working: false });
  const [error, setError] = useState<string | null>(null);
  const [answering, setAnswering] = useState(false);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    if (!isTauri()) {
      setError("Claude sessions need the rmux desktop shell. Run `pnpm tauri dev`.");
      return;
    }

    let disposed = false;
    let id: string | null = null;

    const xterm = new Xterm({
      theme: CLAUDE_THEME,
      allowTransparency: true,
      fontFamily: '"IBM Plex Mono", ui-monospace, Menlo, monospace',
      fontSize: 12,
      lineHeight: 1.3,
      cursorBlink: true,
      scrollback: 5000,
    });
    xtermRef.current = xterm;

    const fit = new FitAddon();
    xterm.loadAddon(fit);
    xterm.open(host);

    // The GPU renderer, same as the terminal tab. Without it this pane falls back
    // to the DOM renderer, and scrolling or dragging a selection across Claude's
    // constantly-redrawing TUI is visibly slow — which is exactly what it was.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      xterm.loadAddon(webgl);
    } catch {
      // Canvas/DOM fallback is automatic; slower, but it renders.
    }

    // Copy and paste are xterm's own; this only adds select-all. Claude's TUI
    // turns on mouse reporting, so a plain drag goes to Claude rather than
    // selecting — hold Option to select, or use the copy buttons in the header.
    attachClipboard(xterm);

    const output = new Channel<ArrayBuffer>();
    output.onmessage = (chunk) => {
      const bytes = new Uint8Array(chunk);
      // Mouse-mode tracking, for the fullscreen escape hatch. Prefiltered on the
      // raw bytes: this is the hottest path in the app, and decoding every chunk
      // to a string just to look for a rare sequence was pure cost. `0x1b` is
      // absent from the overwhelming majority of output.
      if (bytes.includes(0x1b)) {
        mouseModes.current.observe(latin1(bytes));
      }
      // Claude states its own context window beside the model — `Opus 5 (1M
      // context)` — in the banner and in `/status`. That is the only place it
      // is ever stated, so reading it here is what turns the meter's
      // denominator from a guess into an observation. Byte-prefiltered: this
      // runs on every chunk of a TUI that redraws constantly.
      if (mentionsContext(bytes)) {
        const sniffed = sniffWindow(latin1(bytes));
        // Written through the store so it persists and the rail sees it too.
        // Guarded on a change, or a redraw every frame would rewrite it.
        if (sniffed && sniffed !== windowRef.current) {
          windowRef.current = sniffed;
          configure(sessionId, { contextWindow: sniffed });
        }
      }
      xterm.write(bytes);
      setReady(true);
    };

    // Started only once the pane has a real size — fitting before layout tells
    // Claude its window is about 12x4, and its TUI then draws into a space too
    // small to read, which looks like a blank panel.
    // Captured once, and cleared locally when it turns out to be dead.
    //
    // NOT read from the ref on each attempt: `runningRef.current` is reassigned
    // on every render, so clearing it inside a callback is undone by the next
    // one — and the retry would then reattach to the same dead handle forever.
    let attachTo = runningRef.current;

    let starting = false;
    const startClaude = () => {
      if (starting || id || disposed) return;
      starting = true;
      fit.fit();

      const existing = attachTo;
      const request = existing
        ? // Reattach within this run of the app: the view remounted, so re-stream
          // the session object we already hold.
          invoke("claude_attach", { id: existing, output }).then(() => ({ id: existing }))
        : // No live handle. Starting with a stable `sessionName` does not
          // necessarily start anything: the agent reattaches to the Claude
          // already running under that name, which is what brings a conversation
          // back — still working — after rmux has been closed.
          invoke<{ id: string }>("claude_start", {
            target,
            cwd,
            resume,
            sessionName: `claude-${sessionId}`,
            fullscreen,
            cols: xterm.cols,
            rows: xterm.rows,
            output,
          });

      void request
        .then((started) => {
          if (disposed) return;
          id = started.id;
          setClaudeId(started.id);
          setPhase("starting");
          if (!existing) setClaudeSession(sessionId, started.id);
          void invoke("claude_resize", { id: started.id, cols: xterm.cols, rows: xterm.rows });
          // Focus straight away: this view is a terminal, and it should accept
          // typing the moment it appears.
          xterm.focus();
        })
        .catch((e) => {
          // The handle is stale — the process that owned it is gone. Fall
          // through to the named path, which reattaches to the Claude still
          // running under this session's name, or starts one.
          if (existing && !disposed) {
            starting = false;
            attachTo = undefined;
            clearClaudeSession(sessionId);
            startClaude();
            return;
          }
          setError(typeof e === "string" ? e : String(e));
        });
    };

    // Typing goes straight to Claude's TUI, so anything the cards do not cover
    // is still reachable.
    const onData = xterm.onData((data) => {
      if (id) void invoke("claude_write", { id, data });
    });

    const observer = new ResizeObserver(() => {
      if (host.clientWidth === 0 || host.clientHeight === 0) return;
      if (!id) {
        startClaude();
        return;
      }
      fit.fit();
      void invoke("claude_resize", { id, cols: xterm.cols, rows: xterm.rows });
    });
    observer.observe(host);

    if (host.clientWidth > 0 && host.clientHeight > 0) startClaude();

    return () => {
      disposed = true;
      observer.disconnect();
      onData.dispose();
      xtermRef.current = null;
      xterm.dispose();
      // The session is deliberately NOT stopped: Claude may be mid-task, and
      // this view may simply be remounting. It is stopped only when the coding
      // session is closed.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Poll the screen. Cheap — it reads an in-memory emulator, no network.
  useEffect(() => {
    if (!claudeId) return;
    let cancelled = false;

    const tick = async () => {
      try {
        const next = await invoke<ClaudeState>("claude_state", { id: claudeId });
        if (cancelled) return;
        setState(next);
        // Publish to the rail. This is what makes "which session needs me?"
        // answerable at a glance without opening each one.
        setStatus(sessionId, next.prompt ? "waiting" : next.working ? "working" : "idle");
      } catch {
        // The session ended; the terminal above already shows why.
      }
    };

    void tick();
    const timer = setInterval(tick, 400);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [claudeId, sessionId, setStatus]);

  /**
   * Adopt Claude's own title for this conversation.
   *
   * Claude writes the title into its session log once the work has enough shape
   * to name, and revises it as that changes — so this polls rather than reading
   * once. Slowly: it costs a directory scan on the target, it is cosmetic, and a
   * session that has just started has nothing to report yet.
   *
   * The store ignores this entirely once the session has been renamed by hand.
   */
  useEffect(() => {
    if (!claudeId || !cwd) return;
    let cancelled = false;

    const tick = async () => {
      try {
        const found = await api.claudeSessions(target, cwd);
        if (cancelled) return;
        const mine = found.find((s) => s.id === claudeId);
        if (mine?.title) adoptTitle(sessionId, mine.title);

        // Same cadence: this is a bounded tail read on the far side, and the
        // status changes on the scale of turns, not frames.
        const t = await api.claudeTranscript(target, cwd, resume, 128 * 1024);
        if (!cancelled) setClaudeStatus(t.status);
      } catch {
        // Cosmetic. A session whose history cannot be read still works.
      }
    };

    void tick();
    const timer = setInterval(tick, 20_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [claudeId, sessionId, cwd, target, adoptTitle]);

  const answer = async (choice: Choice) => {
    if (!claudeId || !state.prompt) return;
    setAnswering(true);
    setError(null);
    try {
      await invoke("claude_answer", {
        id: claudeId,
        fingerprint: state.prompt.fingerprint,
        key: choice.key,
      });
      // Clear optimistically; the next poll confirms.
      setState((s) => ({ ...s, prompt: null }));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setAnswering(false);
    }
  };

  return (
    <div className="flex h-full flex-col">
      <header
        className="flex shrink-0 items-center gap-3 border-b px-3 py-1"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="micro">CLAUDE</span>

        <ClaudeStatusStrip status={claudeStatus} window={contextWindow} />

        {/* Copying out of this pane needs help. A drag is sent to Claude, not
            used to select, so the usual gesture silently does nothing. */}
        <div className="flex items-center gap-2">
          <button
            type="button"
            className="micro"
            title="Copy the selection, or the visible screen if nothing is selected"
            onClick={() => {
              const term = xtermRef.current;
              if (!term) return;
              void copySelection(term)
                .then(async (had) => {
                  if (!had) await copyViewport(term);
                  setCopied(had ? "selection copied" : "screen copied");
                  setTimeout(() => setCopied(null), 2500);
                })
                .catch(() => setCopied("copy failed"));
            }}
          >
            copy
          </button>
          <button
            type="button"
            className="micro"
            title="Copy the whole scrollback"
            onClick={() => {
              const term = xtermRef.current;
              if (!term) return;
              void copyAll(term)
                .then(() => {
                  setCopied("scrollback copied");
                  setTimeout(() => setCopied(null), 2500);
                })
                .catch(() => setCopied("copy failed"));
            }}
          >
            copy all
          </button>
          {copied && (
            <span
              className="micro"
              style={{
                color: copied === "copy failed" ? "rgb(var(--primary))" : "var(--text)",
              }}
            >
              {copied}
            </span>
          )}
          <button
            type="button"
            className="micro"
            disabled={switching}
            title={
              fullscreen
                ? "Switch to inline rendering: native selection, copy and scrollback"
                : "Switch to Claude's fullscreen TUI. It takes the mouse, so selection and scrolling stop being native."
            }
            onClick={() => {
              if (switching) return;
              setSwitching(true);
              const next = !fullscreen;
              // The conversation must survive the restart. Its id comes from the
              // transcript, which knows it even for a session started fresh.
              void (async () => {
                try {
                  if (cwd) {
                    const t = await api.claudeTranscript(target, cwd, resume, 65536);
                    if (t.sessionId) setResume(sessionId, t.sessionId);
                  }
                  // The agent reattaches by name, so the running Claude has to
                  // end or the new mode would never take effect.
                  await api.claudeEndSession(target, `claude-${sessionId}`);
                } catch {
                  // Even if that failed, switching is still what was asked for.
                } finally {
                  clearClaudeSession(sessionId);
                  setFullscreen(sessionId, next);
                  setSwitching(false);
                }
              })();
            }}
            style={{
              color: fullscreen ? "var(--text)" : "var(--text-faint)",
              background: fullscreen ? "var(--hover)" : "transparent",
              padding: "1px 6px",
            }}
          >
            {switching ? "switching…" : fullscreen ? "fullscreen" : "inline"}
          </button>

          {fullscreen && (
          <button
            type="button"
            className="micro"
            title={
              selectMode
                ? "Give the mouse back to Claude"
                : "Turn Claude's mouse capture off so you can select text"
            }
            onClick={() => {
              const term = xtermRef.current;
              if (!term) return;
              const next = !selectMode;
              // Written into xterm, not sent to Claude: this changes what *this*
              // terminal does with the mouse, and Claude is never told.
              term.write(
                next ? mouseModes.current.disableSequence() : mouseModes.current.restoreSequence(),
              );
              setSelectMode(next);
            }}
            style={{
              color: selectMode ? "var(--text)" : "var(--text-faint)",
              background: selectMode ? "var(--hover)" : "transparent",
              padding: "1px 6px",
            }}
          >
            select mode
          </button>
          )}
        </div>

        {state.working && (
          <div className="flex items-center gap-2">
            {/* Data movement, not a blinking dot — rule 2. */}
            <div className="flex h-[12px] items-end gap-[2px]">
              <div className="eq-bar" style={{ height: 12 }} />
              <div className="eq-bar" style={{ height: 12 }} />
              <div className="eq-bar" style={{ height: 12 }} />
            </div>
            <span className="micro">working</span>
          </div>
        )}

        {/* Amber: in progress is not an alert. */}
        {state.prompt && (
          <span className="micro" style={{ color: "rgb(var(--primary))" }}>
            waiting for you
          </span>
        )}

        <div className="ml-auto flex gap-2">
          <button
            type="button"
            className="micro"
            onClick={() => claudeId && void invoke("claude_interrupt", { id: claudeId })}
            title="Interrupt (Esc)"
          >
            interrupt
          </button>
        </div>
      </header>

      {/* What a connected browser sent back for this session. Empty, and takes
          no space, until something arrives. */}
      <BrowserReports sessionId={sessionId} claudeId={claudeId} />

      <div className="relative min-h-0 flex-1">
        <div ref={hostRef} className="h-full w-full p-2" />

        {/* Until Claude's first byte, the pane is a black rectangle — which reads
            as a crash, not as work in progress. */}
        {!ready && !error && (
          <div
            className="absolute inset-0"
            // The *loader* only — the terminal's own transparency is settled in
            // `signal-room.css`, by overriding the opaque `.xterm-viewport`
            // xterm ships. Tinted rather than solid so this covers the panel
            // consistently with it while Claude starts.
            style={{
              background: "color-mix(in srgb, var(--app-panel) var(--panel-tint, 64%), transparent)",
            }}
          >
            <PanelLoader
              phase={phase === "connecting" ? "CONNECTING" : "STARTING CLAUDE"}
              detail={target.host ?? "local"}
              hint={
                phase === "connecting"
                  ? "First connection to a host also installs the session agent — that is what lets this keep running after rmux is closed."
                  : "Claude is starting in the project folder."
              }
            />
          </div>
        )}

        {/* The decision card, over the live screen. */}
        <AnimatePresence>
          {state.prompt && (
            <motion.div
              initial={{ opacity: 0, y: 10 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: 6 }}
              transition={{ type: "spring", stiffness: 280, damping: 26 }}
              className="window corner absolute inset-x-3 bottom-3 p-4"
            >
              <p className="data mb-3 text-[12px] leading-relaxed">{state.prompt.question}</p>

              <div className="flex flex-col gap-1">
                {state.prompt.choices.map((choice) => (
                  <button
                    key={choice.key}
                    type="button"
                    disabled={answering}
                    onClick={() => void answer(choice)}
                    className="data flex items-center gap-2 px-2 py-[5px] text-left text-[11px]"
                    style={{
                      background: choice.selected ? "var(--hover)" : "transparent",
                      color: "var(--text)",
                    }}
                  >
                    <span className="micro" style={{ minWidth: 12 }}>
                      {choice.key}
                    </span>
                    {choice.label}
                  </button>
                ))}
              </div>

              {error && (
                <p
                  role="alert"
                  className="data mt-2 text-[10px]"
                  style={{ color: "rgb(var(--primary))" }}
                >
                  {error}
                </p>
              )}
            </motion.div>
          )}
        </AnimatePresence>

        {error && !state.prompt && (
          <p
            role="alert"
            className="data absolute inset-x-0 top-0 p-3 text-[11px]"
            style={{ color: "rgb(var(--primary))" }}
          >
            {error}
          </p>
        )}
      </div>

    </div>
  );
}
