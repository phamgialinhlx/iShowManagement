/**
 * The pure half of "Connect a server": parsing what was typed into a target,
 * and turning a key-file inspection into what the dialog says about it.
 *
 * Kept free of React and of the store so `ui/connect-check.ts` can pin it in
 * Node — every rule here is one whose failure is silent in the app (a port that
 * quietly became `NaN`, a key choice that re-keyed the server).
 */
import type { IdentityCheck, TargetRef } from "./api.ts";

/** Which key a server authenticates with. */
export type KeyChoice = "default" | "file" | "generate";

export type ConnectForm = {
  host: string;
  user: string;
  port: string;
  key: KeyChoice;
  /** Absolute path, when `key` is `"file"`. */
  keyPath: string;
};

/** An empty port is "ssh's own default" (or `~/.ssh/config`'s), not an error. */
export function parsePort(raw: string): { port?: number; error?: string } {
  const text = raw.trim();
  if (!text) return {};
  if (!/^\d+$/.test(text)) return { error: "port must be a number" };
  const port = Number(text);
  if (port < 1 || port > 65535) return { error: "port must be 1–65535" };
  return { port };
}

/**
 * The target the form describes, or the reason it describes none.
 *
 * The host goes **verbatim** — it may be a `~/.ssh/config` alias, and ssh
 * resolves it with `ProxyJump`, `Match` and the rest. Nothing a config host's
 * hints showed (HostName/User/Port) is copied in: a field the operator left
 * empty stays empty, and ssh applies the config itself.
 */
export function formTarget(form: ConnectForm): { target?: TargetRef; error?: string } {
  const host = form.host.trim();
  if (!host) return { error: "enter a host" };
  if (/\s/.test(host)) return { error: "a host cannot contain spaces" };
  const user = form.user.trim();
  if (/[\s@]/.test(user)) return { error: "user cannot contain spaces or @" };
  const { port, error } = parsePort(form.port);
  if (error) return { error };
  const target: TargetRef = { host };
  if (user) target.user = user;
  if (port) target.port = port;
  if (form.key === "file") {
    const path = form.keyPath.trim();
    if (!path) return { error: "choose a key file" };
    target.identity = path;
  }
  return { target };
}

/** What to say about a picked key file. `blocking` stops CONNECT; a warning does not. */
export type KeyNote = { text: string; blocking: boolean; fix?: { label: string; path: string } };

export function keyFileNotes(check: IdentityCheck | null): KeyNote[] {
  if (!check) return [];
  if (!check.exists) return [{ text: "that file does not exist", blocking: true }];
  if (!check.readable) return [{ text: "rmux cannot read that file — ssh will not be able to either", blocking: true }];
  const notes: KeyNote[] = [];
  if (check.isPublic) {
    notes.push({
      text: "this is the public half (.pub) — ssh needs the private key",
      blocking: true,
      fix: check.privateSibling ? { label: "USE THE PRIVATE KEY", path: check.privateSibling } : undefined,
    });
  }
  if (check.tooOpen && check.mode != null) {
    notes.push({
      text: `mode ${check.mode.toString(8).padStart(4, "0")} is readable by others — ssh will refuse it. Run chmod 600 on it.`,
      blocking: false,
    });
  }
  return notes;
}

/** The server's label as the rail shows it. */
export function targetLabel(t: TargetRef): string {
  return t.host ? `${t.user ? `${t.user}@` : ""}${t.host}${t.port ? `:${t.port}` : ""}` : "this machine";
}
