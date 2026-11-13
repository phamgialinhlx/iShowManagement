import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { invoke } from "../lib/bridge";
import { canSend, composerHeight, composerKey } from "../lib/composer-keys";
import type { Agent } from "../lib/mobile-sessions";
import { SendIcon } from "./icons";

const AGENT_NAME: Record<Agent, string> = { claude: "Claude", pi: "pi", codex: "Codex" };

const LINE_PX = 22;
const PAD_PX = 20;
const MAX_LINES = 6;

type Outcome = { kind: "idle" } | { kind: "sending" } | { kind: "sent" } | { kind: "error"; message: string };

export const errorText = (e: unknown): string =>
  typeof e === "string" ? e : e instanceof Error ? e.message : typeof e === "object" && e && "message" in e ? String((e as { message: unknown }).message) : String(e);

/**
 * The message box.
 *
 * Sends through `claude_send` — the one Rust definition of submitting a message
 * (`rmux_claude::keys::send_message` / `send_message_pi`: the text, a settle,
 * then Enter as a separate write). Nothing here builds keystrokes.
 *
 * Enter is a newline; the button sends, and so does ⌘/Ctrl+Enter on a hardware
 * keyboard (`composer-keys.ts`). The outcome is reported beside the button and
 * an error stays until the next attempt.
 */
export function Composer({
  handle,
  agent,
  name,
  onSent,
}: {
  /** The live attach handle; absent while the session is still connecting. */
  handle: string | undefined;
  agent: Agent;
  /** The session's name, for the screen reader label; the placeholder names
   *  the agent, which fits on a phone. */
  name: string;
  onSent: () => void;
}) {
  const [text, setText] = useState("");
  const [outcome, setOutcome] = useState<Outcome>({ kind: "idle" });
  const ref = useRef<HTMLTextAreaElement>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${composerHeight(el.scrollHeight, LINE_PX, PAD_PX, MAX_LINES)}px`;
  }, [text]);

  useEffect(() => {
    if (outcome.kind !== "sent") return;
    const timer = window.setTimeout(() => setOutcome((o) => (o.kind === "sent" ? { kind: "idle" } : o)), 2500);
    return () => window.clearTimeout(timer);
  }, [outcome]);

  const sending = outcome.kind === "sending";
  const ready = !!handle && canSend(text) && !sending;

  const send = async () => {
    if (!handle || !canSend(text) || sending) return;
    const message = text;
    setOutcome({ kind: "sending" });
    try {
      await invoke("claude_send", { id: handle, text: message, agent });
      // Only what was sent is cleared — anything typed meanwhile stays.
      setText((t) => (t === message ? "" : t));
      setOutcome({ kind: "sent" });
      onSent();
    } catch (e) {
      setOutcome({ kind: "error", message: errorText(e) });
    }
  };

  const label = !handle ? "connecting…" : sending ? "sending…" : "Send";
  const status =
    outcome.kind === "error"
      ? `Not sent: ${outcome.message}`
      : outcome.kind === "sending"
        ? "sending…"
        : outcome.kind === "sent"
          ? "sent"
          : !handle
            ? "connecting to the session…"
            : "";

  return (
    <form
      className="m-composer"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      {status && (
        <p className={`m-composer-status${outcome.kind === "error" ? " m-error" : ""}`} role={outcome.kind === "error" ? "alert" : "status"}>
          {status}
        </p>
      )}
      <div className="m-composer-row">
        <textarea
          ref={ref}
          className="m-input"
          rows={1}
          value={text}
          placeholder={`Message ${AGENT_NAME[agent]}`}
          aria-label={`Message ${name}`}
          enterKeyHint="enter"
          autoCapitalize="sentences"
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (composerKey(e.nativeEvent) === "send") {
              e.preventDefault();
              void send();
            }
          }}
        />
        <button type="submit" className="m-send" disabled={!ready} aria-label={label}>
          {handle && !sending ? <SendIcon /> : <span className="m-send-wait">{label}</span>}
        </button>
      </div>
    </form>
  );
}
