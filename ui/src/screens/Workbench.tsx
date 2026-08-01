import { useEffect, useState } from "react";
import { AnimatePresence } from "motion/react";

import { ErrorBoundary } from "../components/ErrorBoundary";
import { Metrics } from "../components/Metrics";
import { NewSessionLayer } from "../components/NewSession";
import { SessionDeck } from "../components/SessionView";
import { SessionRail } from "../components/SessionRail";
import { WidgetRail } from "../components/WidgetRail";
import { TitleBar, TITLE_BAR_HEIGHT } from "../components/TitleBar";
import { isDirty, useSessions } from "../lib/sessions";
import { api, isTauri, type LockStatus, type SignedIn } from "../lib/api";
import { SignIn } from "../components/SignIn";
import { LockSettings } from "../components/LockSettings";

/**
 * The workbench.
 *
 * Sessions on the left, the active session's workspace on the right. The rail is
 * always present because the app's real job is not "edit this folder" — it is
 * "keep several pieces of work moving, and tell me which one needs me".
 */
export function Workbench({
  session,
  onSession,
}: {
  session: SignedIn | null;
  onSession: (session: SignedIn | null) => void;
}) {
  const [signInOpen, setSignInOpen] = useState(false);
  const [lockOpen, setLockOpen] = useState(false);
  const [lock, setLock] = useState<LockStatus | null>(null);

  // Whether a lock exists is keychain state, not something this screen can infer
  // from having a session — the app may have been unlocked on the way in.
  useEffect(() => {
    if (!isTauri() || !session) return;
    let cancelled = false;
    api
      .lockStatus(session.serverUrl)
      .then((s) => !cancelled && setLock(s))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [session]);
  const sessions = useSessions((s) => s.sessions);
  const activeId = useSessions((s) => s.activeSession);
  const buffers = useSessions((s) => s.buffers);
  const activate = useSessions((s) => s.activate);

  const [newSessionOpen, setNewSessionOpen] = useState(false);

  const active = sessions.find((s) => s.id === activeId) ?? null;
  const dirtyCount = Object.values(buffers).filter(isDirty).length;
  const waiting = sessions.filter((s) => s.status === "waiting");

  // Open the dialog on a first run, so the app is never a blank window with no
  // obvious next step.
  useEffect(() => {
    if (sessions.length === 0) setNewSessionOpen(true);
    // Only on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Global shortcuts. Session-scoped ones live in SessionView.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;

      if (e.key.toLowerCase() === "n") {
        e.preventDefault();
        setNewSessionOpen(true);
      }
      // ⌘1..⌘9 jumps straight to a session — the fastest way to switch when
      // several are running at once.
      const digit = Number(e.key);
      if (Number.isInteger(digit) && digit >= 1 && digit <= 9) {
        const target = sessions[digit - 1];
        if (target) {
          e.preventDefault();
          activate(target.id);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sessions, activate]);

  return (
    <>
      <div className="atmosphere" />
      <TitleBar>
        <div className="flex items-center gap-3" data-tauri-drag-region>
          {active && (
            <span className="micro" data-tauri-drag-region>
              {active.name} · {active.target.host ?? "local"}
            </span>
          )}
        </div>
      </TitleBar>

      <div className="flex h-full w-full flex-col" style={{ paddingTop: TITLE_BAR_HEIGHT }}>
        <div className="flex min-h-0 flex-1">
          <SessionRail onNewSession={() => setNewSessionOpen(true)} />

          {active ? (
            <ErrorBoundary label="This session">
              <SessionDeck />
            </ErrorBoundary>
          ) : (
            <div className="grid flex-1 place-items-center">
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => setNewSessionOpen(true)}
              >
                New session
              </button>
            </div>
          )}

          {/* Instruments last, so the work keeps the middle of the screen. Its
              own error boundary: a widget that throws must not take the session
              down with it. */}
          <ErrorBoundary label="Instruments">
            <WidgetRail session={active ?? null} />
          </ErrorBoundary>
        </div>

        <footer
          className="flex shrink-0 items-center gap-4 border-t px-3 py-1"
          style={{ borderColor: "var(--border)" }}
        >
          {active && (
            <>
              <span className="micro">
                {active.target.host ? `ssh · ${active.target.host}` : "local"}
              </span>
              <span className="micro truncate" title={active.folder}>
                {active.folder}
              </span>
            </>
          )}

          <div className="ml-auto flex items-center gap-4">
            {/* Sessions needing attention, visible wherever you are. Clicking
                jumps straight there — the point of the whole design. */}
            {waiting.length > 0 && (
              <button
                type="button"
                className="micro"
                style={{ color: "rgb(var(--primary))" }}
                onClick={() => activate(waiting[0]!.id)}
                title="Go to the session waiting on you"
              >
                {waiting.length} waiting
              </button>
            )}

            {dirtyCount > 0 && (
              <span className="micro" style={{ color: "rgb(var(--busy))" }}>
                {dirtyCount} unsaved
              </span>
            )}

            {active && <Metrics target={active.target} />}

            <span className="micro">
              {session ? session.account.displayName || session.account.username : "not signed in"}
            </span>
            {/* Only offered while signed in: there is nothing to seal otherwise,
                and the workbench itself is not what the lock protects. */}
            {session && (
              <button
                type="button"
                className="micro"
                style={{ color: lock?.locked ? "var(--text)" : "var(--text-faint)" }}
                onClick={() => setLockOpen(true)}
                title={
                  lock?.locked
                    ? "rmux asks for your PIN on every start"
                    : "Ask for a PIN before restoring this session"
                }
              >
                {lock?.locked ? "locked" : "lock"}
              </button>
            )}
            <button
              type="button"
              className="micro"
              style={{ color: session ? "var(--text-faint)" : "var(--text)" }}
              onClick={() => {
                if (!session) {
                  setSignInOpen(true);
                  return;
                }
                void api.signOut().finally(() => onSession(null));
              }}
            >
              {session ? "sign out" : "sign in"}
            </button>
          </div>
        </footer>
      </div>

      <ErrorBoundary label="New session" onReset={() => setNewSessionOpen(false)}>
        <NewSessionLayer open={newSessionOpen} onClose={() => setNewSessionOpen(false)} />
      </ErrorBoundary>

      <AnimatePresence>
        {signInOpen && (
          <ErrorBoundary label="Sign in" onReset={() => setSignInOpen(false)}>
            <SignIn
              onClose={() => setSignInOpen(false)}
              onSignedIn={(next) => {
                onSession(next);
                setSignInOpen(false);
              }}
            />
          </ErrorBoundary>
        )}
        {lockOpen && session && (
          <ErrorBoundary label="Lock" onReset={() => setLockOpen(false)}>
            <LockSettings
              status={lock ?? { locked: false, face: false, username: "", serverUrl: session.serverUrl }}
              onChanged={setLock}
              onClose={() => setLockOpen(false)}
            />
          </ErrorBoundary>
        )}
      </AnimatePresence>
    </>
  );
}
