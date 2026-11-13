import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import { hasBackend, type TargetRef, type Transcript } from "../lib/api";
import { MARKDOWN_COMPONENTS } from "../lib/markdown-code";
import {
  TAIL_STEP,
  hasSelectionWithin,
  humanBytes,
  readTranscript,
  sameTranscript,
  type TranscriptProvider,
} from "../lib/transcript-read";
import { shapeBubbles, toolsLabel, type Bubble } from "../lib/chat-bubbles";
import { isNearBottom, mayFetch, planUpdate } from "../lib/chat-scroll";
import type { SessionStatus } from "../lib/workspace-model";
import { PanelLoader } from "../components/PanelLoader";
import { DownIcon } from "./icons";

/** While a turn is running the transcript grows with it; re-read this often,
 *  and only then (an idle conversation re-reads nothing). */
const WORKING_POLL_MS = 8000;

/** Tables scroll sideways inside their own box instead of widening the page;
 *  links open in the real browser. Everything else is the shared definition. */
const COMPONENTS: Components = {
  ...MARKDOWN_COMPONENTS,
  table({ children }) {
    return (
      <div className="m-table-wrap">
        <table>{children}</table>
      </div>
    );
  },
  a({ children, href }) {
    return (
      <a href={href} target="_blank" rel="noopener noreferrer">
        {children}
      </a>
    );
  },
};

const clock = (iso?: string) => {
  if (!iso) return "";
  const at = new Date(iso);
  return Number.isNaN(at.getTime()) ? "" : at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
};

const AGENT_NAME: Record<TranscriptProvider, string> = { claude: "Claude", pi: "pi", codex: "Codex" };

function BubbleView({ bubble, provider }: { bubble: Bubble; provider: TranscriptProvider }) {
  if (bubble.kind === "tools") {
    const last = bubble.calls.at(-1)!;
    return (
      <details className="m-tools">
        <summary>
          <span className="m-tools-label">{toolsLabel(bubble.calls)}</span>
          <span className="m-tools-summary">{last.summary}</span>
        </summary>
        <div className="m-tools-body">
          {bubble.calls.map((c) => (
            <div key={c.key} className="m-tool">
              <div className="m-tool-name">{c.tool || "tool"}</div>
              {/* Shell and file contents: whitespace matters, and a stray
                  backtick must not become formatting. */}
              <pre className="m-tool-text">{c.text}</pre>
            </div>
          ))}
        </div>
      </details>
    );
  }
  const who = bubble.role === "user" ? "You" : AGENT_NAME[provider];
  return (
    <article className={`m-msg m-msg-${bubble.role}`} aria-label={`${who}${bubble.timestamp ? `, ${clock(bubble.timestamp)}` : ""}`}>
      <div className="m-bubble markdown">
        <Markdown remarkPlugins={[remarkGfm]} components={COMPONENTS}>
          {bubble.text}
        </Markdown>
      </div>
      <div className="m-meta">
        {who}
        {bubble.timestamp ? ` · ${clock(bubble.timestamp)}` : ""}
      </div>
    </article>
  );
}

/**
 * The conversation as chat bubbles, read from the agent's own transcript.
 *
 * The same reader as the desktop's `TranscriptView` (`transcript-read.ts`) —
 * tail only, never the whole file — and the same "nothing moves under your
 * hands" rules, made stricter for a phone: a re-read is applied only when the
 * reader is at the bottom; scrolled up, it waits behind a "new messages" pill;
 * selecting text, it is not even fetched until they let go.
 */
export function ChatView({
  provider,
  target,
  folder,
  resume,
  where,
  status,
  refreshNonce,
}: {
  provider: TranscriptProvider;
  target: TargetRef;
  folder: string;
  resume?: string;
  /** Host and folder, for the loading line — over SSH the host is why it is slow. */
  where: string;
  status: SessionStatus | undefined;
  /** Bumped after the operator sends: re-read, and go to the end — they just
   *  spoke, so following the reply is what they asked for. */
  refreshNonce: number;
}) {
  const [shown, setShown] = useState<Transcript | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [tail, setTail] = useState(TAIL_STEP);
  const [pill, setPill] = useState(false);
  const [selecting, setSelecting] = useState(false);

  const scrollRef = useRef<HTMLDivElement>(null);
  const shownRef = useRef<Transcript | null>(null);
  /** A newer read held back because applying it would move the view. */
  const heldRef = useRef<Transcript | null>(null);
  const atBottomRef = useRef(true);
  const followRef = useRef(true);
  /** Distance from the bottom to restore after older turns load in front. */
  const anchorRef = useRef<number | null>(null);
  const deferredRef = useRef(false);
  const inFlightRef = useRef(false);
  const againRef = useRef<string | null>(null);

  const params = useRef({ provider, target, folder, resume, tail });
  params.current = { provider, target, folder, resume, tail };

  const apply = useCallback((next: Transcript, follow: boolean) => {
    followRef.current = follow;
    shownRef.current = next;
    heldRef.current = null;
    setPill(false);
    setShown(next);
  }, []);

  const load = useCallback(
    async (reason: string) => {
      if (!hasBackend()) {
        setError("The conversation needs rmux-web behind this page.");
        return;
      }
      const el = scrollRef.current;
      if (!mayFetch({ selecting: hasSelectionWithin(el), hidden: document.visibilityState === "hidden" })) {
        // Re-rendering under a selection wipes it; take the update once they let go.
        if (hasSelectionWithin(el)) {
          deferredRef.current = true;
          setSelecting(true);
        }
        return;
      }
      if (inFlightRef.current) {
        againRef.current = reason;
        return;
      }
      inFlightRef.current = true;
      setLoading(true);
      try {
        const p = params.current;
        const next = await readTranscript(p.provider, p.target, p.folder, p.resume, p.tail);
        setError(null);
        if (sameTranscript(heldRef.current ?? shownRef.current, next)) return;
        if (!shownRef.current) {
          apply(next, true);
        } else if (reason === "more") {
          if (el) anchorRef.current = el.scrollHeight - el.scrollTop;
          apply(next, false);
        } else if (reason === "sent") {
          apply(next, true);
        } else {
          const plan = planUpdate({ atBottom: atBottomRef.current, selecting: hasSelectionWithin(el) });
          if (plan === "apply-follow") apply(next, true);
          else {
            heldRef.current = next;
            if (plan === "hold-pill") setPill(true);
            else {
              deferredRef.current = true;
              setSelecting(true);
            }
          }
        }
      } catch (e) {
        setError(typeof e === "string" ? e : e instanceof Error ? e.message : String(e));
      } finally {
        inFlightRef.current = false;
        setLoading(false);
        const again = againRef.current;
        againRef.current = null;
        if (again) void load(again);
      }
    },
    [apply],
  );

  // Open, and again whenever the conversation identity changes (a learned resume id).
  useEffect(() => {
    void load("open");
  }, [load, provider, target, folder, resume]);

  // Older turns: the tail doubled.
  useEffect(() => {
    if (tail !== TAIL_STEP) void load("more");
  }, [tail, load]);

  // A turn ended or the agent paused for input: the transcript just grew.
  useEffect(() => {
    if (status === "idle" || status === "waiting") void load(status);
  }, [status, load]);

  // While working, follow along slowly — and only while anyone can see it.
  useEffect(() => {
    if (status !== "working") return;
    const timer = window.setInterval(() => void load("working"), WORKING_POLL_MS);
    return () => window.clearInterval(timer);
  }, [status, load]);

  useEffect(() => {
    if (refreshNonce === 0) return;
    atBottomRef.current = true;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
    // Give the agent a moment to write the turn before reading it back.
    const timer = window.setTimeout(() => void load("sent"), 1200);
    return () => window.clearTimeout(timer);
  }, [refreshNonce, load]);

  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") void load("visible");
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [load]);

  // Take a deferred update the moment the selection clears.
  useEffect(() => {
    const onSelection = () => {
      if (!deferredRef.current || hasSelectionWithin(scrollRef.current)) return;
      deferredRef.current = false;
      setSelecting(false);
      const held = heldRef.current;
      if (held) {
        if (atBottomRef.current) apply(held, true);
        else setPill(true);
      } else void load("flush");
    };
    document.addEventListener("selectionchange", onSelection);
    return () => document.removeEventListener("selectionchange", onSelection);
  }, [apply, load]);

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el || !shown) return;
    if (anchorRef.current != null) {
      el.scrollTop = el.scrollHeight - anchorRef.current;
      anchorRef.current = null;
    } else if (followRef.current) {
      el.scrollTop = el.scrollHeight;
      atBottomRef.current = true;
    }
  }, [shown]);

  const { bubbles, hiddenSystem } = useMemo(() => shapeBubbles(shown?.entries ?? []), [shown]);
  const more = !!shown && shown.totalBytes > shown.readBytes;

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    atBottomRef.current = isNearBottom(el);
    // Reaching the end on their own is the same as tapping the pill.
    if (atBottomRef.current && heldRef.current && !hasSelectionWithin(el)) apply(heldRef.current, true);
  };

  return (
    <div className="m-chat">
      <div
        ref={scrollRef}
        className="m-scroll selectable"
        onScroll={onScroll}
        aria-live="off"
        // Focusable so a hardware keyboard can scroll the conversation.
        tabIndex={0}
        aria-label="Conversation"
      >
        <div className="m-thread">
          {more && (
            <button
              type="button"
              className="m-btn m-more"
              disabled={loading}
              onClick={() => setTail((t) => t * 2)}
            >
              {loading ? "loading earlier messages…" : `Load earlier messages (${humanBytes(shown!.readBytes)} of ${humanBytes(shown!.totalBytes)} read)`}
            </button>
          )}

          {error && (
            <div className="m-alert" role="alert">
              <p>{shown ? `Could not refresh the conversation: ${error}` : `Could not read the conversation: ${error}`}</p>
              <button type="button" className="m-btn" onClick={() => void load("retry")} disabled={loading}>
                {loading ? "retrying…" : "Retry"}
              </button>
            </div>
          )}

          {!shown && !error && <PanelLoader variant="rows" phase="reading the conversation" detail={where} rows={6} />}

          {bubbles.map((b) => (
            <BubbleView key={b.key} bubble={b} provider={provider} />
          ))}

          {shown && !bubbles.length && (
            <div className="m-empty">
              <p className="m-empty-title">Nothing said in this conversation yet</p>
              <p className="m-empty-body">
                {hiddenSystem
                  ? `${hiddenSystem} system ${hiddenSystem === 1 ? "note is" : "notes are"} hidden. Send a message below to start.`
                  : "Send a message below to start."}
              </p>
            </div>
          )}

          {shown && status === "working" && (
            <p className="m-note" role="status">
              {AGENT_NAME[provider]} is working — the reply appears here as the turn is written.
            </p>
          )}
          {shown && status === "waiting" && (
            <p className="m-note m-note-blocked" role="status">
              {AGENT_NAME[provider]} is asking something. Answer with the keys below, or open Terminal to see the question.
            </p>
          )}
          {selecting && (
            <p className="m-note" role="status">
              Updates are paused while you select text.
            </p>
          )}
        </div>
      </div>

      {pill && (
        <button
          type="button"
          className="m-pill"
          onClick={() => {
            if (heldRef.current) apply(heldRef.current, true);
          }}
        >
          New messages <DownIcon />
        </button>
      )}
    </div>
  );
}
