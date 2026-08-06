import { listen } from "@tauri-apps/api/event";

import { api, isTauri } from "./api";
import { useWorkspace } from "./workspace";

/**
 * Keeping rmux's backend face in step with the workbench.
 *
 * Sessions live in the webview — created by the operator, persisted here,
 * ordered by the rail — so Rust holds a mirror rather than the original. This
 * pushes that mirror down whenever it changes, and listens for the two things a
 * client can send back: a request to switch sessions, and a report from a
 * browser.
 *
 * **Only the four fields a client has any business seeing are sent.** The store
 * also holds which file is open, which terminal is focused, whether Claude is
 * running fullscreen — UI state that no other app should be able to depend on,
 * because every one of those is free to change without warning.
 */

/** The client-facing view of a session: id, name, and — resolved from its
 *  Project/Server — the host and folder. Host is omitted rather than "localhost"
 *  for a local session: a client might try to ssh to whatever this says. */
const shape = (id: string, name: string, host: string | undefined, folder: string) => ({
  id,
  name,
  ...(host ? { host } : {}),
  folder,
});

/** What a browser told us about a page. Mirrors `rmux_control::Report`. */
export type Report =
  | {
      report: "selection";
      url: string;
      selector: string;
      text?: string;
      note?: string;
      /** Base64 PNG, with no data-URI prefix. */
      screenshot?: string;
      viewport?: Viewport;
    }
  | { report: "screenshot"; url: string; png: string; viewport?: Viewport }
  | { report: "console"; url: string; entries: ConsoleEntry[] }
  | { report: "har"; url: string; har: string }
  | { report: "viewport"; url: string; viewport: Viewport };

export type Viewport = { width: number; height: number; devicePixelRatio?: number };
export type ConsoleEntry = { level: string; text: string; at?: number };

export type IncomingReport = Report & { session: string };

type Listener = (report: IncomingReport) => void;

const listeners = new Set<Listener>();

/** Subscribe to reports. Returns an unsubscribe. */
export function onReport(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

let started = false;

/**
 * Start mirroring, once per run.
 *
 * Idempotent because React's development double-mount would otherwise install
 * two subscriptions and deliver every report twice.
 */
export function startControlBridge(): void {
  if (started || !isTauri()) return;
  started = true;

  const push = (state: ReturnType<typeof useWorkspace.getState>) => {
    const mirror = state.sessions.map((s) =>
      shape(s.id, s.name, state.serverOf(s.id)?.target.host, state.projectOf(s.id)?.folder ?? ""),
    );
    void api.controlSync(mirror, state.activeSession).catch(() => {
      // The socket is optional — see `control.rs`. A client that cannot attach
      // is not a reason to surface anything in the workbench.
    });
  };

  push(useWorkspace.getState());
  useWorkspace.subscribe((state, previous) => {
    // Cheap guard: this fires on every keystroke in an editor, and re-sending
    // an unchanged list would be an IPC call per character. Projects/servers are
    // watched too, because host and folder are resolved from them.
    if (
      state.sessions === previous.sessions &&
      state.activeSession === previous.activeSession &&
      state.projects === previous.projects &&
      state.servers === previous.servers
    ) {
      return;
    }
    push(state);
  });

  void listen<string>("rmux://activate", (event) => {
    // A client asking rmux to switch. Honoured only for a session that exists —
    // activating an unknown id would blank the workbench.
    const id = event.payload;
    if (useWorkspace.getState().sessions.some((s) => s.id === id)) {
      useWorkspace.getState().activate(id);
    }
  });

  void listen<IncomingReport>("rmux://report", (event) => {
    for (const fn of listeners) fn(event.payload);
  });
}

/**
 * Turn a browser's selection into something worth sending to Claude.
 *
 * Written here rather than at the call site so the shape is consistent, and so
 * the framing is deliberate: the note is presented as **the operator's**
 * message, with the page's own strings quoted as context. Everything except the
 * note came from a page rmux did not write, and a selector or an element's text
 * is free to contain anything at all — including a line that reads like an
 * instruction. Quoting keeps that as evidence rather than letting it sit in the
 * prompt as though the operator had typed it.
 */
export function selectionPrompt(report: Extract<Report, { report: "selection" }>): string {
  const lines = [report.note?.trim() || "Have a look at this element."];
  lines.push("");
  lines.push(`Page: ${report.url}`);
  lines.push(`Element: ${report.selector}`);
  if (report.text) lines.push(`Its text: ${JSON.stringify(report.text)}`);
  if (report.viewport) {
    const { width, height, devicePixelRatio } = report.viewport;
    lines.push(`Viewport: ${width}x${height}${devicePixelRatio ? ` @${devicePixelRatio}x` : ""}`);
  }
  return lines.join("\n");
}
