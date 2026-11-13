import { useEffect, useRef, useState } from "react";
import { Terminal as Xterm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";

import { Channel, invoke, isTauri } from "../lib/bridge";
import type { TargetRef } from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { reattachName, type SessionV3 } from "../lib/workspace-model";
import { claudeTheme } from "../lib/terminal-theme";
import { readMonoStack } from "../lib/fonts";
import { scaledFontSize, remeasureOnFontLoad } from "../lib/terminal-font";
import { settleResize } from "../lib/terminal-resize";
import { attachClipboard } from "../lib/terminal-clipboard";
import { VIEW_EVENT } from "../lib/shortcuts";
import { PanelLoader } from "./PanelLoader";
import { SessionSettings } from "./SessionSettings";
import { BackView, GEAR_PATH, Icon, IconButton, RELOAD_PATH } from "./SessionPaneChrome";
import { loadGpuRenderer } from "../lib/gpu-renderer";

/**
 * A DeepSeek Harness session — dsh's own Web UI, shown in the pane.
 *
 * dsh has no terminal UI, so the pane is two things stacked:
 *
 * - **the console** — the `dsh web` process's own output, streamed exactly like
 *   a Claude pane's screen. It is what shows while starting (an `npx` first run
 *   downloads for a while, and a blank pane reads as broken), and it is where a
 *   missing Node or a crash explains itself.
 * - **the Web UI** — a native child webview the desktop places over this pane's
 *   rectangle (`src-tauri/src/dsh.rs`, which also says why not an `<iframe>`:
 *   dsh's cookie is `SameSite=Strict`, so a cross-site frame cannot sign in).
 *   This component only reports where the rectangle is and whether it is
 *   visible; the URL and token never reach the page.
 *
 * Status for the rail comes from dsh itself (`session/list` over the forward):
 * `running` → working, an open question → waiting, otherwise idle.
 */
type View = "web" | "console" | "settings";

const POLL_STARTING_MS = 1000;
const PROBE_MS = 3000;
/** Consecutive unreachable probes before the forward is redialled. */
const REDIAL_AFTER = 2;

type DshState = { phase: "starting" | "fetching" | "ready" | "failed"; remotePort?: number; problem?: string };
type Rect = { x: number; y: number; width: number; height: number };

/**
 * Is any rmux overlay (`.window` dialog or `.menu`) drawn over `rect`?
 * Surfaces *inside* the pane itself (its own header menus are not native-covered)
 * are ignored, as are invisible/zero-size ones.
 */
function overlayCovers(rect: DOMRect, pane: HTMLElement | null): boolean {
  for (const el of document.querySelectorAll<HTMLElement>(".window, .menu")) {
    if (pane && pane.contains(el)) continue;
    const o = el.getBoundingClientRect();
    if (o.width < 1 || o.height < 1) continue;
    if (o.right > rect.left && o.left < rect.right && o.bottom > rect.top && o.top < rect.bottom) {
      return true;
    }
  }
  return false;
}

export function DshSessionPane({ session }: { session: SessionV3 }) {
  const target = useWorkspace((s) => s.targetOf(session.id));
  const project = useWorkspace((s) => s.projectOf(session.id));
  const folder = project?.folder ?? "";

  if (!isTauri()) {
    // rmux-web (or a plain tab): the forward would land on the dev host's
    // loopback, which this browser cannot reach. Say so rather than draw a
    // frame that can never load.
    return (
      <div className="grid h-full place-items-center p-6">
        <div className="max-w-md text-center">
          <p className="micro" style={{ letterSpacing: "0.08em" }}>
            DEEPSEEK HARNESS
          </p>
          <p className="mt-2 text-[12px]" style={{ color: "var(--text-soft)" }}>
            DeepSeek Harness sessions are not available in rmux-web yet. Its Web UI is served on the
            server's own loopback and reached through a tunnel only the desktop app can open. The
            session keeps running — open it from the rmux desktop app.
          </p>
        </div>
      </div>
    );
  }

  return <DshDesktopPane session={session} target={target} folder={folder} />;
}

function DshDesktopPane({ session, target, folder }: { session: SessionV3; target: TargetRef; folder: string }) {
  const live = useWorkspace((s) => s.live[session.id]);
  const setLive = useWorkspace((s) => s.setLive);
  const clearLive = useWorkspace((s) => s.clearLive);
  const setStatus = useWorkspace((s) => s.setStatus);
  const setSessionError = useWorkspace((s) => s.setSessionError);

  const [view, setView] = useState<View>("web");
  const [state, setState] = useState<DshState>({ phase: "starting" });
  const [opened, setOpened] = useState<{ localPort: number; remotePort: number } | null>(null);
  const [viewError, setViewError] = useState<string | null>(null);
  const [startError, setStartError] = useState<string | null>(null);
  const [generation, setGeneration] = useState(0);
  const [restarting, setRestarting] = useState(false);
  const slotRef = useRef<HTMLDivElement>(null);
  const name = reattachName(session);

  // A keyboard shortcut asking this pane to change view, addressed by id.
  useEffect(() => {
    const onView = (e: Event) => {
      const detail = (e as CustomEvent<{ sessionId: string; view: string }>).detail;
      if (detail?.sessionId !== session.id) return;
      if (detail.view === "settings") setView("settings");
      else setView("web");
    };
    window.addEventListener(VIEW_EVENT, onView);
    return () => window.removeEventListener(VIEW_EVENT, onView);
  }, [session.id]);

  // ── where is the server? Poll the parsed console until it is ready. ──
  useEffect(() => {
    if (!live) return;
    let stop = false;
    const tick = async () => {
      try {
        const s = await invoke<DshState>("dsh_state", { id: live });
        if (stop) return;
        setState(s);
        setSessionError(session.id, s.phase === "failed" ? (s.problem ?? "DeepSeek Harness stopped") : null);
      } catch {
        // The attach is gone (app restarted, ssh dropped): the console starts
        // again by name, which reattaches to the server still running.
        if (stop) return;
        clearLive(session.id);
      }
    };
    void tick();
    const timer = window.setInterval(tick, POLL_STARTING_MS);
    return () => {
      stop = true;
      window.clearInterval(timer);
    };
  }, [live, session.id, clearLive, setSessionError]);

  const ready = state.phase === "ready";
  const showWeb = view === "web" && ready && !restarting;

  // ── the native view follows this pane's rectangle ──
  useEffect(() => {
    if (!ready || !live) return;
    let stop = false;
    let last = "";
    let openedOnce = false;
    let misses = 0;

    const rectOf = (): { rect: Rect; visible: boolean } => {
      const el = slotRef.current;
      const r = el?.getBoundingClientRect();
      // Hidden tab, collapsed split, or display:none ancestor all measure 0.
      // **A native view draws above the whole page**, so any rmux dialog or
      // menu over this pane would open *behind* it — reachable by nothing. The
      // view therefore steps aside while one overlaps it: every rmux overlay is
      // a `.window` or `.menu` surface, so overlap is measured, not guessed, and
      // a toast in a corner that does not touch the pane leaves it alone.
      const visible =
        !!r &&
        showWeb &&
        r.width > 2 &&
        r.height > 2 &&
        document.visibilityState === "visible" &&
        !overlayCovers(r, el);
      return { rect: { x: r?.left ?? 0, y: r?.top ?? 0, width: r?.width ?? 0, height: r?.height ?? 0 }, visible };
    };

    const open = async () => {
      const { rect, visible } = rectOf();
      try {
        const o = await invoke<{ localPort: number; remotePort: number }>("dsh_view_open", {
          sessionId: session.id,
          attachId: live,
          target,
          rect,
          visible,
        });
        if (stop) return;
        openedOnce = true;
        last = JSON.stringify({ rect, visible });
        setOpened(o);
        setViewError(null);
      } catch (e) {
        if (!stop) setViewError(typeof e === "string" ? e : String(e));
      }
    };

    // Geometry: compared every frame-ish, sent only on change. A rail drag, a
    // split drag, a tab switch and a window resize all move the rect without
    // resizing this element's own box, so an observer alone misses half of them.
    const follow = window.setInterval(() => {
      if (!openedOnce) return;
      const next = rectOf();
      const key = JSON.stringify(next);
      if (key === last) return;
      last = key;
      void invoke<boolean>("dsh_view_bounds", { sessionId: session.id, ...next }).then((exists) => {
        // The view was closed under us (server disconnect): reopen on demand.
        if (!exists && !stop) {
          openedOnce = false;
          setOpened(null);
          setViewError("The connection to this server was closed. Reconnect to show DeepSeek Harness again.");
        }
      });
    }, 120);

    // Status and liveness: one `session/list` through the forward.
    const probe = window.setInterval(async () => {
      if (!openedOnce) return;
      try {
        const p = await invoke<{ reachable: boolean; status?: "working" | "waiting" | "idle" }>("dsh_probe", {
          sessionId: session.id,
        });
        if (stop) return;
        if (p.reachable) {
          misses = 0;
          if (p.status) setStatus(session.id, p.status);
        } else if (++misses >= REDIAL_AFTER) {
          // The forward died with the network. Asking again redials it and
          // re-navigates the view if the local port changed.
          misses = 0;
          void open();
        }
      } catch {
        // A failed probe is not a status; leave the last one.
      }
    }, PROBE_MS);

    void open();
    return () => {
      stop = true;
      window.clearInterval(follow);
      window.clearInterval(probe);
      // Unmounted or re-run: hide, never close — the forward and the page's
      // state survive switching away and back. Close is for remove/detach.
      void invoke("dsh_view_bounds", {
        sessionId: session.id,
        rect: { x: 0, y: 0, width: 0, height: 0 },
        visible: false,
      }).catch(() => {});
    };
  }, [ready, live, showWeb, generation, session.id, target, setStatus]);

  const restart = async () => {
    // Kill the server on the host, drop this machine's view, start fresh.
    setRestarting(true);
    try {
      await invoke("dsh_view_close", { sessionId: session.id }).catch(() => {});
      await invoke("terminal_close", { id: live ?? "", target, session: name }).catch(() => {});
      if (live) await invoke("claude_stop", { id: live }).catch(() => {});
    } finally {
      clearLive(session.id);
      setOpened(null);
      setState({ phase: "starting" });
      setGeneration((g) => g + 1);
      setRestarting(false);
    }
  };

  const route = opened
    ? opened.localPort === opened.remotePort
      ? `127.0.0.1:${opened.localPort}`
      : `127.0.0.1:${opened.localPort} → :${opened.remotePort}`
    : state.remotePort
      ? `:${state.remotePort}`
      : "";

  const header = (
    <div className="flex shrink-0 items-center gap-2 border-b px-3 py-1" style={{ borderColor: "var(--border)" }}>
      <span className="micro" style={{ letterSpacing: "0.06em" }}>
        DEEPSEEK HARNESS
      </span>
      <span className="micro truncate" style={{ color: "var(--text-faint)" }}>
        {phaseLabel(state.phase)}
        {route ? ` · ${route}` : ""}
      </span>
      <span className="flex-1" />
      <IconButton
        label={view === "console" ? "Show the Web UI" : "Show the server console"}
        onClick={() => setView(view === "console" ? "web" : "console")}
      >
        <Icon d={CONSOLE_PATH} />
      </IconButton>
      {opened && (
        <IconButton
          label="Open in your browser"
          onClick={() => void invoke("dsh_open_browser", { sessionId: session.id }).catch(() => {})}
        >
          <Icon d={EXTERNAL_PATH} />
        </IconButton>
      )}
      <IconButton
        label={restarting ? "Restarting…" : "Restart DeepSeek Harness on the server"}
        disabled={restarting}
        onClick={() => void restart()}
      >
        <Icon d={RELOAD_PATH} />
      </IconButton>
      <IconButton label="Session settings" onClick={() => setView("settings")}>
        <Icon d={GEAR_PATH} />
      </IconButton>
    </div>
  );

  // The console stays mounted so its attach (and the server's output) is not
  // torn down by switching views; it is merely hidden behind the Web UI.
  const consoleVisible = view === "console" || (view === "web" && !ready);

  return (
    <div className="flex h-full min-h-0 flex-col">
      {view === "settings" ? (
        <BackView label="SETTINGS" onBack={() => setView("web")} backLabel="Back to DeepSeek Harness">
          <SessionSettings sessionId={session.id} />
        </BackView>
      ) : (
        header
      )}

      <div className="relative min-h-0 flex-1" style={{ display: view === "settings" ? "none" : "block" }}>
        {(state.phase === "starting" || state.phase === "fetching") && view === "web" && (
          <div className="absolute inset-x-0 top-0 z-10">
            <PanelLoader
              variant="inline"
              phase={state.phase === "fetching" ? "Fetching DeepSeek Harness with npx" : "Starting DeepSeek Harness"}
              detail={folder}
            />
          </div>
        )}
        {(state.phase === "failed" || startError || viewError) && (
          <div
            className="border-b px-3 py-2 text-[12px]"
            style={{ borderColor: "var(--border)", color: "var(--text-soft)" }}
            role="status"
          >
            {startError ?? (state.phase === "failed" ? state.problem : viewError)}
            {viewError && !startError && state.phase !== "failed" && (
              <button type="button" className="chip ml-2" onClick={() => setGeneration((g) => g + 1)}>
                reconnect
              </button>
            )}
          </div>
        )}

        <div className="h-full" style={{ display: consoleVisible ? "block" : "none" }}>
          <DshConsole
            key={generation}
            target={target}
            cwd={session.hostName ? undefined : (session.cwd ?? folder)}
            name={name}
            modelProfile={session.modelProfile}
            attachTo={live}
            onStarted={(id) => {
              setStartError(null);
              setLive(session.id, id);
            }}
            onStale={() => clearLive(session.id)}
            onError={setStartError}
          />
        </div>

        {/* The slot the native Web UI is laid over. Painted in the panel tone
            so a frame of lag on resize shows the app, not a hole. */}
        <div
          ref={slotRef}
          className="h-full"
          style={{ display: view === "web" && ready ? "block" : "none" }}
          aria-label="DeepSeek Harness Web UI"
        />
      </div>
    </div>
  );
}

function phaseLabel(phase: DshState["phase"]): string {
  switch (phase) {
    case "starting":
      return "starting";
    case "fetching":
      return "downloading with npx";
    case "ready":
      return "running";
    case "failed":
      return "stopped";
  }
}

/** `>_` in a box — the server's own console. */
const CONSOLE_PATH = "M4 5h16v14H4zM7 9l3 3-3 3M12 15h5";
/** An arrow leaving a box — open outside rmux. */
const EXTERNAL_PATH = "M14 4h6v6M20 4l-9 9M18 14v6H4V6h6";

/**
 * The `dsh web` process's output: start (or reattach by name) and stream it.
 *
 * A smaller sibling of `ClaudePanel`'s terminal: the same theme, font, GPU
 * renderer and clipboard, without the agent-specific machinery (context meter,
 * mouse-mode restore, image paste) a log does not need. Typing goes through, so
 * Ctrl-C reaches the server like it would in a shell.
 */
function DshConsole({
  target,
  cwd,
  name,
  modelProfile,
  attachTo,
  onStarted,
  onStale,
  onError,
}: {
  target: TargetRef;
  cwd?: string;
  name: string;
  /** The session's DeepSeek profile; applied before the server starts. */
  modelProfile?: string;
  attachTo?: string;
  onStarted: (id: string) => void;
  onStale: () => void;
  onError: (message: string | null) => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  // Captured once per mount: the live handle changes *because* of this mount.
  const attachRef = useRef(attachTo);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let disposed = false;
    let id: string | null = null;

    const xterm = new Xterm({
      theme: claudeTheme(),
      allowTransparency: true,
      fontFamily: readMonoStack(),
      fontSize: scaledFontSize(12),
      lineHeight: 1.25,
      scrollback: 5000,
      convertEol: false,
    });
    const fit = new FitAddon();
    xterm.loadAddon(fit);
    xterm.open(host);
    remeasureOnFontLoad(xterm, fit, readMonoStack, () => !disposed);
    // See `lib/gpu-renderer.ts`: also repaints after a wake, which can
      // corrupt the glyph atlas without any context-loss event.
      const gpu = loadGpuRenderer(xterm);
    const clipboard = attachClipboard(xterm);
    try {
      fit.fit();
    } catch {
      // Hidden at mount: start at the default size, refit when shown.
    }

    const output = new Channel<ArrayBuffer>();
    output.onmessage = (chunk) => xterm.write(new Uint8Array(chunk));
    const input = xterm.onData((data) => {
      if (id) void invoke("claude_write", { id, data });
    });

    const start = (existing?: string) => {
      const cols = Math.max(xterm.cols, 40);
      const rows = Math.max(xterm.rows, 10);
      const request = existing
        ? invoke("claude_attach", { id: existing, output }).then(() => ({ id: existing }))
        : invoke<{ id: string }>("dsh_start", {
            target,
            cwd,
            sessionName: name,
            modelProfile: modelProfile ?? null,
            cols,
            rows,
            output,
          });
      void request
        .then((started) => {
          if (disposed) return;
          id = started.id;
          onError(null);
          if (!existing) onStarted(started.id);
        })
        .catch((e) => {
          if (disposed) return;
          if (existing) {
            // Stale handle: start by name, which reattaches to the server.
            onStale();
            start();
            return;
          }
          onError(typeof e === "string" ? e : String(e));
        });
    };
    start(attachRef.current);

    const settle = settleResize(
      () => {
        try {
          if (host.offsetWidth > 0 && host.offsetHeight > 0) fit.fit();
        } catch {
          // Not laid out.
        }
      },
      () => {
        if (id && host.offsetWidth > 0) void invoke("claude_resize", { id, cols: xterm.cols, rows: xterm.rows });
      },
    );
    const observer = new ResizeObserver(() => settle.observe());
    observer.observe(host);

    return () => {
      disposed = true;
      observer.disconnect();
      settle.dispose();
      input.dispose();
      clipboard();
      gpu();
      xterm.dispose();
    };
    // `target`/`cwd`/`name` identify the session; a change remounts via `key`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Padding on a wrapper, never on the element FitAddon measures — it would
  // ask for a row more than the pane holds (CLAUDE.md, the black-bar rule).
  return (
    <div className="h-full px-2 py-1">
      <div ref={hostRef} className="h-full" />
    </div>
  );
}
