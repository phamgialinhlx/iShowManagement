import { api, isTauri } from "./api";

/**
 * Whether rmux may install `mosh-server` on a host that has not got one — or
 * replace one that is too old to be usable for what rmux does with it.
 *
 * **Default on**, because that is what was asked for: the low-latency transport
 * is refused on a host with no `mosh-server`, and that refusal is one package
 * away on a machine rmux is already connected to.
 *
 * It still gets a switch, because this mutates machines that are not all the
 * operator's — a shared build box, a client's server, something managed by
 * somebody else's configuration tool. Off means off: Rust attempts no
 * detection, runs no `sudo`, and writes nothing to `~/.rmux/mosh-install.json`.
 *
 * The gate itself lives in Rust, because the attempt is a background task the
 * webview never sees. This module is the stored preference and the mirror —
 * the same shape as the benchmark-logging switch, deliberately, rather than a
 * second mechanism for the same job.
 */

// The key is unchanged, and deliberately: the setting has always meant "rmux may
// change this host's mosh", and an upgrade is squarely that. A second key would
// have silently defaulted every machine that had already opted *out* back into
// being changed.
const KEY = "rmux.mosh.autoInstall";

/** Absent means **on**. A first run must get the behaviour that was asked for,
 *  and only an explicit "0" turns it off. */
function readStored(): boolean {
  try {
    return localStorage.getItem(KEY) !== "0";
  } catch {
    return true;
  }
}

let enabled = readStored();

export function moshAutoInstall(): boolean {
  return enabled;
}

/** Set the preference, update the cache, and sync the Rust-side gate. */
export function setMoshAutoInstall(on: boolean): void {
  enabled = on;
  try {
    localStorage.setItem(KEY, on ? "1" : "0");
  } catch {
    // Unwritable storage costs the preference its persistence, not its effect:
    // Rust is told either way, so this run behaves as asked.
  }
  if (isTauri()) void api.setMoshAutoInstall(on);
}

/** Push the stored preference to Rust at startup. Call once on app load. */
export function syncMoshAutoInstall(): void {
  if (isTauri()) void api.setMoshAutoInstall(enabled);
}

// Cross-window: the Settings window writes the key, the workbench holds the
// connections. Without this the switch would appear to do nothing until the
// next launch, because the window that talks to hosts never re-read it.
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key !== KEY) return;
    enabled = readStored();
    if (isTauri()) void api.setMoshAutoInstall(enabled);
  });
}
