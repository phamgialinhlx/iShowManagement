import { useEffect, useState } from "react";

import { api, isTauri, type LogStatus } from "../lib/api";

/**
 * Getting the log out of the app.
 *
 * ## Why this is a button and not a path in the docs
 *
 * rmux runs on three platforms and its log lives somewhere different on each —
 * `~/Library/Application Support`, `%APPDATA%`, `~/.local/share`. Telling
 * someone to find it is telling them to fail. The button writes one file to the
 * desktop and prints the full path, so "send me that file" is an instruction
 * anyone can follow.
 *
 * ## It says what is in there before it is sent
 *
 * The export carries the app version, the OS and which agent binaries shipped —
 * facts the person reporting a problem is least able to supply, and which the
 * log lines are meaningless without. It also contains paths and host names from
 * the operator's own work, so the panel says so *before* they attach it to
 * anything. A diagnostic bundle that quietly includes more than expected is how
 * people end up sending things they did not mean to.
 */
export function DiagnosticsPanel() {
  const [status, setStatus] = useState<LogStatus | null>(null);
  const [exported, setExported] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    if (!isTauri()) return;
    api
      .logStatus()
      .then(setStatus)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  };

  useEffect(refresh, []);

  const save = async () => {
    setBusy(true);
    setError(null);
    setExported(null);
    try {
      setExported(await api.logExport());
      refresh();
    } catch (e) {
      // Persists until the next attempt — this is the only place the operator
      // learns it failed.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const copyPath = async (value: string) => {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      // The path is already on screen; failing to copy costs nothing.
    }
  };

  return (
    <div className="flex flex-col gap-5" style={{ maxWidth: 640 }}>
      <header className="flex flex-col gap-1">
        <h2 className="kicker">DIAGNOSTICS</h2>
        <p className="data text-[11px] leading-[1.5]" style={{ color: "var(--text-soft)" }}>
          rmux keeps a log of what it did — connections, agent installs, errors. When something
          goes wrong on a machine that is not in front of you, this is the evidence.
        </p>
      </header>

      <div className="inset flex flex-col gap-3 p-4">
        <div className="flex items-baseline gap-3">
          <span className="micro">CURRENT LOG</span>
          <span className="data text-[11px]" style={{ color: "var(--text)" }}>
            {status ? formatSize(status.bytes) : "…"}
          </span>
          {status?.hasPrevious && (
            <span className="micro">PREVIOUS RUN KEPT</span>
          )}
        </div>

        {status && (
          <button
            type="button"
            className="link truncate text-left"
            title={`${status.path} — click to copy`}
            onClick={() => void copyPath(status.path)}
            style={{ color: "var(--text-faint)" }}
          >
            {status.path}
          </button>
        )}

        <div className="flex items-center gap-3">
          <button type="button" className="btn btn-primary" disabled={busy} onClick={save}>
            {busy ? "Writing…" : "Export log"}
          </button>
          <button type="button" className="btn" onClick={refresh} disabled={busy}>
            Refresh
          </button>
        </div>

        {exported && (
          <div className="flex flex-col gap-1">
            <span className="data text-[11px]" style={{ color: "var(--text)" }}>
              Saved to your desktop.
            </span>
            {/* The path, in full and copyable. "Check your desktop" is not an
                answer when the file is one of forty things on it. */}
            <button
              type="button"
              className="data text-left text-[10.5px]"
              onClick={() => void copyPath(exported)}
              title="click to copy"
              style={{ color: "var(--text-soft)", wordBreak: "break-all" }}
            >
              {exported}
            </button>
          </div>
        )}

        {error && (
          <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}
      </div>

      {/* Said before the file is sent, not after. */}
      <p className="data text-[10.5px] leading-[1.5]" style={{ color: "var(--text-faint)" }}>
        The export includes your app version, operating system, and the host names and file paths
        rmux worked with. It never contains your Claude token, your model-profile credentials, or
        your Cowork session — those are held in the OS keychain and are not logged. Read it before
        you send it if any of your host names are sensitive.
      </p>
    </div>
  );
}

/** Bytes as something a person reads, not a number to decode. */
function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} bytes`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
