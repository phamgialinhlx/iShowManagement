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
import { SavedViews } from "../components/SavedViews";
import { TransfersControl } from "../components/Transfers";
import { UpdateStrip } from "../components/UpdateStrip";
import { visibleLeaves } from "../lib/layout-tree";
import { readHandsFree, useHandsFree, writeHandsFree } from "../lib/hands-free";
import type { SessionStatus } from "../lib/workspace-model";
import { useWorkspace } from "../lib/workspace";
import { startAttentionWatch } from "../lib/attention";
import { useShortcuts } from "../lib/use-shortcuts";
import { usePrefixKeys } from "../lib/use-prefix-mode";
import { prefixOwns } from "../lib/prefix-runtime";
import { ModeBar, PrefixDialogs, PrefixHelp } from "../components/PrefixOverlays";
import { QuickSwitch } from "../components/QuickSwitch";
import { QuickOpen } from "../components/QuickOpen";
import { startStatusWatch } from "../lib/status-watch";
import { startChildrenWatch } from "../lib/children-watch";
import { useAutoBridge } from "../lib/redstone-auto-bridge";
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
  // Sub-sessions agents spawn on their hosts (`rmux spawn`) appear beside their
  // parent pane without taking focus. See `lib/children-watch.ts`.
  useEffect(() => startChildrenWatch(), []);
  // One watcher for the whole app: attention is singular, so counting it per
  // pane would credit every mounted session at once. See `lib/attention.ts`.
  useEffect(() => startAttentionWatch(), []);
  // Auto-bridge every server to Redstone when signed in — mounted here, above
  // the panels, so it runs regardless of which panel is open. All guards live
  // inside the engine. See `lib/redstone-auto-bridge.ts`.
  useAutoBridge();

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
  const layout = useWorkspace((s) => s.layout);
  const selectTabIndex = useWorkspace((s) => s.selectTabIndex);
  const railCollapsed = useWorkspace((s) => s.railCollapsed);
  const toggleRail = useWorkspace((s) => s.toggleRail);
  const widgetsCollapsed = useWorkspace((s) => s.widgetsCollapsed);
  const toggleWidgets = useWorkspace((s) => s.toggleWidgets);
  const serverOf = useWorkspace((s) => s.serverOf);
  const projectOf = useWorkspace((s) => s.projectOf);
  const targetOf = useWorkspace((s) => s.targetOf);

  const [newMode, setNewMode] = useState<NewMode | null>(null);
  const [dashboard, setDashboard] = useState(false);
  const [handsFree, setHandsFree] = useState(readHandsFree);
  /**
   * The session hands-free last moved to, shown briefly on its own chip.
   *
   * Focus moving on its own is startling without a reason. The rail highlight
   * and the pane outline both follow, so *something* visibly changes — what is
   * missing is **why**, and the control that caused it is the honest place to
   * say so.
   */
  const [wentTo, setWentTo] = useState<string | null>(null);

  // The keyboard, bound once for the whole workbench. See `use-shortcuts.ts`
  // for why it is one listener and why it bubbles rather than captures.
  useShortcuts({ progressOpen: dashboard, onProgress: setDashboard });
  // herdr's prefix layer (`ctrl+b` then a key), on top of the Mod chords above.
  // See `prefix-runtime.ts` for where it intercepts and why.
  usePrefixKeys({
    onNewProject: (serverId) => setNewMode(serverId ? { kind: "project", serverId } : { kind: "server" }),
  });

  const active = sessions.find((s) => s.id === activeId) ?? null;
  const activeProject = active ? projectOf(active.id) : undefined;
  const activeTarget = active ? targetOf(active.id) : undefined;
  const activeHost = active ? serverOf(active.id)?.target.host : undefined;

  /**
   * Hands-free: the keyboard follows whichever session next needs a person.
   *
   * The statuses are mapped to a plain record first so the hook re-runs when a
   * *status* changes rather than on every write to `runtime` — which carries an
   * error field and a timestamp that move far more often.
   */
  const statuses = useMemo(() => {
    const out: Record<string, SessionStatus | undefined> = {};
    // **A session with no runtime entry is idle**, which is what `statusOf` says
    // and what the rail draws. Runtime is never persisted, so every session
    // restored at launch has none until something publishes one — reading the
    // raw record left them `undefined`, nothing was answerable, and hands-free
    // had no candidates on exactly the sessions that had been sitting there
    // longest. One default, in one place, or the app disagrees with itself.
    for (const s of sessions) out[s.id] = runtime[s.id]?.status ?? "idle";
    return out;
  }, [sessions, runtime]);

  /**
   * What is **actually on screen**: the leaves of the active tab of the active
   * project (only the focused one while zoomed). The same derivation the deck
   * renders from and the rail marks from — two sources for one fact is two
   * chances to drift, and the grid's version of this drifted once already.
   */
  const onScreen = useMemo(() => visibleLeaves(layout).map((l) => l.pane), [layout]);

  useHandsFree({
    enabled: handsFree,
    panes: onScreen,
    statuses,
    activeSession: activeId,
    cells: onScreen.length,
    onGo: (id) => {
      // `activate` focuses the leaf and hands its terminal the keyboard.
      activate(id);
      setWentTo(sessions.find((s) => s.id === id)?.name ?? id);
      window.setTimeout(() => setWentTo(null), 2600);
    },
    // Switching it on and getting silence reads as a broken switch, so say when
    // there was simply nothing to go to.
    onNothingWaiting: (reason) => {
      setWentTo(reason === "focus-view" ? "needs a split" : "nothing waiting");
      window.setTimeout(() => setWentTo(null), 3200);
    },
  });

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
      if (prefixOwns(e)) return;
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;

      // Bare Mod+N only: Mod+Shift+N is "new tab" (`shortcuts.ts`), and an
      // unguarded match here would open the connect dialog on top of it.
      if (e.key.toLowerCase() === "n" && !e.shiftKey && !e.altKey) {
        e.preventDefault();
        setNewMode({ kind: "server" });
      }
      // Mod+digit picks the active project's tab by position — what the digit
      // meant for sessions when each one was a tile, and what it means in
      // every tabbed program.
      const digit = Number(e.key);
      if (Number.isInteger(digit) && digit >= 1 && digit <= 9 && !e.shiftKey && !e.altKey) {
        e.preventDefault();
        selectTabIndex(digit - 1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectTabIndex]);

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
          {/*
            Settings lives at the top-right of the title bar rather than in the
            footer — a gear icon is where an operator instinctively reaches for it,
            and it frees the footer's horizontal budget. A bare button (not a
            `.chip`) so no border boxes the icon; buttons auto-block the drag region
            so the window still moves everywhere around it. When the app lock is
            armed a small padlock rides alongside — the icon-only button must not
            drop the "· locked" signal the text button carried.
          */}
          {/*
            A second workbench window, to put on another desktop. Beside the
            gear because both are window-level actions; icon-only for the same
            reason — a bare button auto-blocks the drag region, so the window
            still moves everywhere around it. Each new window carries its own
            layout (see `window-scope.ts`), so this is "another workbench", not
            a clone of this one.
          */}
          <button
            type="button"
            onClick={() => void api.openWindow()}
            aria-label="New window"
            title="Open another window — its own layout, for another desktop"
            className="title-cog inline-flex items-center"
            style={{ height: 22 }}
          >
            <svg
              width="15"
              height="15"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.7"
              strokeLinecap="square"
              strokeLinejoin="miter"
              aria-hidden="true"
            >
              <rect x="3" y="5" width="14" height="14" />
              <path d="M9 9h12v12H9z" />
            </svg>
          </button>
          <button
            type="button"
            onClick={() => void api.openSettings()}
            aria-label={lock?.locked ? "Settings (app lock armed)" : "Settings"}
            title={
              lock?.locked
                ? "Settings — accounts, your Claude credential, and the app lock (armed)"
                : "Settings — accounts, the app lock, and your Claude credential"
            }
            className="title-cog inline-flex items-center gap-1"
            style={{ height: 22 }}
          >
            {lock?.locked && (
              <svg
                width="9"
                height="9"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2.2"
                strokeLinecap="square"
                strokeLinejoin="miter"
                aria-hidden="true"
              >
                <rect x="4" y="11" width="16" height="10" />
                <path d="M8 11V7a4 4 0 0 1 8 0v4" />
              </svg>
            )}
            <svg
              width="15"
              height="15"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.7"
              strokeLinecap="square"
              strokeLinejoin="miter"
              aria-hidden="true"
            >
              <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
              <circle cx="12" cy="12" r="3" />
            </svg>
          </button>
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
          className="chrome-black relative flex shrink-0 items-center gap-4 border-t px-3 py-1"
          style={{ borderColor: "var(--border)" }}
        >
          {/* PREFIX / NAVIGATE / RESIZE take over this row while active. */}
          <ModeBar />
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
            {/* "A newer rmux is ready / available". In the footer, never a
                dialog: a modal would take keystrokes from a terminal. */}
            <UpdateStrip />

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

            {/* Hands-free. In the footer beside the other whole-workbench
                switches, because it changes how the app behaves rather than
                what a pane shows — and because a mode you cannot see the state
                of is a mode you turn on by accident and never find again. */}
            <button
              type="button"
              className="chip"
              aria-pressed={handsFree}
              onClick={() => {
                const next = !handsFree;
                setHandsFree(next);
                writeHandsFree(next);
              }}
              title={
                handsFree
                  ? "Hands-free is on — the cursor moves to a session as it finishes or asks. It waits while you are typing."
                  : "Hands-free: put the cursor in an on-screen pane as soon as its session finishes or asks you something"
              }
              style={{
                color: handsFree ? "var(--text)" : undefined,
                background: handsFree ? "var(--hover)" : undefined,
              }}
            >
              {/* Names where it went, then goes quiet. A mode that narrates
                  every move becomes noise; one that never explains a jump
                  leaves you wondering what you pressed. */}
              {wentTo ? `→ ${wentTo}` : "HANDS-FREE"}
            </button>

            <button
              type="button"
              className="chip"
              onClick={() => setDashboard((on) => !on)}
              title="Tasks, time, charts and goals across every session"
              style={{ color: dashboard ? "var(--text)" : undefined, background: dashboard ? "var(--hover)" : undefined }}
            >
              PROGRESS
            </button>

            <TransfersControl />

            <div className="flex items-center gap-1">
              <SavedViews />
            </div>

            {activeTarget && <Metrics target={activeTarget} />}

            {/*
              The instruments (right) rail's collapse toggle, mirroring the servers
              toggle at the footer's far left. Same icon, flipped horizontally — the
              panel divider sits on the right (`M15 4v16`) because this panel is on
              the right — so the two toggles read as a matched pair pointing at their
              own sides.
            */}
            <button
              type="button"
              aria-label={widgetsCollapsed ? "Show the instruments panel" : "Hide the instruments panel"}
              aria-pressed={!widgetsCollapsed}
              title={widgetsCollapsed ? "Show instruments panel" : "Hide instruments panel"}
              onClick={toggleWidgets}
              className="shrink-0"
              style={{ color: widgetsCollapsed ? "var(--text-faint)" : "var(--text-soft)" }}
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
                <path d="M15 4v16" />
              </svg>
            </button>
          </div>
        </footer>
            <AlertStack />
</div>

      <PrefixHelp />
      <PrefixDialogs />
      <QuickSwitch />
      <QuickOpen />

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
