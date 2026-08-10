import { useEffect, useMemo, useState } from "react";
import { AnimatePresence, motion } from "motion/react";

import { api, isTauri, type AgentSession, type TargetRef } from "../lib/api";
import { PanelLoader } from "./PanelLoader";
import { useWorkspace } from "../lib/workspace";
import { isClaudeSession, reattachName, type ServerId } from "../lib/workspace-model";

/**
 * Sessions running on a host, and a way into them.
 *
 * A shell or a conversation on a server outlives the app, the network and the
 * laptop it was started from — so the set of things running over there is not
 * the same as the set of rows in this machine's rail. This is where the two are
 * reconciled: everything the host is holding, including work started from
 * another computer, with one action.
 *
 * ## Why a modal, when HOST ▸ SESSIONS already lists these
 *
 * The first version of this control opened the HOST pane instead. It was
 * reported as **"nothing showed up"**, and that is the correct reading: a menu
 * item saying *attach a running session* promises a list of sessions, and what
 * arrived was a pane on its PROCESSES tab with the sessions one click away. A
 * control has to do the thing it names.
 *
 * The pane keeps its list — that is where you go to watch a host. This is where
 * you go to *pick one up*, and it comes to you rather than making you navigate.
 *
 * ## The ceiling is on the list, not on the dialog
 *
 * A host with thirty sessions is ordinary — every rebuilt agent leaves its
 * predecessor's daemon running, and `list` unions them all. The rule from
 * `CLAUDE.md` applies exactly: `flex-1 overflow-y-auto` bounds nothing without a
 * definite-height ancestor, so the **scrolling element** carries the `max-h`,
 * and the dialog carries its own so a long path or a wide header cannot push the
 * buttons off a short window.
 */
export function RunningSessions({
  serverId,
  target,
  label,
  onClose,
}: {
  serverId: ServerId;
  target: TargetRef;
  /** The server as the rail names it, so the dialog says where you are. */
  label: string;
  onClose: () => void;
}) {
  const [rows, setRows] = useState<AgentSession[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const adoptServerSession = useWorkspace((s) => s.adoptServerSession);
  // Selected whole and derived here: a selector that filters returns a fresh
  // array every call, which loops `useSyncExternalStore` into a white screen.
  const sessions = useWorkspace((s) => s.sessions);
  const alreadyHere = useMemo(() => new Set(sessions.map((s) => reattachName(s))), [sessions]);

  // Read once, on open. **No poll**: this is a decision, not a monitor — the
  // list that matters is the one in front of you when you choose, and a refresh
  // under the pointer would move the row being aimed at.
  useEffect(() => {
    if (!isTauri()) {
      setError("Running sessions need the rmux desktop shell.");
      return;
    }
    let cancelled = false;
    void api
      .hostSessions(target)
      .then((r) => !cancelled && setRows(r))
      .catch((e) => !cancelled && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [target]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  /**
   * Put the keyboard on the first action once the list has arrived.
   *
   * A picker that can only be driven with a mouse is a picker you have to leave
   * the keyboard for, in an app whose whole surface is terminals. Landing on the
   * first ATTACH makes Tab and Enter enough — and it waits for `rows` rather
   * than firing on mount, because focusing a skeleton would move the cursor to
   * something that is about to be replaced.
   */
  useEffect(() => {
    if (!rows?.length) return;
    const first = document.querySelector<HTMLButtonElement>("[data-attach-first]");
    first?.focus();
  }, [rows]);

  return (
    <AnimatePresence>
      <motion.div
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.12 }}
        className="fixed inset-0 z-[95] grid place-items-start justify-center pt-[9vh]"
        style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
        onClick={onClose}
      >
        <motion.div
          initial={{ opacity: 0, y: -8 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.14, ease: [0.2, 0.9, 0.3, 1] }}
          // The dialog's own ceiling, so headers and a long host name cannot
          // push the list off a short window.
          className="window flex max-h-[80vh] w-[min(620px,92vw)] flex-col"
          onClick={(e) => e.stopPropagation()}
          role="dialog"
          aria-label={`Sessions running on ${label}`}
        >
          <header
            className="flex shrink-0 items-baseline gap-3 border-b px-4 py-3"
            style={{ borderColor: "var(--border)" }}
          >
            <span className="kicker">RUNNING ON {label.toUpperCase()}</span>
            <span className="micro ml-auto" style={{ color: "var(--text-faint)" }}>
              {rows ? `${rows.length} SESSION${rows.length === 1 ? "" : "S"}` : ""}
            </span>
          </header>

          <p className="data px-4 pt-3 text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            Everything the agent is holding on this host — including sessions started from another
            machine. Attaching opens it here; it keeps running either way.
          </p>

          {/* **The scroller carries the ceiling.** See the file comment. */}
          <div className="min-h-0 flex-1 overflow-y-auto px-2 py-2" style={{ maxHeight: "52vh" }}>
            {error ? (
              <p role="alert" className="data px-2 py-3 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
                {error}
              </p>
            ) : !rows ? (
              <PanelLoader variant="rows" phase="READING SESSIONS" detail={label} rows={4} />
            ) : rows.length === 0 ? (
              <p className="micro px-2 py-3" style={{ color: "var(--text-faint)" }}>
                nothing running on this host
              </p>
            ) : (
              <ul className="flex flex-col">
                {rows.map((s, i) => {
                  const here = alreadyHere.has(s.name);
                  // The first row that can actually be attached — not merely the
                  // first row, which may already be in the rail and carry no
                  // control to focus.
                  const firstAttachable =
                    !here && !rows.slice(0, i).some((r) => !alreadyHere.has(r.name));
                  return (
                    <li
                      key={s.name}
                      className="flex items-center gap-3 border-b px-2 py-[7px]"
                      style={{ borderColor: "var(--border)" }}
                    >
                      <span
                        className="round shrink-0"
                        title={s.attached ? "a client is attached" : "detached"}
                        style={{
                          width: 7,
                          height: 7,
                          background: s.attached ? "rgb(var(--busy))" : "var(--text-faint)",
                        }}
                      />
                      <div className="flex min-w-0 flex-1 flex-col">
                        {/* The alias leads when there is one — it is the name a
                            person chose, possibly at another computer — with the
                            key underneath, since the key is what a bug report
                            and `rmux-agent kill` need. */}
                        <span className="data truncate text-[12px]" style={{ color: "var(--text)" }}>
                          {s.alias ?? s.name}
                        </span>
                        <span className="micro truncate" style={{ color: "var(--text-faint)" }}>
                          {s.alias ? `${s.name} · ` : ""}
                          {isClaudeSession(s.command) ? "claude" : "shell"} ·{" "}
                          {s.attached ? "attached" : "detached"} · {age(s.ageSeconds)}
                          {s.pid ? ` · pid ${s.pid}` : ""}
                        </span>
                      </div>

                      {/* A state, not a disabled button: a greyed ATTACH invites
                          "how do I enable this", a label answers it. */}
                      {here ? (
                        <span className="micro shrink-0" style={{ color: "var(--text-faint)" }}>
                          IN RAIL
                        </span>
                      ) : (
                        <button
                          type="button"
                          className="chip shrink-0"
                          {...(firstAttachable ? { "data-attach-first": "" } : {})}
                          onClick={() => {
                            adoptServerSession(
                              serverId,
                              s.name,
                              isClaudeSession(s.command) ? "claude" : "terminal",
                              s.alias,
                            );
                            onClose();
                          }}
                        >
                          ATTACH
                        </button>
                      )}
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
            <button type="button" className="chip" onClick={onClose}>
              CLOSE
            </button>
          </footer>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}

/** `2h 14m`, `3d` — a duration, not a timestamp: what matters is how long. */
function age(seconds: number): string {
  if (seconds < 60) return `${Math.max(0, Math.round(seconds))}s`;
  const m = Math.floor(seconds / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}
