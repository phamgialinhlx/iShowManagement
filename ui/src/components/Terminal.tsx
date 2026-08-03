import { useEffect, useRef, useState } from "react";
import { Terminal as Xterm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Channel, invoke } from "@tauri-apps/api/core";
import "@xterm/xterm/css/xterm.css";

import { isTauri, type TargetRef } from "../lib/api";
import { settleResize } from "../lib/terminal-resize";
import { attachClipboard } from "../lib/terminal-clipboard";
import { TERMINAL_THEME, gpuRendering } from "../lib/terminal-theme";
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
  /** Kept so a click anywhere in the pane can hand the keyboard back. */
  const termRef = useRef<Xterm | null>(null);
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

    termRef.current = xterm;
    const fit = new FitAddon();
    xterm.loadAddon(fit);
    xterm.open(host);

    // The WebGL renderer is what makes a fast remote session feel local. It is
    // not available everywhere, so failure falls back rather than blanking.
    // Skipped entirely when the operator has turned GPU rendering off — see
    // `gpuRendering`. Loading the addon and disposing it still creates and
    // loses a WebGL context, which is worse than never loading it.
    try {
      if (!gpuRendering()) throw new Error("gpu rendering is off");
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

        // A non-zero exit is an accident — ssh reports a broken connection as
        // 255 — and the shell behind it is still alive under the agent on the
        // target. So reattach rather than leaving a pane that looks like a
        // quiet terminal and silently swallows every keystroke.
        //
        // xterm is deliberately *not* torn down: the agent replays the session
        // backlog on reattach, and disposing here would throw away what the
        // operator is reading in order to get the same text back.
        if (event.code !== 0 && !disposed) {
          terminalId = null;
          binding = false;
          // Clearing the handle matters. It names an attachment inside the
          // connection that just died; reattaching to it fails forever.
          ptyRef.current = undefined;
          setNotice("Connection lost — reattaching…");
          // A beat first: a machine waking from sleep has an interface that is
          // up but not yet routable, so an immediate retry just burns itself.
          window.setTimeout(() => {
            if (!disposed) bind();
          }, 1200);
        }
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
          setNotice(null);
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

    // Same settling as the Claude pane: the local grid follows the window on
    // every callback, the far side hears about it once.
    const settle = settleResize(
      () => fit.fit(),
      () => {
        if (terminalId) {
          void invoke("terminal_resize", { id: terminalId, cols: xterm.cols, rows: xterm.rows });
        }
      },
    );

    const observer = new ResizeObserver(() => {
      // A hidden pane measures 0x0; fitting to that would tell the far side the
      // window collapsed and make full-screen programs redraw at 1x1.
      if (host.clientWidth === 0 || host.clientHeight === 0) return;

      if (!terminalId) {
        bind();
        return;
      }
      settle.observe();
    });
    observer.observe(host);

    // **Refit when the interface scale changes, not only when the pane
    // resizes.** The `ResizeObserver` above is the normal path, but it bails on
    // a zero-sized host — correct while a pane is hidden — and an appearance
    // change arriving from the *Settings window* does not necessarily move this
    // element in a way that fires it. The terminal was then left at the cell
    // grid it had computed earlier: content filling part of the pane, the rest
    // empty, and nudging any slider "fixed" it because that finally produced a
    // resize. A window that has to be jiggled to look right is the definition
    // of not responsive.
    const refit = () => {
      if (host.clientWidth === 0 || host.clientHeight === 0) return;
      fit.fit();
      if (terminalId) {
        void invoke("terminal_resize", { id: terminalId, cols: xterm.cols, rows: xterm.rows });
      }
    };
    // Two frames: the first lets the zoom land, the second lets layout settle
    // before xterm measures a cell.
    const onAppearance = (event: StorageEvent) => {
      if (event.key && event.key !== "rmux.appearance") return;
      requestAnimationFrame(() => requestAnimationFrame(refit));
    };
    window.addEventListener("storage", onAppearance);
    window.addEventListener("resize", refit);


    // A container that is already sized emits no resize event, so kick it off.
    if (host.clientWidth > 0 && host.clientHeight > 0) bind();

    return () => {
      disposed = true;
      observer.disconnect();
      settle.dispose();
      window.removeEventListener("storage", onAppearance);
      window.removeEventListener("resize", refit);
      onData.dispose();
      termRef.current = null;
      xterm.dispose();
      // The PTY is deliberately left running. This view may be remounting, and
      // killing the shell here is what made the terminal flash and reset. It is
      // destroyed only when the tab is closed.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      className="relative h-full w-full"
      // Same reason as the Claude pane: focus lost to a control elsewhere never
      // came back, and Space then scrolls xterm's viewport instead of reaching
      // the shell.
      onMouseDown={(e) => {
        if ((e.target as HTMLElement).closest("button, a, input, textarea, select")) return;
        termRef.current?.focus();
      }}
    >
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
