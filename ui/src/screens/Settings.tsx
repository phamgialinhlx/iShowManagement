import { useEffect, useState } from "react";

import { TitleBar, TITLE_BAR_HEIGHT } from "../components/TitleBar";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { ClaudeAccountPanel } from "../components/ClaudeAccountPanel";
import { ModelProfilesPanel } from "../components/ModelProfilesPanel";
import { DiagnosticsPanel } from "../components/DiagnosticsPanel";
import { LockPanel } from "../components/LockPanel";
import { CoworkPanel } from "../components/CoworkPanel";
import { RedstonePanel } from "../components/RedstonePanel";
import { AppearancePanel } from "../components/AppearancePanel";
import { NotificationsPanel } from "../components/NotificationsPanel";
import { EditorPanel } from "../components/EditorPanel";
import { ShortcutSettings } from "../components/ShortcutSettings";
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

type Section =
  | "claude"
  | "models"
  | "lock"
  | "cowork"
  | "redstone"
  | "notifications"
  | "editor"
  | "shortcuts"
  | "appearance"
  | "diagnostics";

const SECTIONS: { id: Section; label: string; blurb: string }[] = [
  { id: "claude", label: "CLAUDE", blurb: "The account your sessions run as" },
  { id: "models", label: "MODELS", blurb: "Run against Kimi, GLM or a gateway" },
  { id: "lock", label: "LOCK", blurb: "Ask for a PIN or a face on every start" },
  { id: "cowork", label: "COWORK", blurb: "Your team's server, if you use one" },
  { id: "redstone", label: "REDSTONE", blurb: "Let its agent drive a server's sessions" },
  { id: "notifications", label: "NOTIFICATIONS", blurb: "When a session wants you" },
  { id: "editor", label: "EDITOR", blurb: "Whether edits save themselves" },
  { id: "shortcuts", label: "SHORTCUTS", blurb: "Move around without the mouse" },
  { id: "appearance", label: "APPEARANCE", blurb: "Colours, backdrop and interface scale" },
  { id: "diagnostics", label: "DIAGNOSTICS", blurb: "Export the log when something breaks" },
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
                background: section === s.id ? "color-mix(in srgb, var(--text) 6%, transparent)" : "transparent",
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

        {/*
          Bottom padding belongs to the panel, not to the scroller.

          Appearance ends in a `sticky bottom-0` Apply bar, and a sticky box is
          clamped to its containing block — which ends at this element's
          *content* edge. With `p-6` here that edge sat 24px above the
          scrollport, so the bar pinned 24px short and a strip of the page kept
          scrolling underneath it: a footer that read as a card floating over the
          content. Measured at bar bottom 963 against scrollport 987, and only
          removing this padding closed it — padding on the panel itself does
          nothing, because the clamp is to the content box.

          Every other panel gets the same 24px back via `pb-6` below, so nothing
          else moves.
        */}
        <main className="min-h-0 flex-1 overflow-y-auto px-6 pt-6">
          {/* Appearance owns its own bottom edge — its sticky Apply bar must be
              able to reach the scrollport. Everything else gets the 24px back. */}
          <div className={section === "appearance" ? undefined : "pb-6"}>
          {/* Each section is boundaried on its own: a failure reading the Claude
              credential must not take the lock controls down with it. */}
          {section === "claude" && (
            <ErrorBoundary label="Claude account">
              <ClaudeAccountPanel />
            </ErrorBoundary>
          )}
          {section === "models" && (
            <ErrorBoundary label="Model profiles">
              <ModelProfilesPanel />
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
          {section === "editor" && (
            <ErrorBoundary label="Editor">
              <EditorPanel />
            </ErrorBoundary>
          )}
          {section === "shortcuts" && (
            <ErrorBoundary label="Shortcuts">
              <ShortcutSettings />
            </ErrorBoundary>
          )}
          {section === "appearance" && (
            <ErrorBoundary label="Appearance">
              <AppearancePanel />
            </ErrorBoundary>
          )}
          {section === "diagnostics" && (
            <ErrorBoundary label="Diagnostics">
              <DiagnosticsPanel />
            </ErrorBoundary>
          )}
          {section === "cowork" && (
            <ErrorBoundary label="Cowork account">
              <CoworkPanel session={session} onSession={setSession} />
            </ErrorBoundary>
          )}

          {section === "redstone" && (
            <ErrorBoundary label="Redstone bridge">
              <RedstonePanel />
            </ErrorBoundary>
          )}
          </div>
        </main>
      </div>
    </div>
  );
}
