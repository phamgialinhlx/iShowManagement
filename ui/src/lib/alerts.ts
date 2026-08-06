import { create } from "zustand";

/**
 * The alerts waiting to be seen.
 *
 * **Never `window.alert`.** That blocks the webview's main thread, which here
 * means every terminal stops drawing, every Claude pane freezes, and the poller
 * that raised the alert stops running — so a message about one session would
 * halt all sixteen. It is also modal to the whole window, so a second session
 * asking a question during the first alert is silently dropped.
 *
 * So an alert is *state*, and the UI renders it. That keeps three properties
 * that matter:
 *
 * - **Several can be outstanding at once.** Running many sessions is the point
 *   of the app; two stopping together is ordinary, not an edge case.
 * - **Nothing is destroyed by it.** Rule: nothing moves under the operator's
 *   hands. An alert does not steal focus, does not switch session, and does not
 *   interrupt what is being typed — it offers a way to go there.
 * - **It is dismissible and it is remembered.** One alert per session at a
 *   time, replaced rather than stacked, or a session that flips between asking
 *   and working leaves a pile of identical cards.
 */
export type Alert = {
  sessionId: string;
  title: string;
  body: string;
  /** Claude is *asking*, rather than having finished. Rule 0: only this is red. */
  asking: boolean;
  /** For ordering, and so the card can say how long it has been waiting. */
  at: number;
};

type AlertStore = {
  alerts: Alert[];
  raise: (a: Omit<Alert, "at">) => void;
  dismiss: (sessionId: string) => void;
  clear: () => void;
};

export const useAlerts = create<AlertStore>((set) => ({
  alerts: [],

  raise: (a) =>
    set((s) => ({
      // Replace this session's outstanding alert rather than adding a second.
      // "Claude finished" superseding "Claude is waiting" is the newer truth,
      // and two cards for one session is two things to dismiss for one fact.
      alerts: [...s.alerts.filter((x) => x.sessionId !== a.sessionId), { ...a, at: Date.now() }],
    })),

  dismiss: (sessionId) =>
    set((s) => ({ alerts: s.alerts.filter((a) => a.sessionId !== sessionId) })),

  clear: () => set({ alerts: [] }),
}));

/**
 * Raise an alert from outside React.
 *
 * `notify.ts` runs on a plain subscription, not inside a component, so it
 * cannot use the hook.
 */
export function raiseAlert(a: Omit<Alert, "at">): void {
  useAlerts.getState().raise(a);
}
