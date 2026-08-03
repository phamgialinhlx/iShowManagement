import { useMemo, useState } from "react";
import { motion, AnimatePresence } from "motion/react";

import { basename, useSessions, type Session, type SessionStatus } from "../lib/sessions";
import { gridLayout } from "../lib/grid";

/**
 * The session rail.
 *
 * Always visible, always showing every session's status. This is the primary
 * navigation of the app: the question it answers is "which of my sessions needs
 * me?", which is the whole reason for running work across several machines at
 * once.
 *
 * Collapsed, it keeps the status dots — the signal survives, only the labels go.
 * A collapsed rail that hid which session was waiting would defeat its purpose.
 */

const RAIL_WIDTH = 216;
const RAIL_COLLAPSED = 48;

/** Rule 0 in its purest form: red means *you* must act, and nothing else does. */
function statusColor(status: SessionStatus): string {
  switch (status) {
    case "waiting":
      return "rgb(var(--primary))";
    case "working":
      return "rgb(var(--busy))";
    default:
      return "var(--text-faint)";
  }
}

/**
 * The one mark that answers "which of my machines needs me right now?".
 *
 * Three states, and the difference between them has to survive being seen from
 * the corner of an eye:
 *
 * - **Working** — a turning ring. A static amber dot cannot tell a run that is
 *   still going from one that stopped an hour ago, and that is the single most
 *   useful thing the rail can say. Rule 2 holds: this rotates continuously, it
 *   does not blink; the dot never disappears.
 * - **Waiting** — red and still. Claude has asked something and nothing is
 *   moving until it is answered.
 * - **Finished, unseen** — red with a slow outward swell. Work that stopped
 *   while the operator was elsewhere is exactly the thing that gets forgotten,
 *   and "idle" looks identical to a session that has never run. The mark clears
 *   when the session is opened, not on a timer — an alert that expires by
 *   itself is one you miss by being away from the desk, which is precisely when
 *   a long run finishes.
 */
function StatusDot({ status, finished }: { status: SessionStatus; finished?: boolean }) {
  const done = finished && status !== "working";

  if (status === "working") {
    return (
      <span
        className="round ring-spin shrink-0"
        title="working"
        style={{
          width: 9,
          height: 9,
          // An arc, not a full ring: a complete circle rotating looks static.
          border: "1.5px solid rgb(var(--busy) / 0.28)",
          borderTopColor: "rgb(var(--busy))",
          boxSizing: "border-box",
        }}
      />
    );
  }

  return (
    <span
      className={`round shrink-0${done ? " done-swell" : ""}`}
      title={done ? "finished — not looked at yet" : status}
      style={{
        width: 7,
        height: 7,
        background: done ? "rgb(var(--primary))" : statusColor(status),
      }}
    />
  );
}

function Row({
  session,
  active,
  onScreen,
  collapsed,
  onSelect,
  onClose,
}: {
  session: Session;
  active: boolean;
  /**
   * Whether this session currently occupies a grid cell.
   *
   * In a 2x2 the operator is looking at four of maybe eight sessions, and
   * nothing in the rail said which four — so "is that one on screen or do I
   * need to go and find it?" could only be answered by looking away from the
   * rail and counting panes.
   */
  onScreen: boolean;
  collapsed: boolean;
  onSelect: () => void;
  onClose: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [editing, setEditing] = useState(false);
  const rename = useSessions((s) => s.renameSession);
  const host = session.target.host ?? "local";
  // Several sessions routinely share one folder on one host, so the row has to
  // say *where* as well as *what* — the name alone no longer identifies it.
  const where = `${host} · ${basename(session.folder)}`;

  if (collapsed) {
    return (
      <button
        type="button"
        onClick={onSelect}
        title={`${session.name} · ${where}`}
        className="flex h-[34px] w-full items-center justify-center"
        style={{
          background: active ? "var(--hover)" : onScreen ? "rgba(232,230,225,0.03)" : "transparent",
          boxShadow: onScreen ? `inset 2px 0 0 ${active ? "var(--text)" : "var(--text-faint)"}` : "none",
        }}
      >
        <StatusDot status={session.status} finished={session.finishedAt !== undefined} />
      </button>
    );
  }

  return (
    <div
      className="group relative"
      style={{
        // Two levels, because in a grid they are different questions. Every
        // session *on screen* gets the hairline, so the rail answers "which of
        // these am I looking at" without counting panes. The focused one keeps
        // the fill on top of it. Colour is not used for either — that would
        // compete with the status dots, which carry the real signal.
        background: active ? "var(--hover)" : onScreen ? "rgba(232,230,225,0.03)" : "transparent",
        boxShadow: onScreen
          ? `inset 2px 0 0 ${active ? "var(--text)" : "var(--text-faint)"}`
          : "none",
      }}
    >
      {editing ? (
        <div className="flex items-center gap-2 px-3 py-[7px]">
          <StatusDot status={session.status} finished={session.finishedAt !== undefined} />
          <input
            autoFocus
            defaultValue={session.name}
            aria-label="Session name"
            className="data inset min-w-0 flex-1 px-1 py-[1px] text-[12px] outline-none"
            style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
            onFocus={(e) => e.currentTarget.select()}
            // Committing on blur as well as on Enter: clicking away is how most
            // people finish typing, and losing the name then would be maddening.
            onBlur={(e) => {
              rename(session.id, e.currentTarget.value);
              setEditing(false);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") {
                // Restore first, so the blur handler commits the original.
                e.currentTarget.value = session.name;
                e.currentTarget.blur();
              }
            }}
          />
        </div>
      ) : (
        <button
          type="button"
          onClick={onSelect}
          onDoubleClick={() => setEditing(true)}
          title={`${session.name} — ${where}\nDouble-click to rename`}
          className="flex w-full items-center gap-2 px-3 py-[7px] text-left"
        >
          <StatusDot status={session.status} finished={session.finishedAt !== undefined} />
          <span className="flex min-w-0 flex-1 flex-col">
            <span className="data truncate text-[12px]" style={{ color: "var(--text)" }}>
              {session.name}
            </span>
            <span className="micro truncate" style={{ letterSpacing: "0.12em" }}>
              {where}
            </span>
          </span>
        </button>
      )}

      {session.error && (
        <p className="data px-3 pb-1 text-[10px]" style={{ color: "rgb(var(--primary))" }}>
          {session.error}
        </p>
      )}

      {confirming ? (
        <div className="flex flex-col gap-1 px-3 pb-2">
          {/* Said plainly, because it is not what closing a window does. This
              session's shells and its Claude are running under the agent on the
              target — they outlive rmux by design — so closing is the thing
              that ends them. The conversation itself is kept: it is on disk in
              Claude's own transcript and can be resumed into a new session. */}
          <span className="micro" style={{ color: "var(--text-soft)" }}>
            ENDS ITS SHELLS AND CLAUDE ON {(session.target.host ?? "this machine").toUpperCase()}
          </span>
          <div className="flex gap-2">
          <button
            type="button"
            className="micro"
            style={{ color: "rgb(var(--primary))" }}
            onClick={() => onClose()}
          >
            close it
          </button>
          <button type="button" className="micro" onClick={() => setConfirming(false)}>
            cancel
          </button>
          </div>
        </div>
      ) : (
        <button
          type="button"
          aria-label={`Close ${session.name}`}
          className="absolute right-2 top-[9px] opacity-0 group-hover:opacity-100"
          style={{ color: "var(--text-faint)" }}
          onClick={(e) => {
            e.stopPropagation();
            setConfirming(true);
          }}
        >
          <svg
            width="9"
            height="9"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="square"
            aria-hidden="true"
          >
            <path d="M18 6L6 18M6 6l12 12" />
          </svg>
        </button>
      )}
    </div>
  );
}

export function SessionRail({ onNewSession }: { onNewSession: () => void }) {
  const sessions = useSessions((s) => s.sessions);
  const active = useSessions((s) => s.activeSession);
  const collapsed = useSessions((s) => s.railCollapsed);
  const activate = useSessions((s) => s.activate);
  const remove = useSessions((s) => s.removeSession);
  const grid = useSessions((s) => s.grid);
  const slots = useSessions((s) => s.gridSlots);
  const focusedCell = useSessions((s) => s.focusedCell);
  const focusCell = useSessions((s) => s.focusCell);
  const assignSlot = useSessions((s) => s.assignSlot);
  const toggle = useSessions((s) => s.toggleRail);

  // Both kinds of "needs you" — Claude is asking, or Claude finished and nobody
  // has looked. Counting only the first left a rail full of red swells above a
  // header that said everything was fine.
  // The same layout the workbench computes, so the rail cannot disagree with
  // what is actually on screen. Derived rather than published from the grid:
  // two sources for one fact is two chances to drift.
  const onScreen = useMemo(
    () => new Set(gridLayout(sessions, slots, grid).flatMap((s) => (s ? [s.id] : []))),
    [sessions, slots, grid],
  );

  const needing = sessions.filter(
    (s) => s.status === "waiting" || (s.finishedAt !== undefined && s.status !== "working"),
  ).length;

  return (
    <motion.aside
      className="panel flex shrink-0 flex-col overflow-hidden"
      animate={{ width: collapsed ? RAIL_COLLAPSED : RAIL_WIDTH }}
      transition={{ type: "spring", stiffness: 320, damping: 34 }}
      style={{ width: collapsed ? RAIL_COLLAPSED : RAIL_WIDTH }}
    >
      <header
        className="flex shrink-0 items-center justify-between border-b px-2 py-2"
        style={{ borderColor: "var(--border)" }}
      >
        {!collapsed && (
          <span className="micro">
            SESSIONS
            {/* The count of sessions needing attention, which is the number you
                actually want to know at a glance. */}
            {needing > 0 && (
              <span style={{ color: "rgb(var(--primary))" }}> · {needing} need you</span>
            )}
          </span>
        )}
        <button
          type="button"
          className="micro"
          onClick={toggle}
          title={collapsed ? "Expand sessions" : "Collapse sessions"}
          style={{ marginLeft: collapsed ? "auto" : 0, marginRight: collapsed ? "auto" : 0 }}
        >
          {collapsed ? "»" : "«"}
        </button>
      </header>

      {/* The mode is stated rather than left to be discovered. A rail whose
          clicks quietly mean something different is the kind of thing people
          find by accident and then distrust. */}
      {grid >= 2 && focusedCell !== null && !collapsed && (
        <div
          className="flex shrink-0 items-center gap-2 border-b px-3 py-[6px]"
          style={{ borderColor: "var(--border)", background: "var(--hover)" }}
        >
          <span className="micro" style={{ color: "var(--text)" }}>
            PICK A SESSION FOR CELL {focusedCell + 1}
          </span>
          <button
            type="button"
            className="micro ml-auto"
            style={{ color: "var(--text-faint)" }}
            onClick={() => focusCell(null)}
          >
            CANCEL
          </button>
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden">
        <AnimatePresence initial={false}>
          {sessions.map((session) => (
            <motion.div
              key={session.id}
              initial={{ opacity: 0, height: 0 }}
              animate={{ opacity: 1, height: "auto" }}
              exit={{ opacity: 0, height: 0 }}
              transition={{ duration: 0.18, ease: [0.2, 0.9, 0.3, 1] }}
            >
              <Row
                session={session}
                active={session.id === active}
                onScreen={onScreen.has(session.id)}
                collapsed={collapsed}
                onSelect={() => {
                  // **In grid mode with a cell selected, this fills that cell.**
                  // Which is the only interaction that answers "I have eight
                  // sessions and four cells": pick the pane, then pick what
                  // goes in it. Without a cell selected it activates as usual,
                  // so the rail keeps working exactly as before in focus mode.
                  if (grid >= 2 && focusedCell !== null) {
                    assignSlot(focusedCell, session.id);
                  }
                  activate(session.id);
                }}
                onClose={() => remove(session.id)}
              />
            </motion.div>
          ))}
        </AnimatePresence>

        {sessions.length === 0 && !collapsed && (
          <p className="micro px-3 py-3 leading-relaxed">
            no sessions yet — add a server or a local folder
          </p>
        )}
      </div>

      <div className="shrink-0 border-t p-2" style={{ borderColor: "var(--border)" }}>
        <button
          type="button"
          className="btn w-full"
          style={{ padding: collapsed ? "6px 0" : "8px 12px", fontSize: collapsed ? 14 : 11 }}
          onClick={onNewSession}
          title="New session (⌘N)"
        >
          {collapsed ? "+" : "New session"}
        </button>
      </div>
    </motion.aside>
  );
}
