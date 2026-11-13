import { Suspense, lazy, useEffect, useState } from "react";

import { invoke } from "../lib/bridge";
import { useWorkspace } from "../lib/workspace";
import { agentState } from "../lib/agent-state";
import { isChatKind, serverLabel } from "../lib/mobile-sessions";
import { QUICK_KEYS } from "../lib/composer-keys";
import type { SessionV3 } from "../lib/workspace-model";
import { PanelLoader } from "../components/PanelLoader";
import { ChatView } from "./ChatView";
import { Composer, errorText } from "./Composer";
import { BackIcon, ChatIcon, StopIcon, TerminalIcon } from "./icons";
import { SHORT_STATE, STATE_LABEL, StateDot } from "./StateDot";
import type { View } from "./route";

/**
 * The desktop's own session pane, reused as it is for TERMINAL: the same attach
 * path (`claude_start`/`pi_start`/`codex_start`/`terminal_open` by the session's
 * daemon name), the same xterm, fitted to whatever width the phone gives it.
 * Loaded lazily — it carries the whole terminal stack.
 */
const SessionPane = lazy(() => import("../components/WorkspaceDeck").then((m) => ({ default: m.SessionPane })));

/** How long "connecting" may last before the operator is told where to look. */
const SLOW_CONNECT_MS = 10_000;

type KeyOutcome = { kind: "idle" } | { kind: "busy"; what: string } | { kind: "done"; what: string } | { kind: "error"; message: string };

/**
 * One session, opened.
 *
 * For an agent the chat is the view and the terminal pane stays **mounted
 * underneath**, hidden: that pane is what attaches to the session and yields the
 * handle the composer sends through, exactly as it does on the desktop. Hidden
 * with `visibility` (not `display: none`), so it still has a size to fit to and
 * cannot take focus — a hidden terminal grabbing the keyboard from the composer
 * would send the operator's words into the wrong place.
 *
 * A terminal session has no chat: its pane is the whole screen.
 */
export function ChatScreen({
  session,
  view,
  onView,
  onBack,
}: {
  session: SessionV3;
  view: View;
  onView: (v: View) => void;
  onBack: () => void;
}) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const runtime = useWorkspace((s) => s.runtime[session.id]);
  const seenAt = useWorkspace((s) => s.seen[session.id]);
  const handle = useWorkspace((s) => s.live[session.id]);
  const activate = useWorkspace((s) => s.activate);

  const chat = isChatKind(session.kind);
  const showing: View = chat ? view : "terminal";
  const state = agentState(session.kind, runtime, seenAt);
  const folder = project?.folder ?? "";
  // pi and Codex read their transcript under the conversation's own cwd, as
  // their desktop panes do; Claude under the project folder.
  const transcriptFolder = session.kind === "claude" ? folder : (session.cwd ?? folder);
  const where = `${serverLabel(target)} · ${transcriptFolder || "no folder"}`;

  const [refreshNonce, setRefreshNonce] = useState(0);
  const [keys, setKeys] = useState<KeyOutcome>({ kind: "idle" });
  const [slow, setSlow] = useState(false);

  // Looking at it is what acknowledges it — the same watermark the desktop
  // advances when a pane comes to the front. Never the keyboard: on a phone that
  // would raise the on-screen keyboard over the conversation.
  useEffect(() => {
    if (document.visibilityState === "visible") activate(session.id, { keyboard: false });
  }, [activate, session.id, runtime?.lastStatusAt]);

  useEffect(() => {
    if (handle) {
      setSlow(false);
      return;
    }
    const timer = window.setTimeout(() => setSlow(true), SLOW_CONNECT_MS);
    return () => window.clearTimeout(timer);
  }, [handle]);

  useEffect(() => {
    if (keys.kind !== "done") return;
    const timer = window.setTimeout(() => setKeys((k) => (k.kind === "done" ? { kind: "idle" } : k)), 2500);
    return () => window.clearTimeout(timer);
  }, [keys]);

  /** Keystrokes, by name, through `claude_keys` — Rust's one key table. */
  const press = async (names: string[], what: string) => {
    if (!handle) return;
    setKeys({ kind: "busy", what });
    try {
      await invoke("claude_keys", { id: handle, keys: names });
      setKeys({ kind: "done", what });
    } catch (e) {
      setKeys({ kind: "error", message: errorText(e) });
    }
  };

  const interrupt = async () => {
    if (!handle) return;
    setKeys({ kind: "busy", what: "Esc" });
    try {
      await invoke("claude_interrupt", { id: handle });
      setKeys({ kind: "done", what: "Esc" });
    } catch (e) {
      setKeys({ kind: "error", message: errorText(e) });
    }
  };

  // The quick keys are keystrokes, not a copy of Claude's dialog: offered while
  // it is asking something, and always in TERMINAL, where a phone keyboard has
  // no Esc or arrows of its own.
  const quickKeys = chat && (state === "blocked" || showing === "terminal");
  const keyStatus =
    keys.kind === "error"
      ? `Key not sent: ${keys.message}`
      : keys.kind === "busy"
        ? `sending ${keys.what}…`
        : keys.kind === "done"
          ? `sent ${keys.what}`
          : !handle
            ? "connecting to the session…"
            : "";

  return (
    <div className="m-chat-screen">
      <header className="m-top m-chat-top">
        <button type="button" className="m-icon-btn m-back" onClick={onBack} aria-label="Back to sessions">
          <BackIcon />
        </button>
        <div className="m-heading">
          <h1 className="m-title">{session.name}</h1>
          <p className="m-subtitle" title={STATE_LABEL[state ?? "unknown"]}>
            {state && <StateDot state={state} />}
            <span className="m-subtitle-text">
              {state ? `${SHORT_STATE[state]} · ` : ""}
              {project?.label ?? ""} · {serverLabel(target)}
            </span>
          </p>
        </div>
        {chat && (
          <>
            <button
              type="button"
              className="m-icon-btn"
              onClick={() => void interrupt()}
              disabled={!handle || keys.kind === "busy"}
              aria-label="Interrupt (Esc)"
              title="Interrupt (Esc)"
            >
              <StopIcon />
            </button>
            <button
              type="button"
              className="m-icon-btn"
              aria-pressed={showing === "terminal"}
              onClick={() => onView(showing === "terminal" ? "chat" : "terminal")}
              aria-label={showing === "terminal" ? "Show the conversation" : "Show the terminal"}
              title={showing === "terminal" ? "Conversation" : "Terminal"}
            >
              {showing === "terminal" ? <ChatIcon /> : <TerminalIcon />}
            </button>
          </>
        )}
      </header>

      <div className="m-body">
        {chat && (
          <div className="m-layer" data-shown={showing === "chat"} inert={showing !== "chat"}>
            <ChatView
              provider={session.kind as "claude" | "pi" | "codex"}
              target={target}
              folder={transcriptFolder}
              resume={session.resume}
              where={where}
              status={runtime?.status}
              refreshNonce={refreshNonce}
            />
          </div>
        )}
        <div className="m-layer m-terminal" data-shown={showing === "terminal"} inert={showing !== "terminal"}>
          <Suspense fallback={<PanelLoader phase="loading the terminal" detail={where} />}>
            <SessionPane session={session} />
          </Suspense>
        </div>
      </div>

      {chat && slow && !handle && (
        <p className="m-note m-note-bar" role="status">
          Still connecting to the session on {serverLabel(target)}. Open Terminal to see what it says.
        </p>
      )}

      {quickKeys && (
        <div className="m-keys" role="group" aria-label="Keys for the session">
          {QUICK_KEYS.map((k) => (
            <button
              key={k.name}
              type="button"
              className="m-key"
              aria-label={k.aria}
              disabled={!handle || keys.kind === "busy"}
              onClick={() => void press([k.name], k.label)}
            >
              {k.label}
            </button>
          ))}
        </div>
      )}
      {chat && (quickKeys || keys.kind !== "idle") && keyStatus && (
        <p className={`m-composer-status m-keys-status${keys.kind === "error" ? " m-error" : ""}`} role={keys.kind === "error" ? "alert" : "status"}>
          {keyStatus}
        </p>
      )}

      {/* Kept mounted in TERMINAL so a half-written message survives a look at
          the terminal; only hidden. */}
      {chat && (
        <div className="m-composer-slot" hidden={showing !== "chat"}>
          <Composer
            handle={handle}
            agent={session.kind as "claude" | "pi" | "codex"}
            name={session.name}
            onSent={() => setRefreshNonce((n) => n + 1)}
          />
        </div>
      )}
    </div>
  );
}
