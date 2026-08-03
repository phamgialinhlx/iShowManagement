import { useEffect, useState } from "react";

import { TitleBar, TITLE_BAR_HEIGHT } from "../components/TitleBar";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { ClaudeAccountPanel } from "../components/ClaudeAccountPanel";
import { LockPanel } from "../components/LockPanel";
import { CoworkPanel } from "../components/CoworkPanel";
import { AppearancePanel } from "../components/AppearancePanel";
import { NotificationsPanel } from "../components/NotificationsPanel";
import { api, isTauri, type SignedIn } from "../lib/api";
import { SERVER_KEY } from "../components/SignIn";

/**
 * Settings, in its own window.
 *
 * Three things live here and they are genuinely separate credentials, so the
 * page keeps them separate rather than blending them into one "account" idea:
 *
 * - **Cowork** — the team session. Optional; the workbench works without it.
 * - **The app lock** — a PIN that encrypts that session, and optionally a face.
 * - **Claude** — the credential sessions actually run with, which has nothing to
 *   do with Cowork and is useful on its own.
 *
 * Conflating them was the flaw in the old rail widget: it implied that signing
 * in to one had something to do with the others.
 */

type Section = "claude" | "lock" | "cowork" | "notifications" | "appearance";

const SECTIONS: { id: Section; label: string; blurb: string }[] = [
  { id: "claude", label: "CLAUDE", blurb: "The account your sessions run as" },
  { id: "lock", label: "LOCK", blurb: "Ask for a PIN or a face on every start" },
  { id: "cowork", label: "COWORK", blurb: "Your team's server, if you use one" },
  { id: "notifications", label: "NOTIFICATIONS", blurb: "When a session wants you" },
  { id: "appearance", label: "APPEARANCE", blurb: "How much desktop shows through" },
];

export function Settings() {
  const [section, setSection] = useState<Section>("claude");
  const [session, setSession] = useState<SignedIn | null>(null);

  // Settings is its own window with its own JS context, so it does not inherit
  // the workbench's session object and has to ask for itself.
  useEffect(() => {
    if (!isTauri()) return;
    const serverUrl = localStorage.getItem(SERVER_KEY);
    if (!serverUrl) return;

    let cancelled = false;
    api
      .resumeSession(serverUrl)
      .then((s) => !cancelled && s && setSession(s))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    // `h-full w-full`, not `h-screen w-screen`. Viewport units resolve against
    // the real viewport and are *not* scaled by `zoom`, so under any interface
    // scale above 100% a `100vh` box renders that much taller than the window it
    // is in and the sheet overflows its own frame. `#root` is already exactly
    // the window (see `signal-room.css`), so filling the parent is both correct
    // and scale-proof.
    <div className="flex h-full w-full flex-col overflow-hidden">
      {/* The same backdrop the workbench paints. `body` is transparent by
          design — the native window is translucent and this layer is what makes
          it read as glass — so a screen that omits it is genuinely see-through,
          and whatever window sits behind shows through the content. */}
      <div className="atmosphere" />
      <TitleBar />

      <div className="flex min-h-0 flex-1" style={{ paddingTop: TITLE_BAR_HEIGHT }}>
        <nav
          className="flex w-[190px] shrink-0 flex-col gap-1 border-r p-3"
          style={{ borderColor: "var(--border)" }}
        >
          <span className="kicker mb-2">SETTINGS</span>
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => setSection(s.id)}
              className="flex flex-col gap-[2px] px-2 py-[6px] text-left"
              style={{
                background: section === s.id ? "rgba(232,230,225,0.06)" : "transparent",
                borderLeft:
                  section === s.id ? "2px solid rgb(var(--primary))" : "2px solid transparent",
              }}
            >
              <span
                className="micro"
                style={{ color: section === s.id ? "var(--text)" : "var(--text-soft)" }}
              >
                {s.label}
              </span>
              <span className="data text-[9.5px] leading-tight" style={{ color: "var(--text-faint)" }}>
                {s.blurb}
              </span>
            </button>
          ))}
        </nav>

        <main className="min-h-0 flex-1 overflow-y-auto p-6">
          {/* Each section is boundaried on its own: a failure reading the Claude
              credential must not take the lock controls down with it. */}
          {section === "claude" && (
            <ErrorBoundary label="Claude account">
              <ClaudeAccountPanel />
            </ErrorBoundary>
          )}
          {section === "lock" && (
            <ErrorBoundary label="App lock">
              <LockPanel session={session} />
            </ErrorBoundary>
          )}
          {section === "notifications" && (
            <ErrorBoundary label="Notifications">
              <NotificationsPanel />
            </ErrorBoundary>
          )}
          {section === "appearance" && (
            <ErrorBoundary label="Appearance">
              <AppearancePanel />
            </ErrorBoundary>
          )}
          {section === "cowork" && (
            <ErrorBoundary label="Cowork account">
              <CoworkPanel session={session} onSession={setSession} />
            </ErrorBoundary>
          )}
        </main>
      </div>
    </div>
  );
}
