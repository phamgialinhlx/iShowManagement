import { api, isTauri, type BrowserInfo } from "./api";

/**
 * The proxied-browser pieces shared by the rail's `www` verb and the Host
 * pane's PORTS section: one detection per app run, and one remembered choice —
 * two pickers disagreeing about which browser "the" browser is would make the
 * verb and the panel open different apps.
 */

let cached: Promise<BrowserInfo[]> | null = null;

/** Chromium-family browsers on this machine. Detected once per run — a detect
 *  failure yields [] so every caller's "hide the control" path just works. */
export function detectBrowsers(): Promise<BrowserInfo[]> {
  if (!isTauri()) return Promise.resolve([]);
  cached ??= api.browsersDetect().catch(() => []);
  return cached;
}

const KEY = "rmux.browser";

/** The remembered choice if it is still installed, else the first detected. */
export function chosenBrowser(list: BrowserInfo[]): BrowserInfo | null {
  const saved = localStorage.getItem(KEY);
  return list.find((b) => b.bin === saved) ?? list[0] ?? null;
}

export function rememberBrowser(bin: string) {
  localStorage.setItem(KEY, bin);
}
