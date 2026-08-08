import { useEffect, useMemo, useState } from "react";

import { PanelLoader } from "../PanelLoader";

import { api, isTauri, type TargetRef, type TranscriptEntry } from "../../lib/api";

/**
 * What you last said to this session.
 *
 * ## Why this is worth a widget
 *
 * The pane shows the conversation, but Claude's replies dwarf the prompts — a
 * single answer can be two screens, so "what did I actually ask it to do?" means
 * scrolling back past everything it said in reply. Across four panes that
 * question gets asked constantly, and it is the one thing the transcript makes
 * cheap to answer: user turns are a tiny fraction of the bytes.
 *
 * ## It reads the transcript rather than recording as you type
 *
 * `lib/activity.ts` already counts prompts as they are sent, and it would have
 * been easy to store their text there too. That is deliberately not done:
 *
 *  - Claude's own `.jsonl` is the real record and goes back to the start of the
 *    conversation, so a stored copy would begin the day this shipped and be a
 *    worse answer to the same question.
 *  - A stored copy is a second source that drifts. Rewind or fork a conversation
 *    and the transcript changes; a private log would keep insisting on prompts
 *    that are no longer part of it.
 *
 * **Terminal commands are excluded for a different reason.** They have no
 * transcript, so showing them would mean writing command text to disk — and a
 * shell line routinely carries a token, a password or a connection string.
 * Counting them (which `activity.ts` does) costs nothing; keeping them does.
 */
export function SentPrompts({
  target,
  folder,
  resume,
}: {
  target: TargetRef;
  folder: string;
  resume?: string;
}) {
  const [entries, setEntries] = useState<TranscriptEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;

    const tick = async () => {
      try {
        // A small tail. This runs on a timer against a file that reaches
        // hundreds of megabytes, and recent prompts are by definition at the
        // end — reading more would cost a great deal to show nothing extra.
        const t = await api.claudeTranscript(target, folder, resume, 192 * 1024);
        if (cancelled) return;
        setEntries(t.entries);
        setError(null);
      } catch (e) {
        if (cancelled) return;
        // Said out loud rather than left blank: an empty panel is
        // indistinguishable from a session that has never been asked anything.
        setError(e instanceof Error ? e.message : String(e));
      }
    };

    void tick();
    // Slower than the pane's own poll. A prompt list does not need to be live
    // to the second, and this is one SSH round trip per tick.
    const timer = setInterval(tick, 10_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [target, folder, resume]);

  const prompts = useMemo(() => {
    if (!entries) return [];
    return (
      entries
        .filter((e) => e.speaker === "user" && e.text.trim())
        // Newest first: the reason to look is almost always "what did I just
        // ask?", and that answer should not require scrolling.
        .reverse()
        .slice(0, 40)
    );
  }, [entries]);

  if (error) {
    return (
      <p className="data text-[10px]" style={{ color: "rgb(var(--primary))" }}>
        {error}
      </p>
    );
  }

  if (!entries) {
    // Loading is a state with a name. A pane that can take seconds must say so,
    // or it is indistinguishable from a broken one.
    return <PanelLoader variant="inline" phase="READING THE TRANSCRIPT" />;
  }

  if (!prompts.length) {
    return (
      <p className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
        Nothing sent in this conversation yet.
      </p>
    );
  }

  return (
    // Scrolls inside itself. A long prompt must not make the rail taller than
    // the window — the rail is a column of instruments, not a document.
    <div className="flex max-h-[320px] flex-col gap-[6px] overflow-y-auto">
      {prompts.map((entry, i) => (
        <Prompt key={`${entry.timestamp ?? ""}:${i}`} entry={entry} />
      ))}
    </div>
  );
}

/**
 * One sent message: clamped to two lines, and **scrollable when opened**.
 *
 * Expanding used to grow the row without limit. A prompt is routinely a pasted
 * error, a diff or a page of instructions, so one MORE could be several
 * thousand pixels of message — it pushed every other prompt out of the list and
 * turned the outer scroller into a single enormous column, which is not reading
 * a message so much as losing everything around it. The opened body now has a
 * ceiling of its own and scrolls inside it, so the list keeps its shape and the
 * long one is still readable in full.
 */
function Prompt({ entry }: { entry: TranscriptEntry }) {
  const [open, setOpen] = useState(false);
  const text = entry.text.trim();
  // Cheap enough to be worth it: most prompts are one line, and a control that
  // does nothing on them would be noise.
  const long = text.length > 120 || text.includes("\n");

  return (
    <div className="inset px-2 py-[5px]" style={{ border: "1px solid var(--border)" }}>
      <div className="flex items-baseline justify-between gap-2">
        <span className="micro shrink-0" style={{ color: "var(--text-faint)" }}>
          {clockOf(entry.timestamp)}
        </span>
        {long && (
          <button
            type="button"
            className="micro link shrink-0"
            style={{ color: "var(--text-faint)" }}
            onClick={() => setOpen((v) => !v)}
          >
            {open ? "LESS" : "MORE"}
          </button>
        )}
      </div>
      <p
        className="data text-[11px] leading-relaxed"
        style={{
          color: "var(--text)",
          // `pre-wrap` so a multi-line prompt keeps its shape — a pasted stack
          // trace collapsed onto one line is unreadable and unrecognisable.
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          ...(open
            ? {
                // Its own scroller, not the list's: nesting is the point, or
                // one long message is the only thing the widget can show.
                maxHeight: 220,
                overflowY: "auto" as const,
                // Reaching the bottom of a message must not then scroll the
                // list out from under the thing being read.
                overscrollBehavior: "contain" as const,
              }
            : {
                display: "-webkit-box",
                WebkitLineClamp: 2,
                WebkitBoxOrient: "vertical" as const,
                overflow: "hidden",
              }),
        }}
      >
        {text}
      </p>
    </div>
  );
}

/**
 * `14:32` from an ISO stamp.
 *
 * The date is dropped on purpose: this list is the tail of one conversation, so
 * everything in it is recent, and a full timestamp per row would take more width
 * than the message in a rail this narrow.
 */
function clockOf(timestamp?: string): string {
  if (!timestamp) return "";
  const at = new Date(timestamp);
  if (Number.isNaN(at.getTime())) return "";
  return at.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}
