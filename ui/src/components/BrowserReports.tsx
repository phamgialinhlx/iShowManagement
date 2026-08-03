import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { invoke } from "@tauri-apps/api/core";

import { onReport, selectionPrompt, type IncomingReport } from "../lib/control";

/**
 * What the browser sent back.
 *
 * The loop this closes: point at the broken button in rbrowse, type what is
 * wrong, and it arrives here — beside the Claude that can go and fix it, in the
 * session whose server is serving that page.
 *
 * **Nothing is sent to Claude automatically, and that is a security property
 * rather than caution.** A report describes a page rmux did not write. Its
 * selectors, its text and its console output are all chosen by whoever wrote
 * that page, and a line in a page that reads like an instruction is exactly the
 * shape of a prompt injection. So a report is *typed into the composer* and
 * left there: the operator reads it and presses Enter, or does not.
 *
 * `claude_write` and not `claude_send` for the same reason — `send` submits.
 */
export function BrowserReports({
  sessionId,
  claudeId,
}: {
  sessionId: string;
  /** The live Claude PTY, when there is one. Without it there is nowhere to type. */
  claudeId: string | null;
}) {
  const [reports, setReports] = useState<IncomingReport[]>([]);

  useEffect(
    () =>
      onReport((report) => {
        if (report.session !== sessionId) return;
        // Newest first, and bounded: a page with a chatty console could
        // otherwise push an unbounded list into a pane meant for a conversation.
        setReports((current) => [report, ...current].slice(0, 8));
      }),
    [sessionId],
  );

  if (!reports.length) return null;

  const drop = (index: number) =>
    setReports((current) => current.filter((_, i) => i !== index));

  return (
    <div className="flex shrink-0 flex-col gap-[1px] border-b" style={{ borderColor: "var(--border)" }}>
      <AnimatePresence initial={false}>
        {reports.map((report, index) => (
          <motion.div
            key={`${report.report}-${index}-${report.url}`}
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: "auto" }}
            exit={{ opacity: 0, height: 0 }}
            transition={{ duration: 0.16, ease: [0.2, 0.9, 0.3, 1] }}
            className="overflow-hidden"
            style={{ background: "var(--hover)" }}
          >
            <div className="flex items-start gap-3 px-3 py-2">
              <div className="flex min-w-0 flex-1 flex-col gap-[3px]">
                <span className="micro">FROM THE BROWSER · {report.report.toUpperCase()}</span>
                <span className="data truncate text-[11px]" style={{ color: "var(--text-soft)" }}>
                  {report.url}
                </span>
                {report.report === "selection" && (
                  <>
                    <span className="data truncate text-[11px]" style={{ color: "var(--text)" }}>
                      {report.selector}
                    </span>
                    {report.note && (
                      <span className="data text-[11.5px]" style={{ color: "var(--text)" }}>
                        {report.note}
                      </span>
                    )}
                  </>
                )}
                {report.report === "console" && (
                  <span className="data text-[11px]" style={{ color: "var(--text)" }}>
                    {report.entries.length} line{report.entries.length === 1 ? "" : "s"}
                    {/* Errors are the reason anyone sends console output, so they
                        are counted separately rather than left to be found. */}
                    {(() => {
                      const errors = report.entries.filter((e) => e.level === "error").length;
                      return errors ? `, ${errors} error${errors === 1 ? "" : "s"}` : "";
                    })()}
                  </span>
                )}
                {report.report === "viewport" && (
                  <span className="data text-[11px]" style={{ color: "var(--text)" }}>
                    {report.viewport.width}×{report.viewport.height}
                    {report.viewport.devicePixelRatio ? ` @${report.viewport.devicePixelRatio}x` : ""}
                  </span>
                )}
                {report.report === "screenshot" && (
                  <img
                    // Data URI rather than a blob: this is already base64 on the
                    // wire, and a blob URL would have to be revoked on dismiss
                    // or leak for the life of the app.
                    src={`data:image/png;base64,${report.png}`}
                    alt="from the browser"
                    className="mt-1 max-h-[120px] w-auto"
                    style={{ border: "1px solid var(--border)" }}
                  />
                )}
              </div>

              <div className="flex shrink-0 flex-col items-end gap-1">
                {report.report === "selection" && (
                  <button
                    type="button"
                    className="chip"
                    disabled={!claudeId}
                    title={
                      claudeId
                        ? "Type this into Claude's composer. You still press Enter."
                        : "Claude is not running in this session yet"
                    }
                    style={{ color: claudeId ? "var(--text)" : "var(--text-faint)" }}
                    onClick={() => {
                      if (!claudeId) return;
                      void invoke("claude_write", {
                        id: claudeId,
                        // No trailing newline. See the component comment: this
                        // fills the composer, it does not submit.
                        data: selectionPrompt(report),
                      });
                      drop(index);
                    }}
                  >
                    TYPE INTO CLAUDE
                  </button>
                )}
                {(report.report === "console" || report.report === "har") && (
                  <button
                    type="button"
                    className="chip"
                    onClick={() => {
                      void navigator.clipboard.writeText(
                        report.report === "har"
                          ? report.har
                          : report.entries.map((e) => `${e.level}: ${e.text}`).join("\n"),
                      );
                    }}
                  >
                    COPY
                  </button>
                )}
                <button
                  type="button"
                  className="chip"
                  style={{ color: "var(--text-faint)" }}
                  onClick={() => drop(index)}
                >
                  DISMISS
                </button>
              </div>
            </div>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
