import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { api, isTauri, type Transcript, type TranscriptEntry } from "../lib/api";
import type { Session } from "../lib/sessions";

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

const TAIL_STEP = 512 * 1024;

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
          <Markdown remarkPlugins={[remarkGfm]}>{entry.text}</Markdown>
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

export function TranscriptView({ session }: { session: Session }) {
  const [transcript, setTranscript] = useState<Transcript | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [tail, setTail] = useState(TAIL_STEP);
  const [showSystem, setShowSystem] = useState(false);
  const [copied, setCopied] = useState(false);

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
      setTranscript(next);
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
    const timer = setInterval(() => void load(), 5000);
    return () => clearInterval(timer);
  }, [load]);

  const entries = useMemo(
    () => (transcript?.entries ?? []).filter((e) => showSystem || e.speaker !== "system"),
    [transcript, showSystem],
  );

  useEffect(() => {
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
        style={{ userSelect: "text", cursor: "auto" }}
        onScroll={(e) => {
          const el = e.currentTarget;
          pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        <div className="mx-auto flex max-w-[840px] flex-col gap-4">
          {entries.map((entry, i) => (
            <Turn key={`${entry.timestamp ?? ""}-${i}`} entry={entry} />
          ))}

          {!entries.length && !loading && (
            <p className="micro py-6 text-center">
              {transcript ? "nothing to show yet" : "reading transcript…"}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
