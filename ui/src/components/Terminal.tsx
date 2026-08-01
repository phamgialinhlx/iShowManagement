import { useEffect, useRef, useState } from "react";
import { Terminal as Xterm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Channel, invoke } from "@tauri-apps/api/core";
import "@xterm/xterm/css/xterm.css";

import { isTauri, type TargetRef } from "../lib/api";
import { attachClipboard } from "../lib/terminal-clipboard";
import { TERMINAL_THEME } from "../lib/terminal-theme";
import { PanelLoader } from "./PanelLoader";

/**
 * A terminal.
 *
 * The PTY lives in Rust and **outlives this component**. On mount the view either
 * attaches to an existing PTY or opens a new one; on unmount it detaches without
 * killing anything. That distinction is what makes the terminal stable: a
 * remount for any reason — a layout change, React's development double-mount —
 * would otherwise destroy the running shell and start another, which reads as
 * the terminal flashing and resetting itself.
 *
 * The PTY is destroyed only when the tab is explicitly closed, which is the
 * caller's job.
 */

// One palette for every terminal — see `lib/terminal-theme.ts` for why the
// dark slots are translucent now that the panes are glass.

type Lifecycle = { type: "exited"; code: number } | { type: "lagged"; chunks: number };

export function TerminalView({
  target,
  cwd,
  session,
  ptyId,
  onOpened,
  onExit,
}: {
  target: TargetRef;
  cwd?: string;
  /**
   * Stable name for the shell on the target.
   *
   * This is what makes a terminal persistent. `ptyId` identifies a live
   * attachment inside *this* run of the app and is gone after a restart; the
   * session name is stored, so reopening rmux reattaches to the same shell —
   * still running, scrollback intact, jobs still going.
   */
  session?: string;
  /** Existing PTY to reattach to. Absent means open a new one. */
  ptyId?: string;
  /** Reports the PTY id so the caller can reattach after a remount. */
  onOpened?: (id: string) => void;
  onExit?: (code: number) => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Same reason as the Claude pane: an empty black rectangle reads as a crash.
  const [ready, setReady] = useState(false);

  // Read inside callbacks without re-creating the terminal.
  const ptyRef = useRef(ptyId);
  ptyRef.current = ptyId;
  const onOpenedRef = useRef(onOpened);
  onOpenedRef.current = onOpened;
  const onExitRef = useRef(onExit);
  onExitRef.current = onExit;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    // A terminal needs the Tauri IPC bridge. Without it `new Channel()` throws,
    // and an uncaught throw inside an effect unmounts the WHOLE React tree.
    if (!isTauri()) {
      setError("Terminals need the rmux desktop shell. Run `pnpm tauri dev`.");
      return;
    }

    let disposed = false;
    let terminalId: string | null = null;

    const xterm = new Xterm({
      theme: TERMINAL_THEME,
      allowTransparency: true,
      fontFamily: '"IBM Plex Mono", ui-monospace, Menlo, monospace',
      fontSize: 13,
      lineHeight: 1.25,
      // Rule 2: the cursor is the one thing in this design allowed to blink.
      cursorBlink: true,
      cursorStyle: "bar",
      scrollback: 10_000,
      macOptionIsMeta: true,
    });

    const fit = new FitAddon();
    xterm.loadAddon(fit);
    xterm.open(host);

    // The WebGL renderer is what makes a fast remote session feel local. It is
    // not available everywhere, so failure falls back rather than blanking.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      xterm.loadAddon(webgl);
    } catch {
      // Canvas/DOM fallback is automatic.
    }

    attachClipboard(xterm);

    const output = new Channel<ArrayBuffer>();
    output.onmessage = (chunk) => {
      // Raw bytes, not JSON — see the note in src-tauri/src/terminal.rs.
      xterm.write(new Uint8Array(chunk));
      setReady(true);
    };

    const lifecycle = new Channel<Lifecycle>();
    lifecycle.onmessage = (event) => {
      if (event.type === "exited") {
        onExitRef.current?.(event.code);
      } else {
        // Say so rather than silently showing a terminal missing bytes.
        setNotice(`Output dropped (${event.chunks} chunks) — display may be incomplete.`);
      }
    };

    /**
     * Bind to a PTY, only once the container has a real size.
     *
     * Fitting at mount measures a container the browser has not laid out yet, so
     * xterm reports something like 12x4 and the shell is told its window is that
     * size — rendering into a space too small to read, which looks blank.
     */
    let binding = false;
    const bind = () => {
      if (binding || terminalId || disposed) return;
      binding = true;
      fit.fit();

      const existing = ptyRef.current;
      const request = existing
        ? // Reattach: the shell keeps running and its scrollback is replayed.
          invoke("terminal_attach", { id: existing, output, lifecycle }).then(() => ({
            id: existing,
          }))
        : invoke<{ id: string }>("terminal_open", {
            target,
            cwd,
            session,
            cols: xterm.cols,
            rows: xterm.rows,
            output,
            lifecycle,
          });

      void request
        .then(({ id }) => {
          if (disposed) return;
          terminalId = id;
          if (!existing) onOpenedRef.current?.(id);
          // Match the far side to the pane we actually have.
          void invoke("terminal_resize", { id, cols: xterm.cols, rows: xterm.rows });
          xterm.focus();
        })
        .catch((e) => {
          // A stale id means the PTY died while we were away. Opening a fresh one
          // beats an error for a terminal the user would just restart anyway.
          if (existing && !disposed) {
            binding = false;
            ptyRef.current = undefined;
            bind();
            return;
          }
          setError(typeof e === "string" ? e : String(e));
        });
    };

    const onData = xterm.onData((data) => {
      if (terminalId) void invoke("terminal_write", { id: terminalId, data });
    });

    const observer = new ResizeObserver(() => {
      // A hidden pane measures 0x0; fitting to that would tell the far side the
      // window collapsed and make full-screen programs redraw at 1x1.
      if (host.clientWidth === 0 || host.clientHeight === 0) return;

      if (!terminalId) {
        bind();
        return;
      }
      fit.fit();
      void invoke("terminal_resize", { id: terminalId, cols: xterm.cols, rows: xterm.rows });
    });
    observer.observe(host);

    // A container that is already sized emits no resize event, so kick it off.
    if (host.clientWidth > 0 && host.clientHeight > 0) bind();

    return () => {
      disposed = true;
      observer.disconnect();
      onData.dispose();
      xterm.dispose();
      // The PTY is deliberately left running. This view may be remounting, and
      // killing the shell here is what made the terminal flash and reset. It is
      // destroyed only when the tab is closed.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="relative h-full w-full">
      <div ref={hostRef} className="h-full w-full" />

      {!ready && !error && (
        <div
            className="absolute inset-0"
            // The *loader* only — the terminal's own transparency is settled in
            // `signal-room.css`, by overriding the opaque `.xterm-viewport`
            // xterm ships. Tinted rather than solid so this covers the panel
            // consistently with it while the shell is opening.
            style={{
              background: "color-mix(in srgb, var(--app-panel) var(--panel-tint, 64%), transparent)",
            }}
          >
          <PanelLoader
            phase="OPENING SHELL"
            detail={target.host ?? "local"}
            hint="First connection to a host also installs the session agent, so this shell survives rmux closing."
          />
        </div>
      )}

      {notice && (
        <p role="status" className="micro absolute bottom-1 right-2" style={{ color: "var(--warn)" }}>
          {notice}
        </p>
      )}
      {error && (
        <p
          role="alert"
          className="data absolute inset-x-0 top-0 p-3 text-[11px]"
          style={{ color: "rgb(var(--primary))" }}
        >
          {error}
        </p>
      )}
    </div>
  );
}
