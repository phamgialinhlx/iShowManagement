# ADR-001 — Terminal frame-rate cap

Status: **Accepted** (grilled 2026-08-10, implemented same day)

Follows from a systematic-debugging investigation of macOS `WindowServer` CPU: the operator
reported it dropping from ~25% to ~10% the moment rmux was quit.

## Context

rmux's window is one large WebKit webview. On a high-DPI / 4K display, macOS `WindowServer`
composites that window **once per frame the webview produces**, and that composite is expensive.
The investigation established, by controlled measurement, where the ~15% went:

- **It is compositing, not rmux compute.** At 45% `WindowServer`, rmux's own processes — the app,
  the WebKit WebContent renderer, and the WebKit GPU helper — were all near-idle. The frame is
  cheap to **produce** (xterm/WebGL) and expensive to **composite** on 4K.
- **Not CSS animations.** Disabling every `infinite` animation via user CSS dropped WebContent
  9% → 3.4% but left `WindowServer` unchanged (32% → 35%). The rail spinners / box-shadow glows
  are not the driver.
- **Not transparency/vibrancy.** Under a *fixed* terminal load, a translucent `DESKTOP` backdrop
  and a fully opaque `COLOUR` backdrop measured 18.1% vs 19.0% — identical.
- **It is terminal redraw rate.** A `yes`-flood terminal held `WindowServer` at a stable ~29%;
  the *same* full-screen redraw rate-limited to ~4fps sat at ~10.6% — the idle floor.

So the cost is `(terminal frames/sec) × (cost to composite one big-window frame on 4K)`, and the
frame rate is the lever. During heavy output (a Claude stream, a build log) the terminals repaint
at up to the display refresh — up to 120fps on the built-in ProMotion panel — and every one of
those frames is a full-window composite.

The two xterm hosts (`Terminal.tsx`, `ClaudePanel.tsx`) each write PTY bytes to xterm the same
way: a Tauri `Channel<ArrayBuffer>` whose `onmessage` calls `xterm.write(...)`. That `onmessage`
is the one place to intervene.

The operator asked for "a config to set the fps for the xterm."

## Decisions

1. **Opt-in; default OFF; `0` = uncapped.** The cost is specific to a large window on a high-DPI
   *external* display. Most setups (built-in panel, smaller window) pay little, and must not have
   their scroll made chunkier for a problem they do not have — the same "an app that silently
   changes its own behaviour is unsettling" reasoning that keeps native glass off by default. `0`
   carries its own off state, like a tracking goal of zero. A default 30fps cap for everyone was
   rejected on those grounds; a default 60 (a near-invisible win on ProMotion) was considered and
   left for later data.

2. **One global knob, both hosts (`rmux.terminal.fps`).** The compositing cost is **per-window,
   not per-pane** — any pane's frame dirties the whole webview and forces one composite — so the
   thing worth controlling is the aggregate rate, which a single knob sets directly. Per-type
   (shell vs Claude) and per-pane caps were rejected as speculative granularity: new persisted
   state and UI for a distinction nobody has asked for. Mirrors the existing single-global
   `gpuRendering()` setting.

3. **Flat cap — throttle all output equally.** On a remote session a typed character only appears
   once the host echoes it back through this same path, so a cap adds up to one frame-budget of
   echo latency (~33ms at 30fps, ~16ms at 60). That is small next to SSH round-trip, and the fps
   value *is* the operator's latency/cost lever. An "interactive-aware" variant (flush small/
   just-typed output immediately, throttle only sustained bulk) was rejected: it is a heuristic
   with edge cases (prompts, progress bars, pagers), against this codebase's preference for one
   invariant over a cleverness that is only right on the case you tested. In practice interactive
   output is tiny, so a flat cap barely engages except during the floods it exists for.

4. **Applies live, no remount.** Unlike `gpuRendering()` (baked in at xterm *construction*, hence
   its reload), the cap lives in the output path — a per-pane closure — so it can be re-read from
   a variable the existing `storage`/appearance handler refreshes. A control that does nothing
   until a remount "reads as broken" (the design's explicit warning; "restart is offered, never
   required"), and this is a slider someone drags while watching `WindowServer`, so it *must* be
   live.

5. **Segmented control `OFF / 15 / 30 / 60`, in the existing TERMINAL RENDERING section.** The
   design rule is "segmented buttons over dropdowns for small closed choices; state must be
   readable without clicking," and the useful fps values are a small closed set. `OFF` is the
   leftmost segment, so the default needs no separate enable switch. **120 was dropped** as
   ~pointless (near-uncapped on a 120Hz panel; 60 already halves it there). It sits beside the GPU
   toggle — the terminal-knobs cluster — but under its own "applies live" copy, *not* the GPU
   toggle's "RELOADS THE WINDOW" note.

6. **rAF-gated flush with a timestamp check.** The coalescer schedules a `requestAnimationFrame`
   and flushes only when `now − lastFlush ≥ 1000/fps`. This aligns flushes to the compositor's own
   cadence and **pauses for free while the window is hidden** — rAF does not fire off-screen — the
   same "do nothing off-screen" property the CSS animations already rely on. A `setInterval`
   was rejected: it drifts against vsync and keeps firing while hidden, re-creating background
   waste already fixed elsewhere.

## The model

A single setting, `rmux.terminal.fps` in `localStorage`: a positive integer cap, or absent/`0`
for uncapped. `terminalFps()` / `setTerminalFps()` (`ui/src/lib/terminal-fps.ts`) mirror the
`gpuRendering()` pair.

The same file exports `coalesceOutput(onBatch, fps)`, the shared throttle both hosts use. It
buffers incoming chunks and hands `onBatch` the **concatenated** bytes at most `fps()` times a
second; `onBatch` is the one place that runs any per-chunk detection *and* writes to xterm. When
`fps()` is `0` it writes straight through. Each host keeps a `fpsValue` closure variable that its
`onAppearance` handler refreshes when a `storage` event carries the `rmux.terminal.fps` key.

## Invariants this must not break

- **No bytes lost.** The buffer is flushed whole, including a **synchronous flush on dispose**
  (`sink.dispose()` in both hosts' cleanup) — the tail of the output before a disconnect must not
  vanish with the pending buffer.
- **Order preserved.** One ordered buffer, flushed whole: batching is *fewer, larger* writes than
  today, never a reordering.
- **Detectors intact.** `ClaudePanel`'s mouse-mode escape hatch (`mouseModes.observe`) and
  context-window sniff run on the coalesced batch. A sought sequence can straddle a flush boundary
  — but it could already straddle two network chunks today, and larger batches make that *less*
  frequent, not more.
- **OFF is exactly today.** `fps = 0` bypasses the buffer entirely; the throttle path adds no
  overhead.
- **Live, cross-window.** Setting the value in the Settings window fires `storage`; the workbench
  terminals pick it up with no remount.

## Consequences

- **New `ui/src/lib/terminal-fps.ts`** — the setting pair, the `TERMINAL_FPS_KEY` event key, and
  the `coalesceOutput` throttle.
- **`Terminal.tsx` and `ClaudePanel.tsx`** — `output.onmessage` now feeds a `coalesceOutput`
  sink; `onAppearance` refreshes `fpsValue` on the `rmux.terminal.fps` key; cleanup calls
  `sink.dispose()`. `ClaudePanel`'s per-chunk detectors moved verbatim into the batch callback.
- **`AppearancePanel.tsx`** — an `OFF / 15 / 30 / 60` segmented control added to the TERMINAL
  RENDERING section, writing via `setTerminalFps` (live, no reload).
- **No Rust, no agent, no `src-tauri` change.** UI-only; ships with an ordinary `pnpm tauri build`.

## Risks / open follow-ups

- **Default may want to become 60.** If ProMotion machines see a meaningful idle-vs-busy
  `WindowServer` gap on the built-in panel, a default 60 cap is a near-invisible universal win.
  Left opt-in until there is data (decision 1).
- **Per-type cap deferred.** If measurement later shows Claude's constant TUI redraw dominates
  while shells rarely matter, decision 2's single knob can gain a Claude-only variant without
  changing the wire or storage shape.
- **Echo latency at 15fps** (~66ms) is noticeable on a fast local session; it is the strongest
  cost-saver and offered deliberately, but 30 is the sensible starting point for most.
