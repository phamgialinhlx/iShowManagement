import { api, isTauri } from "./api";

/**
 * Benchmark logging — the app observing its own poll behaviour.
 *
 * External OS sampling cannot measure the polling-reduction work: a warm-master
 * metrics poll is a sub-100ms channel too brief to catch, and the wakeup rate is
 * swamped by live terminal streaming. So the app logs structured `BENCH` lines
 * about its own poller/SSH activity, which the existing Diagnostics export
 * carries out. Off by default; the Diagnostics switch flips it live.
 *
 * The flag is cached in a module bool so hot poll paths do not touch
 * `localStorage` on every event, and it is kept in step across windows by the
 * `storage` listener (the Settings window writes; the workbench reads).
 */

const KEY = "rmux.debugLogging";

function readStored(): boolean {
  try {
    return localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

let enabled = readStored();

/** Whether benchmark logging is on. Cached — cheap to call per poll event. */
export function debugLogging(): boolean {
  return enabled;
}

/** Set the preference, update the cache, and sync the Rust-side gate. */
export function setDebugLoggingEnabled(on: boolean): void {
  enabled = on;
  try {
    localStorage.setItem(KEY, on ? "1" : "0");
  } catch {
    // A benchmark toggle that cannot persist is a minor loss.
  }
  if (isTauri()) void api.setDebugLogging(on);
}

/** Emit one benchmark line into the exportable log — a no-op unless on. */
export function bench(msg: string): void {
  if (!enabled || !isTauri()) return;
  void api.logEvent(msg);
}

/** Push the stored preference to Rust at startup. Call once on app load. */
export function syncDebugLogging(): void {
  if (isTauri()) void api.setDebugLogging(enabled);
}

// Cross-window: the Settings window writes the key; the workbench must track it
// so its own poll sites start and stop logging in step.
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key === KEY) enabled = readStored();
  });
}
