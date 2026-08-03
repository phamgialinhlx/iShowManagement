import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { api, isTauri, type Transcript, type TranscriptEntry } from "../lib/api";
import type { Session } from "../lib/sessions";
import { MARKDOWN_COMPONENTS } from "../lib/markdown-code";

/**
 * The conversation, as a document.
 *
 * The Claude tab shows the TUI — a *rendering*, reflowed to the pane and redrawn
 * constantly. It is the right thing for working, and the wrong thing for reading:
 * you cannot scroll back through it reliably, and copying out of it gives you
 * whatever the screen happened to be showing, wrapped at the pane width.
 *
 * This is the same conversation as text. It reads the transcript on disk, so it
 * works for a session that is not even running, and everything in it is
 * selectable — the whole point is to be able to take a decision from an hour ago
 * and paste it somewhere.
 *
 * Transcripts reach hundreds of megabytes (a real one measured 228MB), so only
 * the tail is fetched and "load more" doubles it rather than fetching the lot.
 */

/**
 * How much of the tail to read at a time.
 *
 * 512KB sounded generous and was not: most of a Claude transcript is plumbing
 * — slash-command wrappers, caveat banners, system reminders — which this view
 * demotes and hides. On a real 220MB transcript that budget surfaced a single
 * visible message, which reads as "the transcript is broken" rather than "the
 * window is small". 2MB is a few hundred KB of actual conversation.
 */
const TAIL_STEP = 2 * 1024 * 1024;

const humanBytes = (bytes: number) => {
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)}${units[unit]}`;
};

const clock = (iso?: string) => {
  if (!iso) return "";
  const at = new Date(iso);
  return Number.isNaN(at.getTime())
    ? ""
    : at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
};

/** Who said it — colour and label. Red stays out of this entirely: nothing here
 *  is something the operator must act on, it has already happened. */
const SPEAKER: Record<TranscriptEntry["speaker"], { label: string; color: string }> = {
  user: { label: "you", color: "var(--text)" },
  assistant: { label: "claude", color: "var(--text)" },
  tool: { label: "tool", color: "var(--text-soft)" },
  system: { label: "system", color: "var(--text-faint)" },
};

function Turn({ entry }: { entry: TranscriptEntry }) {
  const meta = SPEAKER[entry.speaker];
  const isProse = entry.speaker === "user" || entry.speaker === "assistant";

  return (
    <article
      className="border-l pl-3"
      style={{
        borderColor: entry.speaker === "assistant" ? "var(--border-strong)" : "var(--border)",
        opacity: entry.speaker === "system" ? 0.62 : 1,
      }}
    >
      <header className="mb-1 flex items-baseline gap-2">
        <span className="micro" style={{ color: meta.color }}>
          {meta.label}
          {entry.tool ? ` · ${entry.tool}` : ""}
        </span>
        <span className="micro">{clock(entry.timestamp)}</span>
      </header>

      {isProse ? (
        <div className="markdown data text-[12.5px] leading-[1.65]">
          <Markdown remarkPlugins={[remarkGfm]} components={MARKDOWN_COMPONENTS}>
            {entry.text}
          </Markdown>
        </div>
      ) : (
        // Tool calls and output are not markdown — they are shell and file
        // contents, where whitespace is meaningful and a stray backtick must not
        // become formatting.
        <pre
          className="data overflow-x-auto text-[11.5px] leading-[1.5]"
          style={{ color: "var(--text-soft)", margin: 0, whiteSpace: "pre-wrap" }}
        >
          {entry.text}
        </pre>
      )}
    </article>
  );
}

/**
 * Is the operator selecting text inside this view right now?
 *
 * Guards every automatic DOM change here. `getSelection` is checked against the
 * container rather than globally, so a selection in another pane does not freeze
 * this one — and `isCollapsed` is what separates a real selection from a bare
 * caret, which every click leaves behind and which nobody would want to be
 * treated as "busy reading".
 */
function hasSelectionWithin(container: HTMLElement | null): boolean {
  if (!container) return false;
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return false;
  const range = selection.getRangeAt(0);
  return container.contains(range.commonAncestorContainer);
}

/**
 * Did the poll actually bring anything new?
 *
 * Compared by what the reader would notice — how much was read, how many turns,
 * and the last turn's text — rather than deep-equalled. A transcript tail is
 * append-only in practice, so the final entry changing is the signal, and this
 * runs every five seconds on a list that can be thousands of items long.
 */
function sameTranscript(a: Transcript | null, b: Transcript): boolean {
  if (!a) return false;
  if (a.readBytes !== b.readBytes || a.totalBytes !== b.totalBytes) return false;
  if (a.entries.length !== b.entries.length) return false;
  const last = a.entries.at(-1);
  const next = b.entries.at(-1);
  return last?.text === next?.text && last?.timestamp === next?.timestamp;
}

export function TranscriptView({ session }: { session: Session }) {
  const [transcript, setTranscript] = useState<Transcript | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [tail, setTail] = useState(TAIL_STEP);
  const [showSystem, setShowSystem] = useState(false);
  const [copied, setCopied] = useState(false);
  // Whether the automatic refresh is currently standing down for a selection.
  // Shown in the header — a view that silently stops updating is exactly the
  // "is this thing broken?" impression worth spending a label to avoid.
  const [held, setHeld] = useState(false);

  const scrollRef = useRef<HTMLDivElement>(null);
  // Only stick to the bottom when the reader is already there — yanking the view
  // down while someone is reading history is the worst thing a live log can do.
  const pinnedRef = useRef(true);

  const load = useCallback(async () => {
    if (!isTauri()) {
      setError("Transcripts need the rmux desktop shell.");
      return;
    }
    setLoading(true);
    try {
      const next = await api.claudeTranscript(session.target, session.folder, session.resume, tail);
      // Only swap state when something actually changed. A poll that returns
      // the same bytes used to replace the array anyway, re-rendering every
      // turn — which is invisible when nothing is selected and destroys the
      // selection when something is.
      setTranscript((prev) => (sameTranscript(prev, next) ? prev : next));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [session.target, session.folder, session.resume, tail]);

  useEffect(() => {
    void load();
    // Re-read while Claude is working. Cheap: it is a bounded `tail` on the far
    // side, not a re-read of the file.
    //
    // **Skipped while the reader has text selected.** This was the whole reason
    // selection "did not work" here: a five-second poll that re-rendered the
    // list wiped the selection, and a drag that crossed a tick was cancelled
    // outright — so the view behaved as though it were read-only. Nothing the
    // operator did not ask for may move under their hands; a transcript that is
    // a few seconds stale while they copy from it is the correct trade, and it
    // catches up the moment they click away.
    const timer = setInterval(() => {
      const selecting = hasSelectionWithin(scrollRef.current);
      setHeld(selecting);
      if (selecting) return;
      void load();
    }, 5000);
    return () => clearInterval(timer);
  }, [load]);

  const entries = useMemo(
    () => (transcript?.entries ?? []).filter((e) => showSystem || e.speaker !== "system"),
    [transcript, showSystem],
  );

  useEffect(() => {
    // Same rule as the poll: sticking to the bottom is a convenience, and it
    // loses to someone mid-drag every time.
    if (hasSelectionWithin(scrollRef.current)) return;
    if (pinnedRef.current && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [entries]);

  const copyAll = async () => {
    const text = entries
      .map((e) => `## ${SPEAKER[e.speaker].label}${e.tool ? ` · ${e.tool}` : ""}\n\n${e.text}`)
      .join("\n\n");
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2500);
    } catch {
      setError("could not write to the clipboard");
    }
  };

  const more = transcript && transcript.totalBytes > transcript.readBytes;

  return (
    <div className="flex h-full flex-col">
      <div
        className="flex shrink-0 items-center gap-3 border-b px-3 py-1"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="micro">{entries.length} messages</span>
        {/* Never leave the reader guessing whether the view is live. Three
            states, all named: refreshing, held because they are selecting, or
            simply current. */}
        {transcript && loading && <span className="micro">refreshing…</span>}
        {transcript && !loading && held && (
          <span className="micro" style={{ color: "rgb(var(--busy))" }} title="Updates resume when you click away.">
            paused while selecting
          </span>
        )}
        {transcript && (
          <span className="micro">
            {humanBytes(transcript.readBytes)} of {humanBytes(transcript.totalBytes)}
          </span>
        )}

        <button
          type="button"
          className="micro"
          onClick={() => setShowSystem((s) => !s)}
          style={{ color: showSystem ? "var(--text)" : "var(--text-faint)" }}
        >
          system
        </button>

        <div className="ml-auto flex items-center gap-3">
          {more && (
            <button
              type="button"
              className="micro"
              disabled={loading}
              onClick={() => setTail((t) => t * 2)}
              style={{ color: loading ? "var(--text-faint)" : "var(--text)" }}
            >
              {loading ? "loading…" : "load more"}
            </button>
          )}
          <button type="button" className="micro" onClick={() => void copyAll()}>
            {copied ? "copied" : "copy all"}
          </button>
        </div>
      </div>

      {error && (
        <p role="alert" className="data px-3 py-2 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}

      <div
        ref={scrollRef}
        className="min-h-0 flex-1 overflow-y-auto px-4 py-3"
        // The reason this view exists: text you can select.
        // The reason this view exists: text you can select. `WebkitUserSelect`
        // as well as the standard property — the shell sets `user-select: none`
        // on `body`, and this is the opt-out.
        style={{ userSelect: "text", WebkitUserSelect: "text", cursor: "text" }}
        onScroll={(e) => {
          const el = e.currentTarget;
          pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        <div className="mx-auto flex max-w-[1100px] flex-col gap-4">
          {entries.map((entry, i) => (
            <Turn key={`${entry.timestamp ?? ""}-${i}`} entry={entry} />
          ))}

          {/*
            The first read, which on a real transcript is genuinely slow — one
            measured 228MB, and even a bounded tail crosses SSH. This used to
            render *nothing* at all in exactly this state (`!entries.length &&
            !loading` excluded the loading case), so the pane was blank for
            several seconds with no way to tell it apart from a broken one.
          */}
          {!transcript && loading && (
            <div className="flex flex-col items-center gap-3 py-10">
              {/* Data movement, not a spinner — rule 2. */}
              <div className="flex h-[16px] items-end gap-[3px]" aria-hidden="true">
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
              </div>
              <span className="micro">reading the last {humanBytes(tail)} of the transcript</span>
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                over {session.target.host ?? "this machine"}
              </span>
            </div>
          )}

          {!entries.length && !loading && transcript && (
            <p className="micro py-6 text-center">
              {transcript.entries.length
                ? "every message here is a system note — turn on SYSTEM to see them"
                : "nothing recorded in this conversation yet"}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
