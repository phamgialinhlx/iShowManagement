import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import {
  PREFIX_ACTIONS,
  tokenLabel,
  type PrefixConfig,
} from "../lib/prefix-mode";
import { prefixConfig, usePrefixMode } from "../lib/prefix-runtime";
import { ACTIONS, label as chordLabel, readBindings } from "../lib/shortcuts";
import { spaceOrder } from "../lib/use-prefix-mode";
import { useWorkspace } from "../lib/workspace";

/**
 * The mode bar: while PREFIX, NAVIGATE or RESIZE is active it takes over the
 * footer row with a pill naming the mode and the keys that mean something
 * *now*. A mode you cannot see is a mode you enter by accident and then type
 * into blind — the next letter would run a command with nothing saying why.
 *
 * Rendered inside the footer (which is `relative`), absolutely over its
 * content, so entering a mode changes no layout and no terminal refits.
 */
export function ModeBar() {
  const mode = usePrefixMode((s) => s.mode);
  const toast = usePrefixMode((s) => s.toast);
  const navTarget = usePrefixMode((s) => s.navTarget);
  const projects = useWorkspace((s) => s.projects);
  // Re-derived on each render, which only happens while a mode is shown.
  const order = mode === "navigate" ? spaceOrder() : [];
  if (mode === "terminal" && !toast) return null;

  const cfg = prefixConfig();
  const pre = tokenLabel(cfg.prefixes[0] ?? "Ctrl+b");
  const key = (id: (typeof PREFIX_ACTIONS)[number]["id"]) => tokenLabel(cfg.bindings[id] ?? "");

  return (
    <div className="mode-bar" role="status" aria-live="polite" data-mode={mode}>
      {mode !== "terminal" && (
        <span className="mode-pill" data-mode={mode}>
          {` ${mode.toUpperCase()} `}
        </span>
      )}
      {mode === "prefix" && (
        <span className="mode-hints">
          <b>esc</b> cancel · <b>{pre}</b> send prefix · <b>{key("mode.navigate")}</b> navigate ·{" "}
          <b>{key("pane.splitRight")}</b>/<b>{key("pane.splitDown")}</b> split · <b>{key("pane.zoom")}</b> zoom ·{" "}
          <b>{key("mode.resize")}</b> resize · <b>{key("tab.new")}</b> new tab · <b>{key("help")}</b> keybinds
        </span>
      )}
      {mode === "navigate" && (
        <>
          <span className="mode-hints">
            <b>↑↓</b>/<b>j k</b> space · <b>enter</b> open · <b>h l</b> pane · <b>esc</b> done
          </span>
          {/* The highlight lives here while the rail is being rebuilt; the rail
              can read `usePrefixMode(s => s.navTarget)` to draw it in place. */}
          <span className="mode-hints">
            {order.map((id) => {
              const p = projects.find((x) => x.id === id);
              return (
                <span key={id} className="mode-space" data-on={id === navTarget}>
                  {p?.label ?? id}
                </span>
              );
            })}
          </span>
        </>
      )}
      {mode === "resize" && (
        <span className="mode-hints">
          <b>h j k l</b>/<b>arrows</b> move the divider 5% · <b>enter</b>/<b>esc</b> done
        </span>
      )}
      {toast && <span className="mode-toast">{toast}</span>}
    </div>
  );
}

type Row = { group: string; keys: string; label: string };

function helpRows(cfg: PrefixConfig): Row[] {
  const pre = cfg.prefixes.map(tokenLabel).join(" / ");
  const rows: Row[] = [
    { group: "Prefix", keys: pre, label: "Enter prefix mode — the next key is a command" },
    { group: "Prefix", keys: `${pre} ${tokenLabel(cfg.prefixes[0] ?? "")}`, label: "Send the prefix to the terminal" },
    { group: "Prefix", keys: `${pre} esc`, label: "Cancel" },
  ];
  for (const a of PREFIX_ACTIONS) {
    rows.push({ group: `Prefix · ${a.group}`, keys: `${pre} ${tokenLabel(cfg.bindings[a.id] ?? "")}`, label: a.label });
  }
  rows.push(
    { group: "Prefix · Tabs", keys: `${pre} 1…9`, label: "Tab by position" },
    { group: "Prefix · Panes", keys: `${pre} ← ↓ ↑ →`, label: "Focus pane (same as h j k l)" },
    { group: "Navigate mode", keys: "↑ ↓ / j k", label: "Move the highlight between spaces" },
    { group: "Navigate mode", keys: "enter", label: "Open the highlighted space" },
    { group: "Navigate mode", keys: "h l", label: "Focus pane left / right" },
    { group: "Navigate mode", keys: "esc", label: "Leave" },
    { group: "Resize mode", keys: "h j k l / arrows", label: "Move the divider 5% that way" },
    { group: "Resize mode", keys: "enter / esc", label: "Leave" },
  );
  const bindings = readBindings();
  for (const a of ACTIONS) rows.push({ group: "Shortcuts", keys: chordLabel(bindings[a.id] ?? ""), label: a.label });
  rows.push(
    { group: "Shortcuts", keys: chordLabel("Mod+N"), label: "Connect a server" },
    { group: "Shortcuts", keys: chordLabel("Mod+1") + "…9", label: "Tab by position" },
  );
  return rows;
}

/**
 * `?` — every binding in one list, filterable with `/`.
 *
 * Bounded twice, per the list-ceiling rule: the dialog has its own `max-h` and
 * `overflow-y-auto`, and the list — the element that actually scrolls — has a
 * `max-height` of its own. The count is ~60 rows today and grows with every
 * action added; a cap on a wrapper alone is the bug that carried a CANCEL
 * button off the screen once already.
 */
export function PrefixHelp() {
  const open = usePrefixMode((s) => s.helpOpen);
  const [filter, setFilter] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const dialog = useRef<HTMLDivElement>(null);
  const rows = useMemo(() => (open ? helpRows(prefixConfig()) : []), [open]);

  useEffect(() => {
    if (!open) return;
    setFilter("");
    dialog.current?.focus();
    const close = () => usePrefixMode.setState({ helpOpen: false });
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (document.activeElement === input.current && input.current?.value) setFilter("");
        else close();
        return;
      }
      if (e.key === "/" && document.activeElement !== input.current) {
        e.preventDefault();
        input.current?.focus();
      }
    };
    // Capture: the overlay is modal, and a terminal under it must not get the
    // `/` or the Escape that closes it.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  if (!open) return null;
  const q = filter.trim().toLowerCase();
  const shown = q ? rows.filter((r) => `${r.group} ${r.keys} ${r.label}`.toLowerCase().includes(q)) : rows;
  let lastGroup = "";

  return (
    <div
      className="fixed inset-0 z-[97] grid place-items-start justify-center pt-[8vh]"
      style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
      onMouseDown={() => usePrefixMode.setState({ helpOpen: false })}
    >
      <div
        ref={dialog}
        tabIndex={-1}
        role="dialog"
        aria-label="Keybinds"
        className="window flex max-h-[84vh] w-[min(620px,94vw)] flex-col gap-3 overflow-y-auto p-4 outline-none"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="flex items-baseline justify-between gap-3">
          <span className="kicker">KEYBINDS</span>
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            / filter · esc close
          </span>
        </div>
        <input
          ref={input}
          className="data w-full border px-2 py-1 text-[12px]"
          style={{ borderColor: "var(--border)", background: "transparent", color: "var(--text)" }}
          placeholder="filter — press /"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <ul className="data flex max-h-[62vh] flex-col overflow-y-auto text-[11px]">
          {shown.map((r, i) => {
            const head = r.group !== lastGroup;
            lastGroup = r.group;
            return (
              <Fragment key={`${r.group}-${i}`}>
                {head && (
                  <li className="micro pb-1 pt-3" style={{ color: "var(--text-faint)" }}>
                    {r.group.toUpperCase()}
                  </li>
                )}
                <li className="flex items-baseline gap-3 py-[2px]">
                  <span className="w-[180px] shrink-0 truncate" style={{ color: "var(--text)" }}>
                    {r.keys}
                  </span>
                  <span className="min-w-0 truncate" style={{ color: "var(--text-soft)" }}>
                    {r.label}
                  </span>
                </li>
              </Fragment>
            );
          })}
          {!shown.length && (
            <li className="micro py-2" style={{ color: "var(--text-faint)" }}>
              nothing matches “{filter}”
            </li>
          )}
        </ul>
      </div>
    </div>
  );
}

/**
 * The prompts the prefix table opens: rename (project, tab, pane) and the
 * close-project confirmation.
 *
 * Close-project says **what ends** — `ENDS 3 SESSIONS ON THE SERVER` and the
 * names — the same sentence the rail's ✕ uses, and then calls the same
 * `removeProject` the rail does, so the keyboard path cannot end more or less
 * than the mouse path. Cancel holds the focus: a reflexive Enter keeps the work.
 */
export function PrefixDialogs() {
  const dialog = usePrefixMode((s) => s.dialog);
  const sessions = useWorkspace((s) => s.sessions);
  const projects = useWorkspace((s) => s.projects);
  const serverOf = useWorkspace((s) => s.serverOf);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const close = () => usePrefixMode.setState({ dialog: null });

  useEffect(() => {
    if (!dialog) return;
    if (dialog.kind === "rename") inputRef.current?.select();
    else cancelRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [dialog]);

  if (!dialog) return null;

  const shell = (children: ReactNode, aria: string) => (
    <div
      className="fixed inset-0 z-[96] grid place-items-start justify-center pt-[14vh]"
      style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
      onMouseDown={close}
    >
      <div
        role="dialog"
        aria-label={aria}
        className="window flex max-h-[70vh] w-[min(440px,92vw)] flex-col gap-3 overflow-y-auto p-4"
        onMouseDown={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );

  if (dialog.kind === "rename") {
    const submit = (value: string) => {
      const s = useWorkspace.getState();
      if (dialog.what === "tab") s.renameTab(dialog.projectId, dialog.id, value);
      else if (dialog.what === "project") s.renameProject(dialog.id, value);
      else s.renameSession(dialog.id, value);
      close();
    };
    return shell(
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          submit(inputRef.current?.value ?? "");
        }}
      >
        <span className="micro" style={{ color: "var(--text)" }}>
          RENAME {dialog.what.toUpperCase()}
        </span>
        <input
          ref={inputRef}
          autoFocus
          defaultValue={dialog.value}
          placeholder={dialog.what === "tab" ? "empty: named after its session" : ""}
          className="data w-full border px-2 py-1 text-[12px]"
          style={{ borderColor: "var(--border)", background: "transparent", color: "var(--text)" }}
        />
        <div className="flex justify-end gap-2">
          <button type="button" className="chip" onClick={close}>
            cancel
          </button>
          <button type="submit" className="chip">
            rename
          </button>
        </div>
      </form>,
      `Rename ${dialog.what}`,
    );
  }

  const project = projects.find((p) => p.id === dialog.id);
  const mine = sessions.filter((s) => s.projectId === dialog.id);
  if (!project) return null;
  const host = serverOf(dialog.id)?.target.host ?? "this machine";
  const n = mine.length;
  return shell(
    <>
      <span className="micro" style={{ color: "var(--text)" }}>
        {`CLOSE ${project.label.toUpperCase()} · ENDS ${n} SESSION${n === 1 ? "" : "S"} ON ${host.toUpperCase()}`}
      </span>
      <ul className="data flex max-h-[30vh] flex-col gap-1 overflow-y-auto text-[11px]" style={{ color: "var(--text-soft)" }}>
        {mine.map((s) => (
          <li key={s.id} className="truncate">
            {s.kind === "terminal" ? "shell" : s.kind} · {s.name}
          </li>
        ))}
      </ul>
      <span className="data text-[11px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
        Every shell and conversation in this project is stopped on the host. To keep them running,
        detach the sessions from the rail's right-click menu instead.
      </span>
      <div className="flex justify-end gap-2">
        <button ref={cancelRef} type="button" className="chip" onClick={close}>
          cancel
        </button>
        <button
          type="button"
          className="chip"
          style={{ color: "rgb(var(--primary))" }}
          onClick={() => {
            close();
            useWorkspace.getState().removeProject(dialog.id);
          }}
        >
          {n === 1 ? "end it" : `end all ${n}`}
        </button>
      </div>
    </>,
    "Close project",
  );
}
