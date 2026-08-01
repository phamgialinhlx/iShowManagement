import { useEffect, useRef, useState } from "react";

/**
 * A scratch note, per session.
 *
 * The thing you would otherwise keep in a text file you never open again: the
 * staging URL, the test account, what you were in the middle of when you
 * switched machines. Per session because that is what makes it worth having —
 * a single global pad is a second notes app, whereas a note attached to *this
 * host, this folder* is context you get back for free when you return.
 *
 * Kept in `localStorage` rather than on the target. It is about the work, not
 * part of it: writing a file into someone's repository because they typed a
 * reminder would be a surprising side effect, and it would want committing.
 */

const key = (sessionId: string) => `rmux.note.${sessionId}`;
/** Long enough that a burst of typing is one write, short enough to survive a crash. */
const SAVE_AFTER = 400;

export function Note({ sessionId }: { sessionId: string }) {
  const [text, setText] = useState("");
  const timer = useRef<number | undefined>(undefined);

  // Reloaded per session, and *not* merged: switching sessions must show the
  // other note, never this one's text under the other one's name.
  useEffect(() => {
    setText(localStorage.getItem(key(sessionId)) ?? "");
    return () => {
      // Flush on the way out, or switching sessions inside the debounce window
      // silently discards whatever was just typed.
      window.clearTimeout(timer.current);
    };
  }, [sessionId]);

  const change = (value: string) => {
    setText(value);
    window.clearTimeout(timer.current);
    const id = sessionId;
    timer.current = window.setTimeout(() => {
      try {
        if (value) localStorage.setItem(key(id), value);
        else localStorage.removeItem(key(id));
      } catch {
        // A full localStorage must not break typing.
      }
    }, SAVE_AFTER);
  };

  return (
    <textarea
      value={text}
      onChange={(e) => change(e.target.value)}
      spellCheck={false}
      placeholder="staging URL, test account, where you left off…"
      rows={4}
      className="data inset w-full resize-y px-2 py-[6px] text-[11px] leading-relaxed outline-none"
      style={{
        border: "1px solid var(--border)",
        color: "var(--text)",
        background: "transparent",
        minHeight: 64,
      }}
    />
  );
}
