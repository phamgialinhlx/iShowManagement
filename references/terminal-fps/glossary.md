# Glossary — terminal frame-rate

- **Frame-rate cap (terminal FPS)** — the maximum number of times per second a terminal pane
  repaints. Stored as `rmux.terminal.fps`. `0` (or absent) means **uncapped** — repaint on every
  frame, the historical behaviour. A positive integer caps it.

- **Coalescing** — buffering the PTY output bytes as they arrive and handing them to xterm in one
  batch per flush, instead of writing each network chunk immediately. Nothing is dropped; the
  same bytes are drawn, just repainted fewer times per second.

- **Flush budget** — the minimum time between flushes, `1000 / fps` milliseconds.

- **Per-window composite** — macOS `WindowServer` re-composites rmux's whole window once per frame
  the webview produces. Because it is per-*window*, any one pane's redraw carries the same cost as
  the whole grid redrawing; that is why a single global cap (not per-pane) matches the physics.

- **Produce vs composite** — a terminal frame is cheap to *produce* (xterm/WebGL, near-zero app
  CPU) and expensive to *composite* on a high-DPI / 4K display (the `WindowServer` cost). The cap
  reduces the composite count, which is where the cost lives.
