import { useEffect, useRef, useState } from "react";

import {
  canCancel,
  canRetry,
  formatBytes,
  isActive,
  percent,
  rate,
  useTransfers,
  wireTransfers,
  type TransferEntry,
} from "../lib/transfers";
import { cancelTransfer, retryTransfer, revealTransfer } from "../lib/transfer-run";
import { isTauri } from "../lib/api";

/**
 * TRANSFERS, in the footer beside HANDS-FREE and PROGRESS.
 *
 * Absent until something has transferred — a control for a list that has never
 * had anything in it is a dead control. It counts what is in flight and opens
 * upward, the same popover `SavedViews` uses, because the footer sits at the
 * bottom of the window.
 */
export function TransfersControl() {
  const entries = useTransfers((s) => s.entries);
  const open = useTransfers((s) => s.panelOpen);
  const setOpen = useTransfers((s) => s.setPanelOpen);
  const clearFinished = useTransfers((s) => s.clearFinished);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => wireTransfers(), []);

  // Outside-click and Escape close it — the dismissal every popover here uses.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, setOpen]);

  // The rate and the bars move between progress events only while something runs.
  const active = entries.filter(isActive).length;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!open || !active) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [open, active]);

  if (!entries.length) return null;

  const failed = entries.filter((e) => e.state === "failed" || e.state === "interrupted").length;
  const finished = entries.length - active;

  return (
    <div className="relative" ref={box}>
      <button
        type="button"
        className="chip"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        title="Downloads and uploads — status of every transfer, kept until you dismiss it"
        style={{ color: open || active ? "var(--text)" : undefined, background: open ? "var(--hover)" : undefined }}
      >
        TRANSFERS{active ? ` · ${active}` : ""}
        {!active && failed > 0 && (
          <span style={{ color: "var(--st-blocked)" }}> · {failed} !</span>
        )}
      </button>

      {open && (
        <div
          className="menu absolute right-0 bottom-full mb-1 flex max-h-[70vh] w-[380px] max-w-[calc(100vw-32px)] flex-col gap-[2px] overflow-y-auto p-1"
          style={{ border: "1px solid var(--border)", zIndex: 50 }}
          role="dialog"
          aria-label="Transfers"
        >
          <div className="flex items-center gap-2 px-[6px] py-[3px]">
            <span className="micro" style={{ color: "var(--text-soft)" }}>
              TRANSFERS
            </span>
            <span className="micro" style={{ color: "var(--text-faint)" }}>
              {active ? `${active} in progress` : "nothing in progress"}
            </span>
            {finished > 0 && (
              <button
                type="button"
                className="micro ml-auto px-1"
                onClick={clearFinished}
                title="Remove every finished row; running ones stay"
                style={{ color: "var(--text-soft)" }}
              >
                CLEAR FINISHED
              </button>
            )}
          </div>

          {/* Bounded in the element that scrolls: a list of transfers has no
              natural length, and the header above must stay reachable. */}
          <div className="flex max-h-[52vh] flex-col gap-[2px] overflow-y-auto">
            {entries.map((e) => (
              <TransferRow key={e.id} entry={e} now={now} />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

const STATE_LABEL: Record<TransferEntry["state"], string> = {
  queued: "QUEUED",
  running: "RUNNING",
  done: "DONE",
  failed: "FAILED",
  cancelled: "CANCELLED",
  interrupted: "INTERRUPTED",
};

function stateColor(e: TransferEntry): string {
  switch (e.state) {
    case "running":
      return "var(--st-working)";
    case "done":
      return "var(--st-done)";
    case "failed":
    case "interrupted":
      return "var(--st-blocked)";
    default:
      return "var(--text-faint)";
  }
}

function TransferRow({ entry: e, now }: { entry: TransferEntry; now: number }) {
  const dismiss = useTransfers((s) => s.dismiss);
  const [actionError, setActionError] = useState<string | null>(null);
  const pct = percent(e);
  const r = rate(e, now);
  const reveal = isTauri();

  const bytes =
    e.total !== null
      ? `${formatBytes(e.done)} / ${formatBytes(e.total)}`
      : e.done > 0
        ? formatBytes(e.done)
        : "size unknown";

  const phase = e.state === "running" && e.direction === "up" && e.phase ? ` · ${e.phase}` : "";

  return (
    <div
      className="flex flex-col gap-[4px] px-[6px] py-[6px]"
      style={{ borderRadius: "var(--radius)", background: "var(--surface0)" }}
      data-transfer-state={e.state}
    >
      <div className="flex min-w-0 items-center gap-2">
        <span
          className="data shrink-0 text-[12px]"
          aria-label={e.direction === "down" ? "download" : "upload"}
          style={{ color: "var(--accent)" }}
        >
          {e.direction === "down" ? "↓" : "↑"}
        </span>
        <span className="data min-w-0 flex-1 truncate text-[11px]" style={{ color: "var(--text)" }} title={e.remotePath}>
          {e.name}
        </span>
        <span className="micro shrink-0" style={{ color: stateColor(e) }}>
          {STATE_LABEL[e.state]}
        </span>
      </div>

      <div className="flex min-w-0 items-center gap-2">
        <span className="micro min-w-0 truncate" style={{ color: "var(--text-faint)" }} title={e.server}>
          {e.server}
        </span>
      </div>

      {(e.state === "running" || e.state === "queued") && (
        <div
          className="relative h-[3px] w-full overflow-hidden"
          style={{ background: "var(--border)", borderRadius: "var(--radius)" }}
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct ?? undefined}
        >
          {pct === null ? (
            // Indeterminate on purpose: the size is not known yet, and a
            // percentage nobody measured is an invented measurement.
            e.state === "running" && <div className="sweep" style={{ height: "100%", width: "38%" }} />
          ) : (
            <div style={{ height: "100%", width: `${pct}%`, background: "var(--accent)" }} />
          )}
        </div>
      )}

      <div className="flex min-w-0 items-center gap-2">
        <span className="data min-w-0 truncate text-[10px]" style={{ color: "var(--text-soft)" }}>
          {e.state === "done" && e.direction === "down" && e.localPath ? e.localPath : bytes}
          {e.state === "running" && pct !== null ? ` · ${pct}%` : ""}
          {phase}
          {r !== null ? ` · ${formatBytes(Math.round(r))}/s` : ""}
        </span>

        <span className="ml-auto flex shrink-0 items-center gap-2">
          {e.state === "done" && e.direction === "down" && e.localPath && reveal && (
            <RowButton
              label={navigator.platform.toLowerCase().includes("mac") ? "SHOW IN FINDER" : "OPEN FOLDER"}
              onClick={() => {
                setActionError(null);
                revealTransfer(e).catch((err) => setActionError(err instanceof Error ? err.message : String(err)));
              }}
            />
          )}
          {canRetry(e) && <RowButton label="RETRY" onClick={() => retryTransfer(e)} />}
          {canCancel(e) && <RowButton label="CANCEL" onClick={() => void cancelTransfer(e)} />}
          {!isActive(e) && <RowButton label="DISMISS" onClick={() => dismiss(e.id)} />}
        </span>
      </div>

      {/* Failures and interruptions say why, and stay until dismissed. */}
      {e.error && (e.state === "failed" || e.state === "interrupted") && (
        <p role="alert" className="data text-[10px] leading-relaxed" style={{ color: "var(--st-blocked)" }}>
          {e.error}
        </p>
      )}
      {actionError && (
        <p role="alert" className="data text-[10px] leading-relaxed" style={{ color: "var(--st-blocked)" }}>
          {actionError}
        </p>
      )}
    </div>
  );
}

function RowButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      className="micro px-1"
      onClick={onClick}
      style={{ color: "var(--text-soft)" }}
      onMouseEnter={(ev) => (ev.currentTarget.style.color = "var(--text)")}
      onMouseLeave={(ev) => (ev.currentTarget.style.color = "var(--text-soft)")}
    >
      {label}
    </button>
  );
}
