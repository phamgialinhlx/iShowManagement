import { WebglAddon } from "@xterm/addon-webgl";
import type { Terminal as Xterm } from "@xterm/xterm";

import { gpuRendering } from "./terminal-theme";

/**
 * The GPU renderer for one xterm, kept healthy across sleep.
 *
 * **A wake can corrupt the glyph atlas without losing the context.** WebGL
 * keeps its textures in GPU memory, and after the Mac sleeps (or the GPU is
 * switched) WebKit often hands the context back with the atlas holding
 * garbage — and fires *no* `webglcontextlost`. Every pane then draws text
 * with wrong-coloured cell backgrounds and black holes between words, which
 * reads as "the colours suddenly went unreadable" and survives until the app
 * restarts. Measured: the report came four minutes after a wake. So a wake is
 * detected independently — the page becoming visible again, or a timer that
 * fired far later than scheduled (the machine was asleep in between) — and
 * every live terminal rebuilds its atlas and repaints. Rebuilding is cheap and
 * idempotent, so a false positive costs one repaint.
 *
 * **A lost context is replaced, not just dropped.** Disposing the addon falls
 * back to the DOM renderer, which is visibly slow on a redrawing TUI (the
 * WebGL rule in CLAUDE.md); one retry after a second gets the GPU path back.
 * Retries are capped so a GPU that keeps losing contexts settles on DOM rather
 * than looping.
 *
 * Returns a disposer; call it before `xterm.dispose()`.
 */
export function loadGpuRenderer(xterm: Xterm): () => void {
  let addon: WebglAddon | null = null;
  let disposed = false;
  let losses = 0;

  const load = () => {
    if (disposed || !gpuRendering()) return;
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        webgl.dispose();
        if (addon === webgl) addon = null;
        if (++losses <= 3) window.setTimeout(load, 1000);
      });
      xterm.loadAddon(webgl);
      addon = webgl;
    } catch {
      // DOM renderer fallback is automatic; slower, but it renders.
    }
  };
  load();

  const entry = { xterm };
  live.add(entry);
  ensureWakeWatch();
  return () => {
    disposed = true;
    live.delete(entry);
    try {
      addon?.dispose();
    } catch {
      // Already gone with its context.
    }
    addon = null;
  };
}

const live = new Set<{ xterm: Xterm }>();

/** Rebuild every live terminal's glyph atlas and repaint it. */
export function repaintAllTerminals(): void {
  for (const { xterm } of live) {
    try {
      xterm.clearTextureAtlas();
      xterm.refresh(0, Math.max(0, xterm.rows - 1));
    } catch {
      // A terminal mid-dispose; the next one still gets repainted.
    }
  }
}

const TICK_MS = 5000;
/** A tick this late means the machine was asleep, not busy. */
const WAKE_GAP_MS = 20_000;
let watching = false;

function ensureWakeWatch() {
  if (watching || typeof window === "undefined") return;
  watching = true;
  let last = Date.now();
  window.setInterval(() => {
    const now = Date.now();
    if (now - last > WAKE_GAP_MS) repaintAllTerminals();
    last = now;
  }, TICK_MS);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") repaintAllTerminals();
  });
}
