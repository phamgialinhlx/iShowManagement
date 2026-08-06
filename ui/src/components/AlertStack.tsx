import { AnimatePresence, motion } from "motion/react";

import { useAlerts } from "../lib/alerts";
import { useWorkspace } from "../lib/workspace";

/**
 * Sessions that need you, said louder than a banner.
 *
 * A macOS notification slides away on its own — fine for "this finished", not
 * enough for a session that has stopped to ask something and is blocking
 * everything behind it. This stays until it is dealt with, because an alert
 * that expires by itself is one the operator misses by being away from the
 * desk, which is exactly when a long run stops.
 *
 * **It does not take the screen.** Bottom-right, above the status bar, over
 * nothing that is being typed into. Clicking it goes to that session; the cross
 * dismisses it. Nothing here moves a pane or steals focus on its own — the rule
 * is that nothing moves under the operator's hands, and an alert is the case
 * where breaking it would be most tempting and most wrong.
 */
export function AlertStack() {
  const alerts = useAlerts((s) => s.alerts);
  const dismiss = useAlerts((s) => s.dismiss);
  const clear = useAlerts((s) => s.clear);
  const activate = useWorkspace((s) => s.activate);

  if (!alerts.length) return null;

  return (
    <div
      className="pointer-events-none fixed right-3 bottom-10 z-50 flex flex-col items-end gap-2"
      role="region"
      aria-label="Sessions needing attention"
    >
      <AnimatePresence initial={false}>
        {alerts.map((alert) => (
          <motion.div
            key={alert.sessionId}
            layout
            initial={{ opacity: 0, x: 16 }}
            animate={{ opacity: 1, x: 0 }}
            exit={{ opacity: 0, x: 16 }}
            transition={{ duration: 0.16, ease: [0.2, 0.9, 0.3, 1] }}
            className="window pointer-events-auto flex w-[300px] items-start gap-2 px-3 py-[10px]"
            style={{
              // Rule 0: red only where the operator must act. A question is
              // exactly that; a finished run is not, so it stays monochrome.
              borderLeft: `3px solid ${alert.asking ? "rgb(var(--primary))" : "var(--text)"}`,
            }}
          >
            <button
              type="button"
              className="flex min-w-0 flex-1 flex-col gap-[3px] text-left"
              onClick={() => {
                activate(alert.sessionId);
                dismiss(alert.sessionId);
              }}
              title="Go to this session"
            >
              <span className="micro" style={{ color: alert.asking ? "rgb(var(--primary))" : "var(--text-soft)" }}>
                {alert.asking ? "NEEDS YOU" : "FINISHED"}
              </span>
              <span className="data truncate text-[12px]" style={{ color: "var(--text)" }}>
                {alert.title}
              </span>
              <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
                {alert.body}
              </span>
            </button>

            <button
              type="button"
              aria-label="Dismiss"
              className="grid h-[14px] w-[14px] shrink-0 place-items-center"
              style={{ color: "var(--text-faint)" }}
              onClick={() => dismiss(alert.sessionId)}
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
          </motion.div>
        ))}
      </AnimatePresence>

      {/* Only once there are enough for dismissing them one at a time to be a
          chore. Below that it is a control that costs more than it saves. */}
      {alerts.length > 2 && (
        <button type="button" className="chip pointer-events-auto" onClick={clear}>
          DISMISS ALL {alerts.length}
        </button>
      )}
    </div>
  );
}
