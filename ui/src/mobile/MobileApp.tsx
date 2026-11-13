import { useEffect, useMemo } from "react";

import { useWorkspace } from "../lib/workspace";
import { startStatusWatch } from "../lib/status-watch";
import { groupSessions } from "../lib/mobile-sessions";
import { useVisualViewport } from "./viewport";
import { useRoute } from "./route";
import { SessionList } from "./SessionList";
import { ChatScreen } from "./ChatScreen";
import { BackIcon } from "./icons";

const SHOWN_KINDS = new Set(["claude", "pi", "codex", "terminal"]);

/**
 * rmux on a phone or a tablet: the session list, and a conversation.
 *
 * Phone (narrow): one screen at a time, list → chat with a back button.
 * Tablet (≥ 768px): both, side by side. The split is CSS (`data-screen`), so a
 * rotating iPad changes layout without remounting the conversation.
 */
export function MobileApp() {
  useVisualViewport();
  // The same status stream the desktop rail uses, started once for the app.
  useEffect(() => startStatusWatch(), []);

  const { route, open, setView, back } = useRoute();
  const servers = useWorkspace((s) => s.servers);
  const projects = useWorkspace((s) => s.projects);
  const sessions = useWorkspace((s) => s.sessions);
  const runtime = useWorkspace((s) => s.runtime);
  const seen = useWorkspace((s) => s.seen);

  const groups = useMemo(
    () => groupSessions({ servers, projects, sessions, runtime, seen }),
    [servers, projects, sessions, runtime, seen],
  );

  const selected = route.session ? sessions.find((s) => s.id === route.session && SHOWN_KINDS.has(s.kind)) : undefined;

  return (
    <div className="m-app" data-screen={route.session ? "chat" : "list"}>
      <aside className="m-pane-list">
        <SessionList groups={groups} selectedId={route.session} onOpen={open} />
      </aside>
      <main className="m-pane-chat">
        {selected ? (
          <ChatScreen key={selected.id} session={selected} view={route.view} onView={setView} onBack={back} />
        ) : route.session ? (
          <div className="m-chat-screen">
            <header className="m-top">
              <button type="button" className="m-icon-btn m-back" onClick={back} aria-label="Back to sessions">
                <BackIcon />
              </button>
              <h1 className="m-title">Session not found</h1>
            </header>
            <div className="m-empty">
              <p className="m-empty-title">That session is not in this browser's workspace</p>
              <p className="m-empty-body">It may have been closed. Go back to the list to pick another.</p>
            </div>
          </div>
        ) : (
          <div className="m-empty m-empty-center">
            <p className="m-empty-title">Pick a session</p>
            <p className="m-empty-body">Its conversation opens here.</p>
          </div>
        )}
      </main>
    </div>
  );
}
