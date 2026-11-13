/**
 * Mirror the operator's server list to `~/.rmux/servers.json`, so the `rmux`
 * command line — run from any terminal, with no app open — knows which machines
 * `rmux ls` should ask.
 *
 * The list lives in this webview's `localStorage` and nowhere else, so a CLI
 * cannot read it directly. Rust writes the file (`servers_sync`: `0600`,
 * temp-then-rename); this only decides *when*, by watching the store rather than
 * hooking every reducer that can add or drop a server — one subscriber cannot be
 * forgotten by the next reducer someone writes.
 *
 * Only the ssh destination crosses: id, a label, host, user, port — exactly as
 * the app stores them, never resolved against `~/.ssh/config`, and no secrets —
 * plus the *path* of a key chosen for the server (never the key itself), which
 * the CLI offers with `-i` exactly as the app does.
 *
 * Rust also treats this list as the authority on chosen keys: `servers_sync`
 * replaces its identity table from it, which is how switching a server back to
 * DEFAULT stops its key being offered.
 */
import { invoke, isTauri } from "./bridge";
import { useWorkspace } from "./workspace";
import type { Server } from "./workspace-model";

type ServerEntry = { id: string; label: string; host: string; user?: string; port?: number; identity?: string };

const entries = (servers: Server[]): ServerEntry[] =>
  servers
    .filter((s) => !!s.target.host)
    .map((s) => {
      const host = s.target.host!;
      const user = s.target.user || undefined;
      const port = s.target.port || undefined;
      const identity = s.target.identity || undefined;
      const label = `${user ? `${user}@` : ""}${host}${port ? `:${port}` : ""}`;
      return { id: s.id, label, host, user, port, identity };
    });

let started = false;

/** Start mirroring. Workbench window only; idempotent. */
export function startServersSync(): void {
  if (started || !isTauri()) return;
  started = true;

  let last = "";
  const push = (servers: Server[]) => {
    const list = entries(servers);
    const key = JSON.stringify(list);
    // The store changes on every status tick; the server list almost never does.
    if (key === last) return;
    last = key;
    invoke<void>("servers_sync", { servers: list }).catch((e) => {
      // Not fatal to anything the app does — only `rmux ls` reads the file — so
      // log and let the next change retry.
      last = "";
      console.warn("servers_sync failed", e);
    });
  };

  push(useWorkspace.getState().servers);
  useWorkspace.subscribe((state) => push(state.servers));
}
