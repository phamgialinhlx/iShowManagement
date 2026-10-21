/**
 * The one module that knows which shell the UI is running in.
 *
 * rmux's UI ships in two shells: the Tauri desktop app, and rmux-web — the same
 * bundle served over HTTP from a dev host (`docs/rmux-web.md`). Everything else in
 * `ui/src` talks to `invoke` / `Channel` / `listen` / `convertFileSrc` from *here*,
 * and **nothing else imports `@tauri-apps/api`** (`ui/bridge-imports-check.mjs`
 * fails the build if something does). That is what lets one call site work in
 * both shells with no `if (web)` at it — the same rule as `Target` on the Rust
 * side: the branch belongs in the seam, never in feature code.
 *
 * ## Desktop behaviour is unchanged, by construction
 *
 * Under Tauri every function here is a straight pass-through, and `new Channel()`
 * *returns Tauri's own `Channel` object* rather than a wrapper around it — so the
 * desktop app sends exactly the bytes it sent before this module existed, and
 * nothing about IPC ordering or channel cleanup is reimplemented.
 *
 * ## The web shell
 *
 * Commands are `POST /api/invoke/<cmd>`; every stream (terminal bytes, Claude
 * output, events) shares one WebSocket, `/api/ws`, multiplexed by channel id.
 * The full wire format is in `docs/rmux-web.md` › "Wire protocol (web)", and the
 * parsing half is the pure `bridge-frames.ts`, so it can be tested without a
 * browser.
 *
 * - **The socket opens lazily** — on the first `listen` or the first invoke that
 *   carries a `Channel` — and is then kept for the life of the page.
 * - **An invoke that carries a channel waits for `hello`**, because the server
 *   binds the channel to the connection named in `X-Rmux-Conn`; sending it before
 *   the page knows its own conn id would bind it to nothing.
 * - **A reconnect is a new conn, and the old channels are dead.** This module
 *   does *not* resurrect them: re-binding a stream is the pane's decision (it
 *   knows whether its shell still exists, and a blind re-send of an `open` would
 *   start a second shell). `onReconnect(cb)` is the hook a pane uses to reattach.
 *   A `Channel` object *may* be passed again after a reconnect — it is re-bound
 *   to the new conn when it is sent.
 */

import {
  Channel as TauriChannel,
  convertFileSrc as tauriConvertFileSrc,
  invoke as tauriInvoke,
} from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

import { CHANNEL_PREFIX, decodeFrame, takeTokenFromHash, type Frame } from "./bridge-frames";

/* ------------------------------------------------------------ shell detection */

/** True inside the Tauri desktop shell. */
export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const TOKEN_KEY = "rmux.web.token";

/**
 * The rmux-web token, taken once from `#token=…` and kept in `sessionStorage`.
 *
 * Stripped from the address bar immediately, so it is not bookmarked, not shared
 * by copying the URL and not left in the history entry. `sessionStorage` rather
 * than `localStorage`: a shell credential should die with the tab, and
 * `localStorage` is the quota that also holds the session list.
 */
const token: string | null = (() => {
  if (typeof window === "undefined" || isTauri()) return null;
  try {
    const { token: fromHash, rest } = takeTokenFromHash(window.location.hash);
    if (fromHash) {
      try {
        window.sessionStorage.setItem(TOKEN_KEY, fromHash);
      } catch {
        // Unwritable storage costs a reload its token, not this page its use.
      }
      const { pathname, search } = window.location;
      window.history.replaceState(window.history.state, "", `${pathname}${search}${rest}`);
      return fromHash;
    }
    return window.sessionStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
})();

/**
 * True when the page is served by rmux-web.
 *
 * Told apart from a plain browser tab (design work on `vite`, the check
 * harnesses) by having a token, or — once Access JWT verification replaces the
 * token — by the server marking the page with `<meta name="rmux-shell"
 * content="web">`. A plain tab must stay inert: it has no server to call, and
 * pointing it at `/api/invoke` would fail every widget with a 404.
 */
export const isWeb = (): boolean => {
  if (typeof window === "undefined" || isTauri()) return false;
  if (token) return true;
  return document.querySelector('meta[name="rmux-shell"][content="web"]') !== null;
};

/** True when there is any backend to call — desktop or web. This is the gate for
 *  a feature that works in both; `isTauri()` is for the desktop-only ones. */
export const hasBackend = (): boolean => isTauri() || isWeb();

/* ---------------------------------------------------------------- types */

/** Matches Tauri's `Event<T>`, which is the shape every handler here reads. */
export type BridgeEvent<T> = { event: string; id: number; payload: T };
export type UnlistenFn = () => void;

/** What a failed invoke throws — the shape `toApiError` in `api.ts` reads. */
export type InvokeFailure = { message: string; requiresSignin?: boolean };

/* ---------------------------------------------------------------- channels */

let nextChannelId = 1;
/** Live web channels by id. A channel is (re)registered each time it is sent. */
const channels = new Map<number, Channel<unknown>>();

/**
 * A stream from a command back to the page.
 *
 * Under Tauri the constructor returns Tauri's own `Channel` — not a subclass, not
 * a wrapper — so desktop IPC is untouched. On the web it is an id in a map,
 * serialised as `"__CHANNEL__:<id>"`, the very string Tauri's `toJSON` produces.
 */
export class Channel<T = unknown> {
  readonly id!: number;
  /** The conn this channel was last sent on; frames for it stop at a reconnect. */
  conn: string | null = null;
  onmessage!: (message: T) => void;

  constructor(onmessage?: (message: T) => void) {
    if (isTauri()) {
      // A constructor may return a different object; this is how the desktop
      // shell gets the real thing with the call sites unchanged.
      return new TauriChannel<T>(onmessage) as unknown as Channel<T>;
    }
    this.id = nextChannelId++;
    this.onmessage = onmessage ?? (() => {});
  }

  toJSON(): string {
    return `${CHANNEL_PREFIX}${this.id}`;
  }
}

/** Every web `Channel` reachable from an invoke's args. */
function channelsIn(value: unknown, found: Channel<unknown>[] = [], seen = new Set<object>()): Channel<unknown>[] {
  if (typeof value !== "object" || value === null || seen.has(value)) return found;
  seen.add(value);
  if (value instanceof Channel) {
    found.push(value);
    return found;
  }
  for (const v of Array.isArray(value) ? value : Object.values(value)) channelsIn(v, found, seen);
  return found;
}

/* ---------------------------------------------------------------- socket */

const BACKOFF_MIN_MS = 500;
const BACKOFF_MAX_MS = 8_000;
/** How long an invoke carrying a channel waits for `hello` before failing. A
 *  pane that waits forever is indistinguishable from a broken one. */
const HELLO_TIMEOUT_MS = 15_000;

let socket: WebSocket | null = null;
let conn: string | null = null;
let attempt = 0;
let everConnected = false;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
const helloWaiters = new Set<(conn: string) => void>();
const reconnectHooks = new Set<(conn: string) => void>();
const eventHandlers = new Map<string, Set<(event: BridgeEvent<unknown>) => void>>();
let nextEventId = 1;

function wsUrl(): string {
  const url = new URL("/api/ws", window.location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  // A browser cannot set headers on a WebSocket upgrade, so the token rides in
  // the query here — and only here. See the wire protocol's Auth note.
  if (token) url.searchParams.set("token", token);
  return url.toString();
}

function ensureSocket(): void {
  if (socket || reconnectTimer || !isWeb()) return;
  openSocket();
}

function openSocket(): void {
  reconnectTimer = null;
  let ws: WebSocket;
  try {
    ws = new WebSocket(wsUrl());
  } catch {
    scheduleReconnect();
    return;
  }
  ws.binaryType = "arraybuffer";
  socket = ws;
  ws.onmessage = (e: MessageEvent) => dispatch(decodeFrame(e.data as string | ArrayBuffer));
  // `close` always follows `error`, so all recovery lives there.
  ws.onclose = () => {
    if (socket !== ws) return;
    socket = null;
    const old = conn;
    conn = null;
    if (old) {
      // The server dropped every channel bound to that conn when it went away.
      for (const [id, ch] of channels) if (ch.conn === old) channels.delete(id);
    }
    scheduleReconnect();
  };
}

function scheduleReconnect(): void {
  const delay = Math.min(BACKOFF_MIN_MS * 2 ** attempt, BACKOFF_MAX_MS);
  attempt += 1;
  reconnectTimer = setTimeout(openSocket, delay);
}

function dispatch(frame: Frame): void {
  switch (frame.kind) {
    case "hello": {
      const isReconnect = everConnected;
      conn = frame.conn;
      everConnected = true;
      attempt = 0;
      for (const resolve of helloWaiters) resolve(frame.conn);
      helloWaiters.clear();
      if (isReconnect) {
        for (const cb of reconnectHooks) {
          try {
            cb(frame.conn);
          } catch (e) {
            console.error("rmux bridge: onReconnect handler threw", e);
          }
        }
      }
      return;
    }
    case "bytes":
      channels.get(frame.channel)?.onmessage(frame.bytes);
      return;
    case "json":
      channels.get(frame.channel)?.onmessage(frame.data);
      return;
    case "end":
      channels.delete(frame.channel);
      return;
    case "event": {
      const handlers = eventHandlers.get(frame.event);
      if (!handlers) return;
      const event: BridgeEvent<unknown> = { event: frame.event, id: nextEventId++, payload: frame.payload };
      for (const h of handlers) h(event);
      return;
    }
    case "invalid":
      console.warn(`rmux bridge: dropped a frame — ${frame.reason}`);
      return;
  }
}

/** Resolve with the current conn id, opening the socket and waiting for `hello`
 *  if there is none yet. */
function connected(): Promise<string> {
  if (conn) return Promise.resolve(conn);
  ensureSocket();
  return new Promise((resolve, reject) => {
    const done = (c: string) => {
      clearTimeout(timer);
      resolve(c);
    };
    const timer = setTimeout(() => {
      helloWaiters.delete(done);
      reject({
        message: "rmux-web did not answer — the connection to the server is down or still reconnecting.",
      } satisfies InvokeFailure);
    }, HELLO_TIMEOUT_MS);
    helloWaiters.add(done);
  });
}

/**
 * Run `cb` with the new conn id every time the web socket comes back after
 * having been lost. Channels sent on the old conn are dead by then; a pane uses
 * this to re-issue its attach. Never fires on the desktop, and never for the
 * first connection. Returns an unsubscribe.
 */
export function onReconnect(cb: (conn: string) => void): () => void {
  reconnectHooks.add(cb);
  return () => reconnectHooks.delete(cb);
}

/* ---------------------------------------------------------------- invoke */

async function webInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const carried = channelsIn(args);
  const id = carried.length ? await connected() : conn;
  for (const ch of carried) {
    ch.conn = id;
    channels.set(ch.id, ch);
  }

  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  if (id) headers["X-Rmux-Conn"] = id;

  let response: Response;
  try {
    response = await fetch(`/api/invoke/${encodeURIComponent(cmd)}`, {
      method: "POST",
      headers,
      body: JSON.stringify(args ?? {}),
    });
  } catch (e) {
    throw { message: `rmux-web is unreachable: ${e instanceof Error ? e.message : String(e)}` } satisfies InvokeFailure;
  }

  const text = await response.text();
  if (!response.ok) {
    let failure: InvokeFailure | null = null;
    try {
      const body = JSON.parse(text) as unknown;
      if (typeof body === "object" && body !== null && typeof (body as InvokeFailure).message === "string") {
        const b = body as InvokeFailure;
        failure = { message: b.message, requiresSignin: b.requiresSignin === true };
      }
    } catch {
      // Not JSON — a proxy's error page, most likely. Reported by status below.
    }
    throw failure ?? ({ message: `${cmd}: HTTP ${response.status}${text ? ` — ${text.slice(0, 200)}` : ""}` } satisfies InvokeFailure);
  }
  // A command returning `()` may answer with an empty body rather than `null`.
  return (text ? JSON.parse(text) : null) as T;
}

/** Call a backend command. Desktop: Tauri IPC, unchanged. Web: `POST /api/invoke`. */
export function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri()) return tauriInvoke<T>(cmd, args);
  if (isWeb()) return webInvoke<T>(cmd, args);
  return Promise.reject({
    message: `Not running in the rmux desktop shell — "${cmd}" is unavailable. Use \`pnpm tauri dev\`.`,
  } satisfies InvokeFailure);
}

/* ---------------------------------------------------------------- events */

/** Subscribe to a backend event. The handler receives `{ event, id, payload }`. */
export function listen<T>(event: string, handler: (event: BridgeEvent<T>) => void): Promise<UnlistenFn> {
  if (isTauri()) return tauriListen<T>(event, handler);
  let set = eventHandlers.get(event);
  if (!set) eventHandlers.set(event, (set = new Set()));
  const h = handler as (event: BridgeEvent<unknown>) => void;
  set.add(h);
  ensureSocket();
  return Promise.resolve(() => {
    const s = eventHandlers.get(event);
    s?.delete(h);
    if (s && !s.size) eventHandlers.delete(event);
  });
}

/* ---------------------------------------------------------------- files */

/**
 * A URL the page can load a local-to-the-backend file from (a background
 * picture). Desktop: Tauri's asset protocol. Web: `GET /api/file?path=…`, with
 * the token in the query because an `<img>` cannot send a header.
 */
export function convertFileSrc(path: string): string {
  if (isTauri()) return tauriConvertFileSrc(path);
  const params = new URLSearchParams({ path });
  if (token) params.set("token", token);
  return `/api/file?${params.toString()}`;
}
