/**
 * The rmux-web frame decoder and the `#token=` parser (`src/lib/bridge-frames.ts`).
 *
 *   node --experimental-strip-types ui/bridge-frames-check.ts
 *
 * Runs in plain Node — the decoder has no imports for exactly this reason. The
 * layout pinned here is the wire protocol in `docs/rmux-web.md`; a server and a
 * page that disagree about byte order or frame shape produce a terminal that
 * shows garbage or nothing, with no error anywhere.
 */
import { decodeFrame, encodeBinaryFrame, takeTokenFromHash } from "./src/lib/bridge-frames.ts";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) console.log(`PASS  ${what}`);
  else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

const bytesOf = (b: ArrayBuffer) => Array.from(new Uint8Array(b));

// --- binary: 4-byte big-endian id + raw bytes --------------------------------
{
  // Id 0x01020304 big-endian, payload includes a NUL and invalid UTF-8.
  const raw = new Uint8Array([0x01, 0x02, 0x03, 0x04, 0x68, 0x00, 0xff, 0x0a]);
  const f = decodeFrame(raw.buffer);
  check("binary frame decodes as bytes", f.kind === "bytes");
  if (f.kind === "bytes") {
    check("channel id is read big-endian (0x01020304 = 16909060)", f.channel === 16909060);
    check("payload excludes the 4 id bytes, NUL and 0xff intact", JSON.stringify(bytesOf(f.bytes)) === "[104,0,255,10]");
    check("payload is its own ArrayBuffer, not a view onto the frame", f.bytes.byteLength === 4);
  }
}
{
  const payload = new TextEncoder().encode("\x1b[31mred\x1b[0m");
  const f = decodeFrame(encodeBinaryFrame(7, payload));
  check(
    "encode → decode round-trips id and bytes",
    f.kind === "bytes" && f.channel === 7 && new TextDecoder().decode(f.bytes) === "\x1b[31mred\x1b[0m",
  );
}
{
  const f = decodeFrame(encodeBinaryFrame(0xffffffff, new Uint8Array()));
  check("max u32 id and an empty payload decode", f.kind === "bytes" && f.channel === 0xffffffff && f.bytes.byteLength === 0);
}
{
  // A view with a non-zero offset (what some WebSocket shims hand over).
  const backing = new Uint8Array([9, 9, 0, 0, 0, 5, 0x41]);
  const f = decodeFrame(backing.subarray(2));
  check("a Uint8Array view at an offset decodes", f.kind === "bytes" && f.channel === 5 && bytesOf(f.bytes).join() === "65");
}
check("a binary frame shorter than the id is invalid, not thrown", decodeFrame(new Uint8Array([1, 2]).buffer).kind === "invalid");

// --- text: channel JSON ------------------------------------------------------
{
  const f = decodeFrame(JSON.stringify({ channel: 3, data: { type: "exited", code: 255 } }));
  check(
    "text channel frame delivers its JSON",
    f.kind === "json" && f.channel === 3 && JSON.stringify(f.data) === '{"type":"exited","code":255}',
  );
}
check("channel data of null is still data", (() => {
  const f = decodeFrame('{"channel":1,"data":null}');
  return f.kind === "json" && f.data === null;
})());
check("channel end frame", (() => {
  const f = decodeFrame('{"channel":4,"end":true}');
  return f.kind === "end" && f.channel === 4;
})());
check("channel frame without data is invalid", decodeFrame('{"channel":4}').kind === "invalid");
check("a non-integer channel id is invalid", decodeFrame('{"channel":"4","data":1}').kind === "invalid");

// --- text: events and hello ---------------------------------------------------
{
  const f = decodeFrame(JSON.stringify({ event: "ssh://prompt", payload: { id: "p1", prompt: "Password:" } }));
  check(
    "event frame delivers name and payload",
    f.kind === "event" && f.event === "ssh://prompt" && JSON.stringify(f.payload) === '{"id":"p1","prompt":"Password:"}',
  );
}
check("hello carries the conn id", (() => {
  const f = decodeFrame('{"hello":{"conn":"c-42"}}');
  return f.kind === "hello" && f.conn === "c-42";
})());
check("hello without a conn is invalid", decodeFrame('{"hello":{}}').kind === "invalid");
check("non-JSON text is invalid, not thrown", decodeFrame("not json").kind === "invalid");
check("a JSON array is invalid", decodeFrame("[1,2]").kind === "invalid");
check("an unknown shape is invalid", decodeFrame('{"what":1}').kind === "invalid");

// --- #token= ------------------------------------------------------------------
{
  const t = takeTokenFromHash("#token=abc123");
  check("token alone: taken, hash emptied", t.token === "abc123" && t.rest === "");
}
{
  const t = takeTokenFromHash("#view=grid&token=s3cr%2Bt&x=1");
  check("token among others: taken and decoded, the rest kept", t.token === "s3cr+t" && t.rest === "#view=grid&x=1");
}
{
  const t = takeTokenFromHash("#view=grid");
  check("no token: hash untouched", t.token === null && t.rest === "#view=grid");
}
check("empty hash: nothing", (() => {
  const t = takeTokenFromHash("");
  return t.token === null && t.rest === "";
})());
check("empty token value is no token", takeTokenFromHash("#token=").token === null);

if (failures) {
  console.error(`\n${failures} FAILED`);
  (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
} else {
  console.log("\nall passed");
}
