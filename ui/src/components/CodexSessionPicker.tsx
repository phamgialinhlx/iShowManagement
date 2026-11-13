import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "motion/react";

import { api, hasBackend, type PiConversation, type TargetRef } from "../lib/api";
import { PanelLoader } from "./PanelLoader";
import { ModelChoice } from "./ModelChoice";
import { PermissionChoice } from "./PermissionChoice";
import { basename } from "../lib/workspace-model";

/**
 * Start Codex on a host, or continue a conversation recorded there.
 *
 * The pi picker's shape — host-wide, because each Codex rollout records its own
 * `cwd`, so a resume runs where the conversation ran — plus the two launch
 * decisions Codex has: which **OpenAI profile** (only OpenAI ones are offered;
 * the host hands a profile's key only to sessions of its own agent) and the
 * **bypass**, which follows the skip-permissions rule exactly: off by default, a
 * deliberate second click, kept with this one session.
 *
 * The list is read once, never polled, and its scroller carries the ceiling (a
 * host's `~/.codex/sessions` grows for as long as anyone uses Codex there). A
 * listing that fails says so, and NEW still works.
 */
export function CodexSessionPicker({
  target,
  busy,
  onChoose,
  onCancel,
}: {
  target: TargetRef;
  busy?: boolean;
  /** `resume` absent means start fresh. `cwd`/`name` come from the chosen row. */
  onChoose: (choice: {
    resume?: string;
    cwd?: string;
    name?: string;
    modelProfile?: string;
    bypassApprovals: boolean;
  }) => void;
  onCancel: () => void;
}) {
  const [profile, setProfile] = useState<string | null>(null);
  const [bypass, setBypass] = useState(false);
  const launch = { modelProfile: profile ?? undefined, bypassApprovals: bypass };
  const [rows, setRows] = useState<PiConversation[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const host = target.host ?? "this machine";

  // Read once, on open. **No poll**: this is a decision, not a monitor — a
  // refresh under the pointer would move the row being aimed at.
  useEffect(() => {
    if (!hasBackend()) {
      // Not fatal — a new pi session can still be started, so the list is set to
      // empty rather than left loading forever.
      setError("Resuming Codex needs the rmux desktop app or rmux-web.");
      setRows([]);
      return;
    }
    let cancelled = false;
    void api
      .codexListAllSessions(target)
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
          aria-label={`Start or resume Codex on ${host}`}
        >
          <header
            className="flex shrink-0 items-baseline gap-3 border-b px-4 py-3"
            style={{ borderColor: "var(--border)" }}
          >
            <span className="kicker">CODEX ON {host.toUpperCase()}</span>
            <span className="micro ml-auto" style={{ color: "var(--text-faint)" }}>
              {rows ? `${rows.length} CONVERSATION${rows.length === 1 ? "" : "S"}` : ""}
            </span>
          </header>

          {/* New always leads, and always works — even when the list could not
              be read. It sits outside the scroller so it is never scrolled away. */}
          <div
            className="flex shrink-0 flex-col gap-3 border-b px-3 py-3"
            style={{ borderColor: "var(--border)" }}
          >
            <ModelChoice kind="openai" value={profile} onChange={setProfile} hintWhenEmpty />
            <PermissionChoice agent="codex" value={bypass} onChange={setBypass} />
            <button
              type="button"
              className="btn btn-primary w-full"
              disabled={busy}
              onClick={() => onChoose({ ...launch })}
            >
              {busy ? "Opening…" : "New Codex session"}
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
                could not read Codex sessions: {error}
              </p>
            )}

            {!rows && !error && (
              <PanelLoader variant="rows" phase="READING CODEX SESSIONS" detail={host} rows={4} />
            )}

            {rows && rows.length === 0 && !error && (
              <p className="micro px-2 py-3" style={{ color: "var(--text-faint)" }}>
                no previous Codex sessions on this host
              </p>
            )}

            {rows && rows.length > 0 && (
              <ul className="flex flex-col">
                {rows.map((c) => {
                  // Codex's listing carries no summary over SSH (only the first
                  // line of each rollout is read), so the folder's basename is
                  // the label and the full cwd beneath tells rows apart.
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
                        onClick={() => onChoose({ ...launch, resume: c.id, cwd: c.cwd, name: label })}
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
 * `modified` is unix **milliseconds** here (from the rollout's mtime), unlike the Claude
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
