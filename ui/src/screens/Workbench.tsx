import { useEffect, useMemo, useState } from "react";
import { AnimatePresence } from "motion/react";

import { ErrorBoundary } from "../components/ErrorBoundary";
import { Metrics } from "../components/Metrics";
import { WorkspaceNewSessionLayer, type NewMode } from "../components/WorkspaceNewSession";
import { WorkspaceDeck } from "../components/WorkspaceDeck";
import { WorkspaceRail } from "../components/WorkspaceRail";
import { WidgetRail, type Active } from "../components/WidgetRail";
import { AlertStack } from "../components/AlertStack";
import { TitleBar, TITLE_BAR_HEIGHT } from "../components/TitleBar";
import { DECKS, deckId } from "../lib/grid";
import { useWorkspace } from "../lib/workspace";
import { startAttentionWatch } from "../lib/attention";
import { useShortcuts } from "../lib/use-shortcuts";
import { startStatusWatch } from "../lib/status-watch";
import { api, isTauri, type LockStatus, type SignedIn } from "../lib/api";
import { SignIn } from "../components/SignIn";
import { Dashboard } from "./Dashboard";

/**
 * The workbench.
 *
 * A **Server → Project → Session** tree on the left, a pane manager in the
 * middle, instruments on the right. The rail is always present because the
 * app's job is not "edit this folder" — it is "keep several pieces of work
 * moving across several machines, and tell me which one needs me".
 */
export function Workbench({
  session,
  onSession,
}: {
  session: SignedIn | null;
  onSession: (session: SignedIn | null) => void;
}) {
  const [signInOpen, setSignInOpen] = useState(false);
  const [lock, setLock] = useState<LockStatus | null>(null);

  // The rail's status for sessions that are not on screen. Started here rather
  // than in a pane, because a pane only exists for a session you can see.
  useEffect(() => startStatusWatch(), []);
  // One watcher for the whole app: attention is singular, so counting it per
  // pane would credit every mounted session at once. See `lib/attention.ts`.
  useEffect(() => startAttentionWatch(), []);

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

  const servers = useWorkspace((s) => s.servers);
  const sessions = useWorkspace((s) => s.sessions);
  const activeId = useWorkspace((s) => s.activeSession);
  const runtime = useWorkspace((s) => s.runtime);
  const activate = useWorkspace((s) => s.activate);
  const deck = useWorkspace((s) => s.deck);
  const setDeck = useWorkspace((s) => s.setDeck);
  const railCollapsed = useWorkspace((s) => s.railCollapsed);
  const toggleRail = useWorkspace((s) => s.toggleRail);
  const serverOf = useWorkspace((s) => s.serverOf);
  const projectOf = useWorkspace((s) => s.projectOf);
  const targetOf = useWorkspace((s) => s.targetOf);

  const [newMode, setNewMode] = useState<NewMode | null>(null);
  const [dashboard, setDashboard] = useState(false);

  // The keyboard, bound once for the whole workbench. See `use-shortcuts.ts`
  // for why it is one listener and why it bubbles rather than captures.
  useShortcuts({ progressOpen: dashboard, onProgress: setDashboard });

  const active = sessions.find((s) => s.id === activeId) ?? null;
  const activeProject = active ? projectOf(active.id) : undefined;
  const activeTarget = active ? targetOf(active.id) : undefined;
  const activeHost = active ? serverOf(active.id)?.target.host : undefined;

  const waitingIds = useMemo(
    () =>
      sessions
        .filter((s) => s.kind === "claude" && runtime[s.id]?.status === "waiting")
        .map((s) => s.id),
    [sessions, runtime],
  );

  // The flattened context the instruments read (v3 splits target/folder off the
  // session — see `Active`).
  const activeCtx: Active | null =
    active && activeTarget
      ? {
          id: active.id,
          target: activeTarget,
          folder: activeProject?.folder ?? "",
          resume: active.resume,
          contextWindow: active.contextWindow,
          jiraProject: active.jiraProject,
        }
      : null;

  // Open the connect dialog on a first run, so the app is never a blank window
  // with no obvious next step.
  useEffect(() => {
    if (servers.length === 0) setNewMode({ kind: "server" });
    // Only on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Global shortcuts. Pane-scoped ones live in the panes.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;

      if (e.key.toLowerCase() === "n") {
        e.preventDefault();
        setNewMode({ kind: "server" });
      }
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
              {active.name} · {activeHost ?? "local"}
            </span>
          )}
        </div>
      </TitleBar>

      <div className="flex h-full w-full flex-col" style={{ paddingTop: TITLE_BAR_HEIGHT }}>
        {/*
          **Progress replaces the body; it does not float over it.** Every panel
          here is translucent by design, so an overlay let the terminal and the
          rail show straight through the charts — and tinting the sheet to fix
          that would just make the app opaque, which is the thing the appearance
          system exists to avoid. The title bar and footer stay, because they
          carry the way back.
        */}
        {dashboard ? (
          <ErrorBoundary label="Progress">
            <Dashboard onClose={() => setDashboard(false)} />
          </ErrorBoundary>
        ) : (
        <div className="flex min-h-0 flex-1">
          <WorkspaceRail
            onConnectServer={() => setNewMode({ kind: "server" })}
            onNewProject={(serverId) => setNewMode({ kind: "project", serverId })}
          />

          <ErrorBoundary label="The workbench">
            <WorkspaceDeck />
          </ErrorBoundary>

          <ErrorBoundary label="Instruments">
            <WidgetRail session={activeCtx} />
          </ErrorBoundary>
        </div>
        )}

        <footer
          className="flex shrink-0 items-center gap-4 border-t px-3 py-1"
          style={{ borderColor: "var(--border)" }}
        >
          <button
            type="button"
            aria-label={railCollapsed ? "Show the servers panel" : "Hide the servers panel"}
            aria-pressed={!railCollapsed}
            title={railCollapsed ? "Show servers panel" : "Hide servers panel"}
            onClick={toggleRail}
            className="shrink-0"
            style={{ color: railCollapsed ? "var(--text-faint)" : "var(--text-soft)" }}
          >
            <svg
              width="15"
              height="15"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.7"
              strokeLinecap="square"
              aria-hidden="true"
            >
              <rect x="3" y="4" width="18" height="16" />
              <path d="M9 4v16" />
            </svg>
          </button>

          {active && (
            <>
              <span className="micro">{activeHost ? `ssh · ${activeHost}` : "local"}</span>
              {activeProject && (
                <span className="micro truncate" title={activeProject.folder}>
                  {activeProject.folder}
                </span>
              )}
            </>
          )}

          <div className="ml-auto flex items-center gap-4">
            {waitingIds.length > 0 && (
              <button
                type="button"
                className="chip"
                style={{ color: "rgb(var(--primary))" }}
                onClick={() => activate(waitingIds[0]!)}
                title="Go to the session waiting on you"
              >
                {waitingIds.length} waiting
              </button>
            )}

            <button
              type="button"
              className="chip"
              onClick={() => setDashboard((on) => !on)}
              title="Tasks, time, charts and goals across every session"
              style={{ color: dashboard ? "var(--text)" : undefined, background: dashboard ? "var(--hover)" : undefined }}
            >
              PROGRESS
            </button>

            <div className="flex items-center gap-1">
              <span className="micro">VIEW</span>
              {DECKS.map((d) => {
                const on = deckId(deck) === d.id;
                return (
                  <button
                    key={d.id}
                    type="button"
                    className="data px-[5px] text-[10px]"
                    onClick={() => setDeck(d.deck)}
                    title={d.title}
                    style={{
                      border: "1px solid var(--border)",
                      color: on ? "var(--text)" : "var(--text-soft)",
                      background: on ? "var(--hover)" : "transparent",
                      // Selection is not carried by tone alone: at 10px a tonal
                      // step is not a state anyone can see across six chips.
                      textDecoration: on ? "underline" : "none",
                    }}
                  >
                    {d.label}
                  </button>
                );
              })}
            </div>

            {activeTarget && <Metrics target={activeTarget} />}

            <button
              type="button"
              className="chip"
              style={{ color: "var(--text-faint)" }}
              onClick={() => void api.openSettings()}
              title="Accounts, the app lock, and your Claude credential"
            >
              {lock?.locked ? "settings · locked" : "settings"}
            </button>
          </div>
        </footer>
            <AlertStack />
</div>

      <ErrorBoundary label="New" onReset={() => setNewMode(null)}>
        <WorkspaceNewSessionLayer mode={newMode} onClose={() => setNewMode(null)} />
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
      </AnimatePresence>
    </>
  );
}
