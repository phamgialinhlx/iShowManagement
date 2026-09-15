/**
 * Does the terminal frame-rate throttle preserve every byte, in order, and
 * flush what it is still holding when the pane goes away?
 *
 * Run at http://localhost:5273/terminal-fps-check.html and read the console.
 * The invariant worth a harness is **no bytes lost**: `coalesceOutput` buffers
 * PTY output and hands it to xterm in batches, so a dropped or reordered buffer
 * would corrupt a terminal silently — the worst kind of failure here. This
 * drives the *real* `coalesceOutput` with real `requestAnimationFrame`, the same
 * clock the app uses, rather than a stubbed timer that would prove nothing about
 * the rAF gating.
 */
import { coalesceOutput } from "./src/lib/terminal-fps";

let failures = 0;
function check(name: string, ok: boolean, detail: string) {
  if (ok) console.log(`%c PASS %c ${name} — ${detail}`, "background:#2b7;color:#000", "");
  else {
    failures++;
    console.error(`FAIL  ${name} — ${detail}`);
  }
}

const enc = new TextEncoder();
const dec = new TextDecoder();
const buf = (s: string): ArrayBuffer => enc.encode(s).buffer as ArrayBuffer;
const raf = () => new Promise<void>((r) => requestAnimationFrame(() => r()));
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

async function run() {
  // 1. Capped: many chunks coalesce into fewer batches, losing nothing, in order.
  //    Chunks are split at character boundaries, so any batch boundary is too —
  //    a multi-byte glyph is never cut, exactly as the real byte stream behaves
  //    at chunk granularity.
  {
    const fps = 30;
    const seen: string[] = [];
    const sink = coalesceOutput((bytes) => seen.push(dec.decode(bytes)), () => fps);
    const inputs = ["Ti", "ế", "ng ", "Việt", " 🌱", "\r\n"];
    for (const s of inputs) sink.write(buf(s));
    check("capped: nothing painted synchronously", seen.length === 0, `batches so far: ${seen.length}`);
    for (let i = 0; i < 6; i++) await raf();
    await sleep(50);
    for (let i = 0; i < 4; i++) await raf();
    const joined = seen.join("");
    check("capped: no bytes lost, in order", joined === inputs.join(""), JSON.stringify(joined));
    check(
      "capped: coalesced (fewer batches than chunks)",
      seen.length > 0 && seen.length < inputs.length,
      `${seen.length} batches for ${inputs.length} chunks`,
    );
    sink.dispose();
  }

  // 2. Flush on dispose: bytes written then immediately disposed still arrive.
  {
    const seen: string[] = [];
    const sink = coalesceOutput((bytes) => seen.push(dec.decode(bytes)), () => 15);
    sink.write(buf("tail-before-close"));
    check("dispose: held until flush", seen.length === 0, "buffered");
    sink.dispose();
    check("dispose: flushed synchronously", seen.join("") === "tail-before-close", JSON.stringify(seen));
  }

  // 3. OFF (fps 0): straight through, one batch per chunk, in order, no rAF wait.
  {
    const seen: string[] = [];
    const sink = coalesceOutput((bytes) => seen.push(dec.decode(bytes)), () => 0);
    sink.write(buf("a"));
    sink.write(buf("b"));
    sink.write(buf("c"));
    check("off: immediate passthrough", seen.join("|") === "a|b|c", seen.join("|"));
    sink.dispose();
  }

  // 4. Dragged to OFF mid-stream: the held buffer flushes *before* the new chunk,
  //    so the switch can never reorder output across itself.
  {
    let fps = 30;
    const seen: string[] = [];
    const sink = coalesceOutput((bytes) => seen.push(dec.decode(bytes)), () => fps);
    sink.write(buf("buffered-")); // held for a later frame
    fps = 0; // operator drags the slider to OFF
    sink.write(buf("live")); // must land AFTER the buffered part
    check("off mid-stream: order preserved across the switch", seen.join("") === "buffered-live", JSON.stringify(seen));
    sink.dispose();
  }

  if (failures === 0) console.log("%c ALL PASS ", "background:#2b7;color:#000;font-weight:bold");
  else console.error(`${failures} CHECK(S) FAILED`);
}

void run();
