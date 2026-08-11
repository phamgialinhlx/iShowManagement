/**
 * Terminal frame-rate cap.
 *
 * WindowServer composites rmux's window once per terminal frame, and on a
 * high-DPI / 4K display that composite is expensive. During heavy output — a
 * Claude stream, a `yes` flood — the terminals repaint at up to the display
 * refresh, so a busy session drove WindowServer ~15% higher than an idle one
 * with rmux's *own* processes (app, WebContent, GPU) near-idle the whole time:
 * measured, the frame is cheap to **produce** and expensive to **composite**.
 * Capping how often a terminal repaints cuts that compositing proportionally —
 * measured, a full-screen redraw rate-limited to ~4fps sat at the idle floor
 * while the same redraw uncapped sat ~3x higher. See
 * `references/terminal-fps/ADR-001-terminal-fps-cap.md`.
 *
 * **Off by default (`0`).** The cost is specific to a large window on a
 * high-DPI external display; most setups pay nothing and must not have their
 * scroll made chunkier for a problem they do not have. `0` carries its own off
 * state, the same way a tracking goal of zero means "not tracking".
 */

const FPS_KEY = "rmux.terminal.fps";

/** The event key, exported so a host's `storage` handler can spot a live change. */
export const TERMINAL_FPS_KEY = FPS_KEY;

/** The cap in frames/sec, or `0` for uncapped (write every frame, as today). */
export function terminalFps(): number {
  const raw = Number(localStorage.getItem(FPS_KEY));
  return Number.isFinite(raw) && raw > 0 ? raw : 0;
}

export function setTerminalFps(fps: number): void {
  if (fps > 0) localStorage.setItem(FPS_KEY, String(Math.round(fps)));
  else localStorage.removeItem(FPS_KEY);
}

export type OutputSink = {
  /** Feed one raw output chunk straight off the PTY channel. */
  write(chunk: ArrayBuffer): void;
  /**
   * Flush whatever is buffered and stop the clock. Call from the pane's dispose
   * so the last bytes before a disconnect are not thrown away with the pending
   * buffer.
   */
  dispose(): void;
};

/**
 * Coalesce PTY output and hand it to `onBatch` at most `fps()` times a second.
 *
 * `onBatch` receives the concatenated bytes and is the single place that both
 * runs any per-chunk detection *and* writes to xterm. Order is preserved: one
 * ordered buffer is flushed whole, so this is *fewer, larger* calls than
 * writing each network chunk — never a reordering. (A byte sequence a detector
 * looks for can straddle a flush boundary, but it could already straddle two
 * network chunks today; larger batches make that *less* frequent, not more.)
 *
 * **rAF-gated.** The flush rides `requestAnimationFrame`, so it aligns to the
 * compositor's own cadence and pauses for free while the window is hidden (rAF
 * does not fire) — the same "do nothing off-screen" property the CSS animations
 * rely on. When `fps()` is `0` the chunk is written straight through, so OFF
 * carries no overhead over the original path.
 */
export function coalesceOutput(
  onBatch: (bytes: Uint8Array) => void,
  fps: () => number,
): OutputSink {
  let parts: Uint8Array[] = [];
  let buffered = 0;
  let raf = 0;
  let last = 0;

  const flush = () => {
    raf = 0;
    if (buffered === 0) return;
    // `buffered > 0` guarantees a first element; the single-part case skips a copy.
    const batch = parts.length === 1 ? parts[0]! : join(parts, buffered);
    parts = [];
    buffered = 0;
    last = performance.now();
    onBatch(batch);
  };

  const schedule = () => {
    if (!raf) raf = requestAnimationFrame(tick);
  };

  const tick = () => {
    raf = 0;
    if (buffered === 0) return;
    const f = fps();
    // Turned OFF while bytes were held: flush now rather than wait forever on a
    // `1000 / 0` budget that never elapses.
    if (f <= 0 || performance.now() - last >= 1000 / f) flush();
    else schedule();
  };

  return {
    write(chunk) {
      if (fps() <= 0) {
        // OFF. Anything the throttle was still holding when it was switched off
        // goes first, so the switch can never reorder output across itself.
        if (buffered > 0) flush();
        onBatch(new Uint8Array(chunk));
        return;
      }
      parts.push(new Uint8Array(chunk));
      buffered += chunk.byteLength;
      schedule();
    },
    dispose() {
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
      if (buffered > 0) flush();
    },
  };
}

function join(parts: Uint8Array[], total: number): Uint8Array {
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}
