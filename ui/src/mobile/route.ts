import { useCallback, useEffect, useState } from "react";

/**
 * Which session is open, and in which view — kept in the URL (`?s=<id>&v=terminal`)
 * so a phone's own Back gesture goes back to the list, a reload reopens the same
 * conversation, and a check harness can address each screen directly.
 */
export type View = "chat" | "terminal";
export type Route = { session: string | null; view: View };

const MARK = "rmuxMobile";

function read(): Route {
  const q = new URLSearchParams(window.location.search);
  return { session: q.get("s"), view: q.get("v") === "terminal" ? "terminal" : "chat" };
}

function href(r: Route): string {
  const q = new URLSearchParams();
  if (r.session) q.set("s", r.session);
  if (r.session && r.view === "terminal") q.set("v", "terminal");
  const search = q.toString();
  return `${window.location.pathname}${search ? `?${search}` : ""}${window.location.hash}`;
}

export function useRoute() {
  const [route, setRoute] = useState<Route>(read);

  useEffect(() => {
    const onPop = () => setRoute(read());
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  /** Open a session (a history entry, so Back returns to the list). */
  const open = useCallback((session: string) => {
    const next: Route = { session, view: "chat" };
    window.history.pushState({ [MARK]: true }, "", href(next));
    setRoute(next);
  }, []);

  /** Switch view in place — a toggle, not a place to go back from. */
  const setView = useCallback((view: View) => {
    setRoute((r) => {
      const next = { ...r, view };
      window.history.replaceState(window.history.state, "", href(next));
      return next;
    });
  }, []);

  /** Back to the list: undo our own entry when there is one, so the browser's
   *  history stays one step per screen. */
  const back = useCallback(() => {
    const state = window.history.state as Record<string, unknown> | null;
    if (state?.[MARK]) {
      window.history.back();
      return;
    }
    const next: Route = { session: null, view: "chat" };
    window.history.replaceState(null, "", href(next));
    setRoute(next);
  }, []);

  return { route, open, setView, back };
}
