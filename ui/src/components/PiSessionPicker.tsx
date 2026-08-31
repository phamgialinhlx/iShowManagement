import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "motion/react";

import { api, isTauri, type PiConversation, type TargetRef } from "../lib/api";
import { PanelLoader } from "./PanelLoader";
import { basename } from "../lib/workspace-model";

/**
 * The pi conversations already recorded on a host, and a way back into them.
 *
 * Like the Claude picker, resuming is offered **beside** starting fresh: the
 * work you did here yesterday is almost always worth more than a clean slate.
 * Unlike it, this is host-wide — pi conversations carry their own `cwd`, so
 * there is no folder to pick first, and a resume simply runs in the directory
 * the conversation recorded (pi locates its sessions under a cwd-encoded dir).
 *
 * ## The ceiling is on the list, not on the dialog
 *
 * A host with many pi conversations is ordinary — every rebuilt agent leaves its
 * predecessor's daemon running, and a real host reached 228MB transcripts. The
 * `CLAUDE.md` rule applies exactly: `flex-1 overflow-y-auto` bounds nothing
 * without a definite-height ancestor, so the **scrolling element** carries the
 * `max-h`, and the dialog carries its own so a long `cwd` cannot push CANCEL off
 * a short window.
 *
 * ## Never a dead list
 *
 * When the read fails (`summary` is often absent over SSH, and the listing can
 * fail outright), the error is shown *and* "New pi session" still stands — a
 * lookup that could not run must never read as "no history".
 */
export function PiSessionPicker({
  target,
  busy,
  onChoose,
  onCancel,
}: {
  target: TargetRef;
  busy?: boolean;
  /** `resume` absent means start fresh. `cwd`/`name` come from the chosen row. */
  onChoose: (choice: { resume?: string; cwd?: string; name?: string }) => void;
  onCancel: () => void;
}) {
  const [rows, setRows] = useState<PiConversation[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const host = target.host ?? "this machine";

  // Read once, on open. **No poll**: this is a decision, not a monitor — a
  // refresh under the pointer would move the row being aimed at.
  useEffect(() => {
    if (!isTauri()) {
      // Not fatal — a new pi session can still be started, so the list is set to
      // empty rather than left loading forever.
      setError("Resuming pi needs the rmux desktop shell.");
      setRows([]);
      return;
    }
    let cancelled = false;
    void api
      .piListAllSessions(target)
      .then((found) => {
        if (!cancelled) setRows(Array.isArray(found) ? found : []);
      })
      .catch((e) => {
        // Say what happened rather than showing an empty list, which would read
        // as "no history" when the truth is "could not look".
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
          setRows([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [target]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  return (
    <AnimatePresence>
      <motion.div
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.12 }}
        className="fixed inset-0 z-[95] grid place-items-start justify-center pt-[9vh]"
        style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
        onClick={onCancel}
      >
        <motion.div
          initial={{ opacity: 0, y: -8 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.14, ease: [0.2, 0.9, 0.3, 1] }}
          // The dialog's own ceiling, so a long host name or `cwd` cannot push
          // the list or CANCEL off a short window.
          className="window flex max-h-[80vh] w-[min(620px,92vw)] flex-col"
          onClick={(e) => e.stopPropagation()}
          role="dialog"
          aria-label={`Resume a pi session on ${host}`}
        >
          <header
            className="flex shrink-0 items-baseline gap-3 border-b px-4 py-3"
            style={{ borderColor: "var(--border)" }}
          >
            <span className="kicker">PI SESSIONS ON {host.toUpperCase()}</span>
            <span className="micro ml-auto" style={{ color: "var(--text-faint)" }}>
              {rows ? `${rows.length} CONVERSATION${rows.length === 1 ? "" : "S"}` : ""}
            </span>
          </header>

          {/* New always leads, and always works — even when the list could not
              be read. It sits outside the scroller so it is never scrolled away. */}
          <div className="shrink-0 border-b px-2 py-2" style={{ borderColor: "var(--border)" }}>
            <button
              type="button"
              className="btn btn-primary w-full"
              disabled={busy}
              onClick={() => onChoose({})}
            >
              {busy ? "Opening…" : "New pi session"}
            </button>
          </div>

          {/* **The scroller carries the ceiling.** See the file comment. */}
          <div className="min-h-0 flex-1 overflow-y-auto px-2 py-2" style={{ maxHeight: "52vh" }}>
            {error && (
              <p
                role="alert"
                className="data px-2 py-2 text-[11px]"
                style={{ color: "rgb(var(--primary))" }}
              >
                could not read pi sessions: {error}
              </p>
            )}

            {!rows && !error && (
              <PanelLoader variant="rows" phase="READING PI SESSIONS" detail={host} rows={4} />
            )}

            {rows && rows.length === 0 && !error && (
              <p className="micro px-2 py-3" style={{ color: "var(--text-faint)" }}>
                no previous pi sessions on this host
              </p>
            )}

            {rows && rows.length > 0 && (
              <ul className="flex flex-col">
                {rows.map((c) => {
                  // Prefer the summary pi recorded; when it is absent (routine
                  // over SSH) the folder's basename is the next-best label, and
                  // the full cwd underneath tells otherwise-identical rows apart.
                  const label = c.summary?.trim() || basename(c.cwd) || c.id.slice(0, 8);
                  return (
                    <li
                      key={c.id}
                      className="border-b"
                      style={{ borderColor: "var(--border)" }}
                    >
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => onChoose({ resume: c.id, cwd: c.cwd, name: label })}
                        className="flex w-full flex-col gap-[2px] px-2 py-[7px] text-left"
                        onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
                        onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
                      >
                        <span className="data truncate text-[12px]" style={{ color: "var(--text)" }}>
                          {label}
                        </span>
                        <span className="micro truncate" style={{ color: "var(--text-faint)" }}>
                          {c.cwd} · {ago(c.modified)}
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>

          <footer
            className="flex shrink-0 items-center justify-end border-t px-4 py-3"
            style={{ borderColor: "var(--border)" }}
          >
            <button type="button" className="chip" onClick={onCancel}>
              CANCEL
            </button>
          </footer>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}

/**
 * "3h ago", "yesterday" — the form you actually reason about when choosing.
 *
 * `modified` is unix **milliseconds** here (pi's header), unlike the Claude
 * picker's seconds — so this halves the constants rather than sharing the helper.
 */
function ago(unixMs: number): string {
  if (!unixMs) return "unknown";
  const seconds = Math.max(0, Math.floor((Date.now() - unixMs) / 1000));

  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "yesterday" : `${days}d ago`;
}
