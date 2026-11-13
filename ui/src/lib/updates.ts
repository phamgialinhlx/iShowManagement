/**
 * The update strip's model. The checker itself is in Rust (`src-tauri/src/updates.rs`),
 * one per app, broadcasting `rmux-update` to every window; this module only
 * decides what a window shows and remembers the two preferences.
 *
 * Pure apart from `localStorage`, so `ui/update-check.ts` can pin it in Node.
 */

export type UpdatePhase = "idle" | "available" | "downloading" | "ready";

/** What Rust broadcasts. It carries no URL: links are opened by *kind*. */
export type UpdateStatus = {
  current: string;
  platform: "macos" | "windows" | "linux";
  phase: UpdatePhase;
  version: string | null;
  done: number;
  total: number | null;
  selfUpdate: boolean;
  dismissed: boolean;
  checking: boolean;
  auto: boolean;
  installError: string | null;
};

export const UPDATE_EVENT = "rmux-update";
export const AUTO_KEY = "rmux.update.auto";
export const SKIPPED_KEY = "rmux.update.skipped";

function storage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** "Check for updates automatically" — on unless switched off. */
export function readAutoUpdate(): boolean {
  return storage()?.getItem(AUTO_KEY) !== "off";
}

export function writeAutoUpdate(on: boolean): void {
  try {
    storage()?.setItem(AUTO_KEY, on ? "on" : "off");
  } catch {
    // An unwritable store costs the preference, not the app.
  }
}

/** The version "Later" was pressed on, for a release this install can only
 *  download. Just the version string — a newer release shows again. */
export function readSkipped(): string | null {
  return storage()?.getItem(SKIPPED_KEY) ?? null;
}

export function writeSkipped(version: string): void {
  try {
    storage()?.setItem(SKIPPED_KEY, version);
  } catch {
    // As above.
  }
}

/** A manual check is a request to see the answer again, even after "Later". */
export function clearSkipped(): void {
  try {
    storage()?.removeItem(SKIPPED_KEY);
  } catch {
    // As above.
  }
}

/** Whether the strip is on screen. Idle shows nothing, ever; "Later" on a
 *  download offer is remembered per version, on a ready update for the run. */
export function stripVisible(status: UpdateStatus | null, skipped: string | null): boolean {
  if (!status || status.phase === "idle" || !status.version) return false;
  if (status.dismissed) return false;
  if (status.phase === "available" && skipped === status.version) return false;
  return true;
}

/** Download progress as a whole percentage, or `null` when the size is unknown
 *  — never a guessed number. */
export function downloadPercent(status: UpdateStatus): number | null {
  if (!status.total || status.total <= 0) return null;
  return Math.max(0, Math.min(100, Math.floor((status.done / status.total) * 100)));
}

/** The strip's sentence. */
export function stripText(status: UpdateStatus): string {
  const v = status.version ?? "";
  switch (status.phase) {
    case "ready":
      return `rmux ${v} is ready`;
    case "downloading": {
      const pct = downloadPercent(status);
      return pct === null ? `Downloading rmux ${v}…` : `Downloading rmux ${v}… ${pct}%`;
    }
    case "available":
      return `rmux ${v} is available`;
    default:
      return "";
  }
}

/** What "Check now" says once it has an answer. */
export function checkOutcome(status: UpdateStatus): string {
  switch (status.phase) {
    case "idle":
      return `You're on the latest version (${status.current})`;
    case "available":
      return status.installError
        ? `rmux ${status.version} is available — couldn't install it automatically (${status.installError}), use Download`
        : `rmux ${status.version} is available`;
    case "downloading":
      return `Downloading rmux ${status.version}…`;
    case "ready":
      return `rmux ${status.version} is ready — restart to update`;
  }
}
