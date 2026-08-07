import { useEffect, useRef, useState, type ReactNode } from "react";
import { motion, AnimatePresence } from "motion/react";
import { Terminal as Xterm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Channel, invoke } from "@tauri-apps/api/core";

import { api, isTauri, type ClaudeStatus, type TargetRef } from "../lib/api";
import { contextLimit, sniffWindow } from "../lib/context-window";
import { settleResize } from "../lib/terminal-resize";
import { useWorkspace } from "../lib/workspace";
import { basename } from "../lib/workspace-model";
import { claudeTheme, gpuRendering } from "../lib/terminal-theme";
import { readMonoStack } from "../lib/fonts";
import { imagesFrom, promptFor, uploadImage } from "../lib/paste-image";
import { ContextMeter } from "./ContextMeter";
import { BrowserReports } from "./BrowserReports";
import { record } from "../lib/activity";
import { attachImeReplace } from "../lib/ime-replace";
import { attachClipboard } from "../lib/terminal-clipboard";
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
  skipPermissions,
  modelProfile,
  headerActions,
}: {
  sessionId: string;
  target: TargetRef;
  cwd?: string;
  /** Controls rendered at the right of the status line (e.g. open-transcript). */
  headerActions?: ReactNode;
  /** Conversation to continue instead of starting a new one. */
  resume?: string;
  /** Let Claude use its fullscreen TUI. Off by default — see `Rendering`. */
  fullscreen?: boolean;
  /** Launch with `--dangerously-skip-permissions`. Decided when the session was created. */
  skipPermissions?: boolean;
  /**
   * The saved model profile to run under. `undefined` is Anthropic.
   *
   * Passed on every start, including when absent: the agent daemon outlives
   * sessions, so the previous profile's variables are still in its environment
   * and Rust needs the chance to clear them. Omitting this when nothing is
   * selected would leave a session talking to the last provider used.
   */
  modelProfile?: string;
}) {
  const setStatus = useWorkspace((s) => s.setStatus);
  const setClaudeSession = useWorkspace((s) => s.setLive);
  const adoptTitle = useWorkspace((s) => s.adoptClaudeTitle);
  const clearClaudeSession = useWorkspace((s) => s.clearLive);
  const configure = useWorkspace((s) => s.configureSession);
  // The window Claude itself printed. Read from the pane's own output rather
  // than inferred, which is the only way anyone actually knows it: the
  // transcript records `claude-opus-5` whether that is a 200k or a 1M window.
  /**
   * The session's own name, shown in its header.
   *
   * In a grid every pane says "CLAUDE" and nothing else, so telling four
   * conversations apart meant looking away to the rail and counting positions —
   * which is exactly the question the pane itself should answer. It replaces the
   * static "CLAUDE" label rather than sitting beside it: the tab strip above
   * already says which surface this is, so the word carried no information while
   * occupying the most valuable space in the header.
   */
  const sessionName = useWorkspace((s) => s.sessions.find((x) => x.id === sessionId)?.name);

  /**
   * The header is titled by the **folder**, not the session's name.
   *
   * A session name is now frequently a resumed conversation's title — long,
   * about the *topic*, and identical-looking across machines. The folder is the
   * one thing that says which body of work this pane is, it is stable across
   * every session started in it, and it is what an operator scanning a grid is
   * actually matching against. The session name is not lost: it follows as the
   * secondary label, and it is what the rail is sorted by.
   */
  const paneTitle = cwd ? basename(cwd) : (sessionName ?? "claude");

  const contextWindow = useWorkspace((s) =>
    s.sessions.find((x) => x.id === sessionId)?.contextWindow,
  );
  // Read by the output handler, which is installed once and never re-created —
  // so it cannot close over `contextWindow` and see anything but its first
  // value. A ref is the only thing that stays current there.
  const windowRef = useRef(contextWindow);
  windowRef.current = contextWindow;
  // The running session, remembered across remounts so the view reattaches
  // rather than launching a second Claude in the same folder.
  const runningId = useWorkspace((s) => s.live[sessionId]);
  const runningRef = useRef(runningId);
  runningRef.current = runningId;
  const hostRef = useRef<HTMLDivElement>(null);
  /** Typed since the last Enter, for the prompt tally. */
  const pendingPrompt = useRef("");
  // Held so the pane's own handlers (focus restore, select mode, drops) can reach
  // the live terminal.
  const xtermRef = useRef<Xterm | null>(null);
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
  // Image paste. `dropping` only drives the overlay; `sending` is what the
  // operator needs to see, because a screenshot to a remote host takes long
  // enough that silence reads as nothing having happened.
  const [dropping, setDropping] = useState(false);
  // Set while the connection is being rebuilt after a drop. Distinct from the
  // first-load state: this pane already has content, and blanking it would
  // throw away the conversation the operator is reading.
  const [reconnecting, setReconnecting] = useState(false);
  const reconnectingRef = useRef(false);
  // Bumping this re-runs the mount effect, which is what actually rebuilds the
  // terminal and the attachment. Nothing else in the pane can restart it: the
  // setup effect has no dependencies by design, so that an ordinary re-render
  // never tears down a live conversation.
  const [generation, setGeneration] = useState(0);
  const [sending, setSending] = useState<string | null>(null);

  /**
   * Put images on the target and type their paths into Claude.
   *
   * The paths are *typed*, not submitted — same rule as a browser report. The
   * operator says what to do with the screenshot; rmux only carries it.
   */
  const sendImages = async (images: File[]) => {
    const id = runningRef.current;
    if (!id) {
      setError("Claude is not running in this session yet.");
      return;
    }

    setError(null);
    setSending(
      images.length === 1 ? "Sending the image…" : `Sending ${images.length} images…`,
    );

    try {
      // Sequential on purpose. Each upload streams its bytes over the same ssh
      // connection, so firing them together would interleave several megabytes
      // on one channel and finish no sooner.
      const paths: string[] = [];
      for (const image of images) {
        paths.push((await uploadImage(target, image)).path);
      }
      await invoke("claude_write", { id, data: promptFor(paths) });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSending(null);
    }
  };

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
      theme: claudeTheme(),
      allowTransparency: true,
      // The operator's chosen mono font (ADR-003) — same live-token read and
      // event-driven push as the terminal tab.
      fontFamily: readMonoStack(),
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
    // Skipped entirely when the operator has turned GPU rendering off — see
    // `gpuRendering`. Loading the addon and disposing it still creates and
    // loses a WebGL context, which is worse than never loading it.
    try {
      if (!gpuRendering()) throw new Error("gpu rendering is off");
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      xterm.loadAddon(webgl);
    } catch {
      // Canvas/DOM fallback is automatic; slower, but it renders.
    }

    // Copy and paste are xterm's own; this adds select-all, and off macOS the
    // Ctrl+C rule. Claude's TUI turns on mouse reporting, so a plain drag goes
    // to Claude rather than selecting — hold Option (macOS) or **Shift**
    // (Windows/Linux) to select, or use SELECT in the header. Returns a
    // disposer; see the note in Terminal.tsx.
    const clipboard = attachClipboard(xterm);

    // Vietnamese and every other IME that rewrites what it already typed.
    // macOS Telex fires no composition events at all — it inserts the plain
    // letter and then replaces it via `insertReplacementText`, which xterm
    // drops. Measured in this app; see `lib/ime-replace.ts`.
    const ime = attachImeReplace(host.querySelector("textarea")!, (data) => {
      if (id) void invoke("claude_write", { id, data: data.normalize("NFC") });
    });



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
      // Bytes are arriving, so whatever broke is fixed. Cleared here rather than
      // when the attach call returns: the call can succeed against a connection
      // that dies a moment later, and the operator would be told it recovered
      // when it had not.
      if (reconnectingRef.current) {
        reconnectingRef.current = false;
        setReconnecting(false);
      }
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
            skipPermissions,
            modelProfile,
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
      // **NFC, because macOS composes Vietnamese decomposed.**
      //
      // `ế` is one code point precomposed (U+1EBF) and three decomposed
      // (`e` + U+0302 + U+0301). macOS's text system hands over the decomposed
      // form, and a terminal program receiving it sees a letter followed by two
      // separate combining marks — so cursor arithmetic, width measurement and
      // any per-character editing all disagree with what is on screen. The
      // reported symptom is simply "Vietnamese does not work".
      //
      // Normalising costs nothing for ASCII (`normalize` returns the same
      // string) and is the form a terminal should receive in any case.
      // **A prompt is Enter on a line with something on it**, counted the same
      // way the terminal counts commands — Claude's own transcript is the real
      // record of what was asked, but it lags by a turn and cannot see a prompt
      // that is still being typed. Both are needed: this one is live, the
      // transcript is history.
      if (data === "\r" || data === "\n") {
        if (pendingPrompt.current.trim()) record(sessionId, "prompts");
        pendingPrompt.current = "";
      } else if (data === "\x7f") {
        pendingPrompt.current = pendingPrompt.current.slice(0, -1);
      } else if (!data.startsWith("\x1b")) {
        pendingPrompt.current += data;
      }
      if (id) void invoke("claude_write", { id, data: data.normalize("NFC") });
    });

    // Fit locally on every callback so the grid tracks the window, but tell
    // Claude only once the drag stops — see `settleResize` for what forty
    // redraws during one drag does to an inline TUI.

    const settle = settleResize(
      () => fit.fit(),
      () => {
        if (id) void invoke("claude_resize", { id, cols: xterm.cols, rows: xterm.rows });
      },
    );

    const observer = new ResizeObserver(() => {
      if (host.clientWidth === 0 || host.clientHeight === 0) return;
      if (!id) {
        startClaude();
        return;
      }
      settle.observe();
    });
    observer.observe(host);

    // **Refit when the interface scale changes, not only when the pane
    // resizes.** The `ResizeObserver` above is the normal path, but it bails on
    // a zero-sized host — correct while a pane is hidden — and an appearance
    // change arriving from the *Settings window* does not necessarily move this
    // element in a way that fires it. The terminal was then left at the cell
    // grid it had computed earlier: content filling part of the pane, the rest
    // empty, and nudging any slider "fixed" it because that finally produced a
    // resize. A window that has to be jiggled to look right is the definition
    // of not responsive.
    const refit = () => {
      if (host.clientWidth === 0 || host.clientHeight === 0) return;
      fit.fit();
      if (id) void invoke("claude_resize", { id, cols: xterm.cols, rows: xterm.rows });
    };
    // Two frames: the first lets the zoom land, the second lets layout settle
    // before xterm measures a cell.
    const onAppearance = (event: StorageEvent) => {
      if (event.key && event.key !== "rmux.appearance") return;
      // The palette is derived from the live design tokens, so a colour chosen
      // in Settings has to be pushed into xterm — it does not re-read CSS on
      // its own. Without this the terminals keep the colours they were born
      // with, which is the whole complaint being fixed. Same for the mono font
      // (ADR-003): the token changed, but xterm cached the family.
      xterm.options.theme = claudeTheme();
      xterm.options.fontFamily = readMonoStack();
      requestAnimationFrame(() => requestAnimationFrame(refit));
    };
    // The ANSI theme *or the font* changed in this document — push both in, like
    // above (`applyFonts` dispatches this for the live font preview).
    const onTheme = () => {
      xterm.options.theme = claudeTheme();
      xterm.options.fontFamily = readMonoStack();
      requestAnimationFrame(() => requestAnimationFrame(refit));
    };
    window.addEventListener("storage", onAppearance);
    window.addEventListener("resize", refit);
    window.addEventListener("rmux-theme", onTheme);


    if (host.clientWidth > 0 && host.clientHeight > 0) startClaude();

    return () => {
      disposed = true;
      observer.disconnect();
      clipboard();
      ime.dispose();
      settle.dispose();
      window.removeEventListener("storage", onAppearance);
      window.removeEventListener("resize", refit);
      window.removeEventListener("rmux-theme", onTheme);
      onData.dispose();
      xtermRef.current = null;
      xterm.dispose();
      // The session is deliberately NOT stopped: Claude may be mid-task, and
      // this view may simply be remounting. It is stopped only when the coding
      // session is closed.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [generation]);

  /**
   * Rebuild a connection that dropped.
   *
   * Guarded by a ref rather than by state: the poll runs every 400ms, and a
   * `reconnecting` flag read from a closure would still be `false` on the next
   * three ticks — firing four reattaches for one drop.
   *
   * The handle is cleared first. It names an attachment inside the *previous*
   * ssh connection; asking to reattach to it yields "no such session" forever,
   * which is the shape of a pane that never comes back.
   */
  const reconnect = () => {
    if (reconnectingRef.current) return;
    reconnectingRef.current = true;
    setReconnecting(true);
    clearClaudeSession(sessionId);

    // A beat before retrying. A machine waking from sleep has an interface that
    // is up but not yet routable, so an immediate reattach fails on DNS or a
    // refused connection and simply burns the attempt.
    setTimeout(() => setGeneration((n) => n + 1), 1200);
  };

  // Poll the screen. Cheap — it reads an in-memory emulator, no network.
  useEffect(() => {
    if (!claudeId) return;
    let cancelled = false;

    const tick = async () => {
      try {
        const next = await invoke<ClaudeState & { exited?: number }>("claude_state", {
          id: claudeId,
        });
        if (cancelled) return;

        // **The connection died.** After a laptop sleeps, ssh drops and the
        // attach process exits — the pane then sits there fed by a stream that
        // has simply stopped, looking like a Claude that went quiet. It is not:
        // nothing will ever arrive again, and typing goes nowhere.
        //
        // The conversation itself is untouched. It runs under `rmux-agent` on
        // the target precisely so it survives this, so reattaching by name
        // brings it back whole — scrollback included.
        if (next.exited != null) {
          setState({ prompt: null, working: false });
          setStatus(sessionId, "idle");
          reconnect();
          return;
        }

        setState(next);
        // Publish to the rail. This is what makes "which session needs me?"
        // answerable at a glance without opening each one.
        setStatus(sessionId, next.prompt ? "waiting" : next.working ? "working" : "idle");
      } catch {
        // The handle is gone entirely — the same outcome, reached differently.
        if (!cancelled) reconnect();
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
    <div
      className="relative flex h-full flex-col"
      // Capture, so this runs before xterm's own paste handling on the textarea.
      // Text pastes are left entirely alone — `imagesFrom` returns nothing for
      // them and `preventDefault` is never called, so xterm handles them as it
      // always has. Intercepting text here would be a second implementation of
      // paste, which is exactly the bug that made every paste arrive twice.
      onPasteCapture={(e) => {
        const images = imagesFrom(e.clipboardData);
        if (!images.length) return;
        e.preventDefault();
        void sendImages(images);
      }}
      onDragOver={(e) => {
        if (!e.dataTransfer.types.includes("Files")) return;
        e.preventDefault();
        setDropping(true);
      }}
      onDragLeave={(e) => {
        // Only when the pointer actually left the pane. `dragleave` also fires
        // crossing into a child, which would flicker the overlay constantly.
        if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
        setDropping(false);
      }}
      onDrop={(e) => {
        const images = imagesFrom(e.dataTransfer);
        setDropping(false);
        if (!images.length) return;
        e.preventDefault();
        void sendImages(images);
      }}
      // **Clicking the pane gives the keyboard back to Claude.**
      //
      // The terminal was focused once, at start. Use any control in the header,
      // or click the padding around the rows, and focus
      // moved away with nothing to bring it back. From then on a keystroke went
      // to whatever the browser considered focused, which for **Space** means
      // scrolling the nearest scrollable ancestor: xterm's own viewport, jumping
      // to the bottom. Reported as "space scrolls instead of ticking the box",
      // and it is worst in exactly the situation that produces it — a window too
      // small to show a multiple-choice question, so you scroll up to read it.
      //
      // `mousedown`, not `click`, so focus is restored before the key is pressed
      // rather than after. Interactive controls are skipped: focusing the
      // terminal on the way into a button would take focus straight back off it.
      onMouseDown={(e) => {
        if ((e.target as HTMLElement).closest("button, a, input, textarea, select")) return;
        xtermRef.current?.focus();
      }}
    >
      <header
        className="flex shrink-0 items-center gap-3 border-b px-3 py-1"
        style={{ borderColor: "var(--border)" }}
      >
        {/* Truncated, never wrapped: a long name must not push the status strip
            onto a second line — the header's height is what `fit()` measures, so
            a wrapped header silently costs the terminal a row. */}
        <span
          className="micro truncate"
          style={{ color: "var(--text)", maxWidth: "34%", flex: "0 1 auto" }}
          title={cwd ? `${cwd}${sessionName ? `\n${sessionName}` : ""}` : (sessionName ?? "Claude")}
        >
          {paneTitle.toUpperCase()}
        </span>

        {/* The session's own name, second and dimmer — several sessions share a
            folder, so the title alone cannot tell them apart. Dropped first when
            the header runs out of room, because the status strip beside it is
            live data and this is a label. */}
        {sessionName && sessionName !== paneTitle && (
          <span
            className="micro truncate"
            style={{ color: "var(--text-faint)", maxWidth: "30%", flex: "0 1 auto" }}
            title={sessionName}
          >
            {sessionName}
          </span>
        )}

        <ClaudeStatusStrip status={claudeStatus} window={contextWindow} />

        {/*
          **No copy buttons, and no rendering toggle.**

          The copy pair existed because Claude's fullscreen TUI captured the
          mouse: a drag went *to Claude* rather than making a selection, so
          Cmd-C had nothing to copy and the buttons were the only way out. rmux
          now runs Claude **inline** with `CLAUDE_CODE_DISABLE_MOUSE=1`, so
          dragging selects and Cmd-C works as it does anywhere else. Keeping
          them would leave two chips in the busiest header in the app
          duplicating a gesture the operator already has.

          The fullscreen toggle lives in Session Settings. It restarts the
          conversation to take effect -- not a thing to sit one stray click from
          INTERRUPT, and a per-session property decided once rather than a
          control reached mid-run.
        */}
        {/* Select mode is meaningful only in Claude's fullscreen TUI, which
            captures the mouse; turning capture off locally lets a drag select
            again. It stays hidden in the default inline rendering. */}
        {fullscreen && (
          <div className="flex items-center gap-2">
            <button
              type="button"
              className="chip"
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
          </div>
        )}

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

        {headerActions && <div className="ml-auto flex items-center gap-1">{headerActions}</div>}
      </header>

      {/* What a connected browser sent back for this session. Empty, and takes
          no space, until something arrives. */}
      <BrowserReports sessionId={sessionId} claudeId={claudeId} />

      {reconnecting && (
        <div
          className="flex shrink-0 items-center gap-2 border-b px-3 py-1"
          style={{ borderColor: "var(--border)", background: "var(--hover)" }}
        >
          <span className="micro" style={{ color: "rgb(var(--busy))" }}>
            CONNECTION LOST — REATTACHING TO {(target.host ?? "this machine").toUpperCase()}
          </span>
          {/* Said out loud, because the alternative reading is "my work is
              gone". It is not: the conversation runs under the agent on the
              target and survives this. */}
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            YOUR CONVERSATION IS STILL RUNNING THERE
          </span>
        </div>
      )}

      {/* An image on its way to the target. Worth saying out loud: a screenshot
          over ssh takes seconds, and Claude's pane looks identical while it
          happens. */}
      {sending && (
        <div
          className="flex shrink-0 items-center gap-2 border-b px-3 py-1"
          style={{ borderColor: "var(--border)", background: "var(--hover)" }}
        >
          <span className="micro" style={{ color: "rgb(var(--busy))" }}>
            {sending.toUpperCase()}
          </span>
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            → {target.host ?? "this machine"}
          </span>
        </div>
      )}

      {/* The drop target. Pointer-events off so it cannot swallow the drop it
          is advertising. */}
      {dropping && (
        <div
          className="pointer-events-none absolute inset-0 z-20 grid place-items-center"
          style={{
            background: "color-mix(in srgb, var(--app-panel) 70%, transparent)",
            outline: "1px dashed var(--border-strong)",
            outlineOffset: -6,
          }}
        >
          <span className="micro" style={{ color: "var(--text)" }}>
            DROP TO SEND TO {(target.host ?? "this machine").toUpperCase()}
          </span>
        </div>
      )}

      <div className="claude-pane-area relative min-h-0 flex-1">
        {/*
          **The padding is on a wrapper, never on the element `fit()` measures.**
          This was the black bar along the bottom of the pane, and it is a layout
          fault rather than the transparency one it looks like.

          `FitAddon` reads `getComputedStyle(host).height` — and under
          `box-sizing: border-box`, which Tailwind sets globally, the resolved
          `height` is the *padding* box. It compensates by subtracting padding,
          but it reads that padding from `.xterm`, which has none: the addon
          assumes any padding is on the terminal element itself. With `p-2` here,
          16px of padding was counted as terminal space and never taken back, so
          `fit()` asked for one row more than the pane could hold — measured at
          25 rows x 19.5px = 487.5px into 476px, hanging 4px past the pane edge.
          The overhanging viewport is what painted the bar, and it carried the
          scrollbar thumb that was visible inside it.

          Two rounds of `background: transparent` could not have worked: it was
          real content in the wrong place, not an unpainted gap.

          The wrapper also cannot be the `relative` element — the overlays below
          position against it, and padding it would inset them by 8px.

          `ui/blackbar-check.ts` measures this against a real xterm and fails on
          the old structure.
        */}
        {/*
          **No padding along the bottom.** `fit()` floors to whole rows, so a
          remainder of up to one cell is always left below the last row and no
          arithmetic removes it. Eight more pixels of padding under it made that
          bare region half again as tall for no gain — the rows are already clear
          of the pane edge by the remainder itself. Under native glass with a low
          overlay the panel paints nothing there, so bare is *visible*: measured
          in the running app, `.panel` resolves to `rgba(0, 0, 0, 0)` from the top
          of the pane to the bottom, which makes any region the terminal does not
          cover read as a hole rather than as part of the app.
        */}
        <div className="claude-pane-host h-full w-full px-2 pt-2">
          <div ref={hostRef} className="h-full w-full" />
        </div>

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
