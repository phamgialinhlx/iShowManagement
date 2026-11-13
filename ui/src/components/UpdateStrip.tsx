import { useEffect, useState } from "react";

import { api, isTauri } from "../lib/api";
import { listen } from "../lib/bridge";
import {
  AUTO_KEY,
  SKIPPED_KEY,
  UPDATE_EVENT,
  clearSkipped,
  checkOutcome,
  readAutoUpdate,
  readSkipped,
  stripText,
  stripVisible,
  writeAutoUpdate,
  writeSkipped,
  type UpdateStatus,
} from "../lib/updates";

/** The app-wide update status, live. Desktop only — rmux-web never asks. */
export function useUpdateStatus(): UpdateStatus | null {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  useEffect(() => {
    if (!isTauri()) return;
    let alive = true;
    let off: (() => void) | undefined;
    api
      .updateStatus()
      .then((s) => alive && setStatus(s))
      .catch(() => {});
    void listen<UpdateStatus>(UPDATE_EVENT, (e) => setStatus(e.payload)).then((fn) => {
      if (alive) off = fn;
      else fn();
    });
    return () => {
      alive = false;
      off?.();
    };
  }, []);
  return status;
}

/**
 * The update prompt, in the footer beside PROGRESS and TRANSFERS.
 *
 * **Not a dialog, and it never takes focus**: a modal over the workbench takes
 * the keystrokes of whoever is typing in a terminal. **It never restarts on its
 * own** either — RESTART TO UPDATE is the only thing that installs, and sessions
 * survive it because they run under `rmux-agent` on their servers.
 *
 * Absent when there is nothing to say. Also tells Rust, on mount and whenever
 * Settings changes it, whether the background checker should run — off means
 * no timer and no requests (`updates::update_configure`).
 */
export function UpdateStrip() {
  const status = useUpdateStatus();
  const [skipped, setSkipped] = useState(readSkipped);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    void api.updateConfigure(readAutoUpdate()).catch(() => {});
    // Settings is another window; its switch arrives here as a storage event.
    const onStorage = (e: StorageEvent) => {
      if (e.key === AUTO_KEY) void api.updateConfigure(readAutoUpdate()).catch(() => {});
      if (e.key === SKIPPED_KEY) setSkipped(readSkipped());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  if (!status || !stripVisible(status, skipped)) return null;

  const ready = status.phase === "ready";
  const available = status.phase === "available";
  const mac = status.platform === "macos";

  const restart = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.updateRestart();
      // On success the app is going away; nothing more to show.
    } catch (e) {
      // Persists until the next attempt — the only place this failure is said.
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  const later = () => {
    if (available && status.version) {
      writeSkipped(status.version);
      setSkipped(status.version);
    } else {
      void api.updateDismiss().catch(() => {});
    }
  };

  const title = ready
    ? "Installs the update and reopens rmux. Your sessions keep running — they live on their servers, not in this window."
    : available
      ? status.installError
        ? `Couldn't install automatically (${status.installError}). Download the installer instead.`
        : mac
          ? "Downloads the installer in your browser. Quit rmux before replacing it."
          : "Downloads the installer in your browser."
      : "Downloading and verifying the update in the background.";

  return (
    <div className="flex shrink-0 items-center gap-2" title={title}>
      <span className="micro" style={{ color: "var(--text)" }}>
        {stripText(status)}
        {ready && <span style={{ color: "var(--text-soft)" }}> · your sessions keep running</span>}
      </span>
      {ready && (
        <button
          type="button"
          className="chip"
          disabled={busy}
          onClick={() => void restart()}
          style={{ color: "rgb(var(--primary))" }}
        >
          {busy ? "RESTARTING…" : "RESTART TO UPDATE"}
        </button>
      )}
      {available && (
        <button
          type="button"
          className="chip"
          onClick={() => void api.updateOpen("download").catch((e) => setError(String(e?.message ?? e)))}
          style={{ color: "rgb(var(--primary))" }}
        >
          DOWNLOAD
        </button>
      )}
      <button
        type="button"
        className="chip"
        onClick={() => void api.updateOpen("notes").catch((e) => setError(String(e?.message ?? e)))}
      >
        WHAT&rsquo;S NEW
      </button>
      {!busy && (
        <button type="button" className="chip" onClick={later} title="Hide this until the next release (or the next launch)">
          LATER
        </button>
      )}
      {error && (
        <span role="alert" className="micro" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </span>
      )}
    </div>
  );
}

/** Settings › UPDATES. */
export function UpdatesPanel() {
  const status = useUpdateStatus();
  const [auto, setAuto] = useState(readAutoUpdate);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<{ ok: boolean; text: string } | null>(null);

  const toggle = (on: boolean) => {
    setAuto(on);
    writeAutoUpdate(on);
    void api.updateConfigure(on).catch(() => {});
  };

  const checkNow = async () => {
    setBusy(true);
    setOutcome(null);
    try {
      const s = await api.updateCheck();
      setOutcome({ ok: true, text: checkOutcome(s) });
      clearSkipped();
    } catch (e) {
      setOutcome({ ok: false, text: `Couldn't reach GitHub: ${e instanceof Error ? e.message : String(e)}` });
    } finally {
      setBusy(false);
    }
  };

  const selfUpdate = status?.selfUpdate ?? true;

  return (
    <div className="flex flex-col gap-5" style={{ maxWidth: 640 }}>
      <header className="flex flex-col gap-1">
        <h2 className="kicker">UPDATES</h2>
        <p className="data text-[11px] leading-[1.5]" style={{ color: "var(--text-soft)" }}>
          {selfUpdate
            ? "rmux downloads a new release in the background, checks its signature, and then asks you to restart — it never restarts on its own. Your sessions keep running through a restart."
            : "This install cannot replace itself (only the AppImage can, on Linux), so rmux tells you when a new release is out and links to the installer."}
        </p>
      </header>

      <div className="inset flex flex-col gap-3 p-4">
        <div className="flex items-baseline gap-3">
          <span className="micro">THIS VERSION</span>
          <span className="data text-[11px]" style={{ color: "var(--text)" }}>
            {status?.current ?? "—"}
          </span>
        </div>

        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            checked={auto}
            disabled={!isTauri()}
            style={{ accentColor: "rgb(var(--primary))", marginTop: 2 }}
            onChange={(e) => toggle(e.target.checked)}
          />
          <span className="flex flex-col gap-[2px]">
            <span className="data text-[11px]" style={{ color: "var(--text)" }}>
              {selfUpdate ? "Update automatically" : "Check for updates automatically"}
            </span>
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              On by default: a check shortly after launch and every six hours. Off means rmux makes
              no requests on its own; Check now still works.
            </span>
          </span>
        </label>

        <div className="flex items-center gap-3">
          <button type="button" className="btn" disabled={!isTauri() || busy} onClick={() => void checkNow()}>
            {busy ? "Checking…" : "Check now"}
          </button>
          {outcome && (
            <span
              role={outcome.ok ? undefined : "alert"}
              className="data text-[11px]"
              style={{ color: outcome.ok ? "var(--text-soft)" : "rgb(var(--primary))" }}
            >
              {outcome.text}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
