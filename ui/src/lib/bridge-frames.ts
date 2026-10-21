/**
 * The pure half of `bridge.ts`: parsing what arrives on the rmux-web WebSocket
 * and the `#token=` in the address bar.
 *
 * It lives in its own module, with **no imports at all**, so it can be run by
 * plain Node (`ui/bridge-frames-check.ts`) without a browser, a bundler or the
 * Tauri API. Anything here that needed `window` would stop being testable that
 * way, which is the whole reason it was split out.
 *
 * The wire format is specified in `docs/rmux-web.md` › "Wire protocol (web)".
 */

/** One decoded WebSocket frame. `invalid` is a frame the page cannot use — it is
 *  reported rather than thrown, because one malformed frame must not take the
 *  socket (and every terminal on it) down with it. */
export type Frame =
  | { kind: "hello"; conn: string }
  | { kind: "bytes"; channel: number; bytes: ArrayBuffer }
  | { kind: "json"; channel: number; data: unknown }
  | { kind: "end"; channel: number }
  | { kind: "event"; event: string; payload: unknown }
  | { kind: "invalid"; reason: string };

/** The prefix Tauri's own `Channel` serialises to (`core.js`, `toJSON`). The web
 *  shell uses the same shape so a Rust handler sees one format from both. */
export const CHANNEL_PREFIX = "__CHANNEL__:";

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

const isChannelId = (v: unknown): v is number =>
  typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= 0xffff_ffff;

/**
 * Decode one frame.
 *
 * - Binary: a 4-byte **big-endian** channel id, then the raw payload, delivered
 *   as an `ArrayBuffer` exactly as a Tauri `Channel<ArrayBuffer>` delivers it.
 *   The payload is *copied* into its own buffer: a view onto the frame would
 *   hand xterm a buffer whose first four bytes are the id.
 * - Text: JSON — `{hello:{conn}}`, `{channel, data}`, `{channel, end:true}` or
 *   `{event, payload}`.
 */
export function decodeFrame(data: string | ArrayBuffer | ArrayBufferView): Frame {
  if (typeof data !== "string") {
    const view =
      data instanceof ArrayBuffer
        ? new Uint8Array(data)
        : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    if (view.byteLength < 4) {
      return { kind: "invalid", reason: `binary frame of ${view.byteLength} bytes has no channel id` };
    }
    const channel = new DataView(view.buffer, view.byteOffset, 4).getUint32(0, false);
    const bytes = view.slice(4).buffer as ArrayBuffer;
    return { kind: "bytes", channel, bytes };
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    return { kind: "invalid", reason: "text frame is not JSON" };
  }
  if (!isRecord(parsed)) return { kind: "invalid", reason: "text frame is not an object" };

  if ("hello" in parsed) {
    const hello = parsed.hello;
    if (isRecord(hello) && typeof hello.conn === "string" && hello.conn) {
      return { kind: "hello", conn: hello.conn };
    }
    return { kind: "invalid", reason: "hello frame without a conn id" };
  }

  if ("channel" in parsed) {
    if (!isChannelId(parsed.channel)) return { kind: "invalid", reason: "channel id is not a u32" };
    if (parsed.end === true) return { kind: "end", channel: parsed.channel };
    if (!("data" in parsed)) return { kind: "invalid", reason: "channel frame without data" };
    return { kind: "json", channel: parsed.channel, data: parsed.data };
  }

  if ("event" in parsed) {
    if (typeof parsed.event !== "string") return { kind: "invalid", reason: "event name is not a string" };
    return { kind: "event", event: parsed.event, payload: parsed.payload };
  }

  return { kind: "invalid", reason: "text frame of an unknown shape" };
}

/** Build a binary frame — the inverse of `decodeFrame`'s binary branch. Used by
 *  the check harness, and documents the layout in executable form. */
export function encodeBinaryFrame(channel: number, payload: Uint8Array): ArrayBuffer {
  const out = new Uint8Array(4 + payload.byteLength);
  new DataView(out.buffer).setUint32(0, channel, false);
  out.set(payload, 4);
  return out.buffer;
}

/**
 * Pull `token=` out of a `location.hash`, returning the token and the hash to
 * put back without it. Other fragment parameters survive — the hash is the
 * page's, and the token is only a guest in it.
 *
 * The token travels in the fragment, not the query, because a fragment is never
 * sent to a server: it cannot land in an access log or a proxy's on the way in.
 */
export function takeTokenFromHash(hash: string): { token: string | null; rest: string } {
  const body = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!body) return { token: null, rest: "" };
  const params = new URLSearchParams(body);
  const token = params.get("token");
  if (token === null) return { token: null, rest: `#${body}` };
  params.delete("token");
  const left = params.toString();
  return { token: token || null, rest: left ? `#${left}` : "" };
}
