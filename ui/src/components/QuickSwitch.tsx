import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { GLYPHS, STATE_COLOR, STATE_LABEL, agentState, rollup } from "../lib/agent-state";
import { useIndicatorStyle } from "../lib/agent-indicators";
import { useBranches } from "../lib/git-branch";
import { locateSession, tabLabel, tabsOf } from "../lib/layout-tree";
import {
  QUICK_SWITCH_EVENT,
  parseQuery,
  rank,
  segments,
  touchMru,
  type Ranked,
  type SwitchItem,
} from "../lib/quick-switch";
import { isMac } from "../lib/shortcuts";
import { useWorkspace } from "../lib/workspace";
import type { Server } from "../lib/workspace-model";

/**
 * The quick switcher — ⌘K (Ctrl+Shift+G off macOS), or herdr's prefix `g`.
 *
 * One field, every session on every server plus every project below it, Enter
 * jumps. Ranking is `quick-switch.ts`; this file only builds the rows from the
 * store and draws them.
 *
 * - **Split in two so a closed switcher costs nothing.** The outer component
 *   only listens for the open event and keeps the recency list; the dialog,
 *   which subscribes to `runtime` (written on every status change), is mounted
 *   only while open.
 * - **Monaco keeps ⌘K.** `useShortcuts` skips a chord whose target is a text
 *   field, and Monaco's input is a textarea outside `.xterm` — so with the
 *   editor focused ⌘K stays Monaco's chord prefix (⌘K ⌘C and friends). From a
 *   terminal it opens this: xterm is exempt from that rule, and ⌘ never reaches
 *   the pty.
 * - **Enter during an IME composition is the IME's**, not a jump — Vietnamese
 *   commits a word with it.
 * - **The keyboard lands in the terminal after a jump.** The dialog closes
 *   first, then `activate` hands the session's terminal the cursor over two
 *   frames (`focusKeyboard`). Cancelling puts focus back where it was.
 */

const MRU_KEY = "rmux.quickSwitch.mru";

function readMru(): string[] {
  try {
    const raw = localStorage.getItem(MRU_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function writeMru(list: string[]) {
  try {
    localStorage.setItem(MRU_KEY, JSON.stringify(list));
  } catch {
    /* recency is a convenience; a full store must not break switching */
  }
}

function machineOf(server: Server | undefined): string {
  return server?.target.host ?? "local";
}

export function QuickSwitch() {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const toggle = () => setOpen((o) => !o);
    window.addEventListener(QUICK_SWITCH_EVENT, toggle);
    // Recency: every session that becomes the active one, persisted so the
    // empty query means something on the first open after a relaunch.
    let last = useWorkspace.getState().activeSession;
    if (last) writeMru(touchMru(readMru(), last));
    const unsub = useWorkspace.subscribe((s) => {
      if (!s.activeSession || s.activeSession === last) return;
      last = s.activeSession;
      const known = new Set(s.sessions.map((x) => x.id));
      writeMru(touchMru(readMru(), s.activeSession, (id) => known.has(id)));
    });
    return () => {
      window.removeEventListener(QUICK_SWITCH_EVENT, toggle);
      unsub();
    };
  }, []);

  if (!open) return null;
  return <QuickSwitchDialog onClose={() => setOpen(false)} />;
}

function useItems(): SwitchItem[] {
  const servers = useWorkspace((s) => s.servers);
  const projects = useWorkspace((s) => s.projects);
  const sessions = useWorkspace((s) => s.sessions);
  const runtime = useWorkspace((s) => s.runtime);
  const seen = useWorkspace((s) => s.seen);
  const layout = useWorkspace((s) => s.layout);
  const branches = useBranches();

  return useMemo(() => {
    const serverById = new Map(servers.map((s) => [s.id, s]));
    const nameOf = (id: string) => sessions.find((s) => s.id === id)?.name;
    const out: SwitchItem[] = [];
    const projectRows: SwitchItem[] = [];
    // Sidebar order: servers in order, each one's projects, each one's sessions.
    for (const server of servers) {
      for (const project of projects.filter((p) => p.serverId === server.id)) {
        const branch = branches[project.id]?.value?.branch;
        const mine = sessions.filter((s) => s.projectId === project.id);
        for (const session of mine) {
          const loc = locateSession(layout, session.id);
          let tab: string | undefined;
          if (loc) {
            const tabs = tabsOf(layout, loc.projectId).tabs;
            const i = tabs.findIndex((t) => t.id === loc.tabId);
            if (i >= 0) tab = tabLabel(tabs[i]!, i, nameOf);
          }
          const rt = runtime[session.id];
          out.push({
            kind: "session",
            id: session.id,
            name: session.name,
            sessionKind: session.kind,
            state: agentState(session.kind, rt, seen[session.id]),
            machine: machineOf(serverById.get(server.id)),
            project: project.label,
            // A tab named after the very session it holds says nothing twice.
            tab: tab && tab !== session.name ? tab : undefined,
            branch,
            folder: session.cwd ?? project.folder,
            lastStatusAt: rt?.lastStatusAt,
          });
        }
        projectRows.push({
          kind: "project",
          id: project.id,
          name: project.label,
          state: rollup(mine.map((s) => agentState(s.kind, runtime[s.id], seen[s.id]))),
          machine: machineOf(server),
          branch,
          folder: project.folder,
        });
      }
    }
    return [...out, ...projectRows];
  }, [servers, projects, sessions, runtime, seen, layout, branches]);
}

function QuickSwitchDialog({ onClose }: { onClose: () => void }) {
  const items = useItems();
  const indicator = useIndicatorStyle();
  const current = useWorkspace((s) => s.activeSession);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const returnFocus = useRef<Element | null>(document.activeElement);
  const mru = useMemo(readMru, []);

  const { rows, total } = useMemo(() => rank(items, query, mru, current), [items, query, mru, current]);
  const parsed = useMemo(() => parseQuery(query), [query]);
  const sel = Math.min(selected, Math.max(0, rows.length - 1));

  useEffect(() => {
    input.current?.focus();
  }, []);

  useEffect(() => setSelected(0), [query]);

  // The selected row stays in view, inside the list's own scroller.
  useLayoutEffect(() => {
    const el = list.current?.querySelector<HTMLElement>(`[data-index="${sel}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [sel, rows]);

  const cancel = () => {
    onClose();
    const back = returnFocus.current;
    if (back instanceof HTMLElement && back.isConnected) requestAnimationFrame(() => back.focus());
  };

  const choose = (row: Ranked | undefined, beside = false) => {
    if (!row) return;
    // Close first, so the input is gone before `activate` hands the terminal
    // the keyboard — otherwise the unmount would race the focus request.
    onClose();
    const s = useWorkspace.getState();
    const pid = row.item.kind === "session" ? s.projectOf(row.item.id)?.id : row.item.id;
    // ⌘↵: the project grid — the row's project in a tile beside the current
    // one (already on screen: its tile is focused), then the session in it.
    if (beside && pid) s.openBeside(pid);
    if (row.item.kind === "session") s.activate(row.item.id, { keyboard: true });
    else if (!beside) s.selectProject(row.item.id);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    // The switcher is modal: no key typed here may reach a window listener.
    // The workbench's own handler takes bare Ctrl+N / ⌘N (connect a server)
    // and Mod+digit even from a text field, so Ctrl+N — "next row" here —
    // opened the connect dialog behind the switcher until this was added.
    e.stopPropagation();
    // An IME owns Enter, arrows and Escape while it composes.
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    const mod = isMac() ? e.metaKey : e.ctrlKey;
    const down = e.key === "ArrowDown" || (e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "n");
    const up = e.key === "ArrowUp" || (e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "p");
    if (down || up) {
      e.preventDefault();
      if (rows.length) setSelected((sel + (down ? 1 : -1) + rows.length) % rows.length);
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      choose(rows[sel], mod);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      // First Escape clears a query, the second closes — like the keybinds list.
      if (query) setQuery("");
      else cancel();
      return;
    }
    // The opening chord closes it again (the field is a typing target, so the
    // global handler does not see it here).
    if (mod && e.key.toLowerCase() === "k") {
      e.preventDefault();
      cancel();
    }
  };

  const chips = [
    ...parsed.states.map((s) => `is:${s}`),
    ...parsed.hosts.map((h) => `@${h}`),
  ];

  return (
    <div
      className="fixed inset-0 z-[98] grid place-items-start justify-center pt-[12vh]"
      style={{ background: "color-mix(in srgb, var(--app-bg) 55%, transparent)" }}
      onMouseDown={cancel}
    >
      <div
        role="dialog"
        aria-label="Go to"
        className="window flex max-h-[76vh] w-[min(640px,94vw)] flex-col overflow-y-auto"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center gap-2 px-3 py-[10px]"
          style={{ borderBottom: "1px solid var(--surface1)" }}
        >
          <SearchGlyph />
          {chips.map((c) => (
            <span
              key={c}
              className="data shrink-0 px-[6px] py-[1px] text-[11px]"
              style={{ background: "var(--accent)", color: "var(--on-accent)", borderRadius: "var(--radius)" }}
            >
              {c}
            </span>
          ))}
          <input
            ref={input}
            aria-label="Go to a session or project"
            aria-controls="quick-switch-list"
            aria-activedescendant={rows[sel] ? `qs-${sel}` : undefined}
            className="data min-w-0 flex-1 bg-transparent text-[14px] outline-none"
            style={{ color: "var(--text)" }}
            placeholder="go to a session or project…"
            spellCheck={false}
            autoComplete="off"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
          />
        </div>

        <ul
          id="quick-switch-list"
          ref={list}
          role="listbox"
          className="flex max-h-[min(52vh,520px)] flex-col overflow-y-auto py-1"
        >
          {rows.map((row, i) => (
            <Row
              key={`${row.item.kind}-${row.item.id}`}
              row={row}
              index={i}
              selected={i === sel}
              current={row.item.id === current}
              indicator={indicator}
              onHover={(e) => {
                // Only a pointer that actually moved selects. Chrome fires a
                // mousemove when the list scrolls under a still pointer, so
                // without this ↓ scrolls a row under the cursor and the
                // cursor takes the selection back — the list fights the keys.
                if (e.movementX === 0 && e.movementY === 0) return;
                setSelected(i);
              }}
              onChoose={(beside) => choose(row, beside)}
            />
          ))}
          {!rows.length && (
            <li className="micro px-3 py-3" style={{ color: "var(--overlay1)" }}>
              {items.length ? `nothing matches “${query.trim()}”` : "no sessions or projects yet"}
            </li>
          )}
        </ul>

        <div
          className="micro flex items-center justify-between gap-3 px-3 py-[6px]"
          style={{ borderTop: "1px solid var(--surface1)", color: "var(--overlay1)" }}
        >
          <span>↑↓ move · ↵ open · {isMac() ? "⌘↵" : "ctrl+↵"} beside · esc close · is:blocked · @host</span>
          {total > rows.length && (
            <span>
              {rows.length} of {total} — type to narrow
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function Row({
  row,
  index,
  selected,
  current,
  indicator,
  onHover,
  onChoose,
}: {
  row: Ranked;
  index: number;
  selected: boolean;
  current: boolean;
  indicator: "dots" | "symbols";
  onHover: (e: React.MouseEvent) => void;
  onChoose: (beside: boolean) => void;
}) {
  const { item } = row;
  const secondary =
    item.kind === "session"
      ? [item.machine, item.project, item.tab].filter(Boolean).join(" · ")
      : [item.machine, item.folder].filter(Boolean).join(" · ");
  return (
    <li
      id={`qs-${index}`}
      data-index={index}
      role="option"
      aria-selected={selected}
      className="mx-1 flex cursor-default items-center gap-[10px] px-2 py-[6px]"
      style={{
        background: selected ? "var(--selection-bg)" : "transparent",
        borderRadius: "var(--radius)",
      }}
      onMouseMove={selected ? undefined : onHover}
      onMouseDown={(e) => {
        // Keep the field focused; the click acts on mouseup's click below.
        e.preventDefault();
      }}
      onClick={(e) => onChoose(isMac() ? e.metaKey : e.ctrlKey)}
    >
      <span
        className="sb-state data text-[12px]"
        style={{ color: item.state ? STATE_COLOR[item.state] : undefined }}
        title={item.state ? STATE_LABEL[item.state] : undefined}
        aria-label={item.state ? STATE_LABEL[item.state] : undefined}
      >
        {item.state ? GLYPHS[indicator][item.state] : ""}
      </span>
      <KindGlyph kind={item.kind === "project" ? "project" : (item.sessionKind ?? "terminal")} />
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="data truncate text-[13px]" style={{ color: "var(--text)" }}>
          {segments(item.name, row.ranges).map((s, i) =>
            s.hit ? (
              <span key={i} style={{ color: "var(--accent)", fontWeight: 700 }}>
                {s.text}
              </span>
            ) : (
              <span key={i}>{s.text}</span>
            ),
          )}
        </span>
        <span className="data truncate text-[11px]" style={{ color: "var(--subtext0)" }}>
          {secondary}
          {item.branch && (
            <span style={{ color: "var(--overlay1)" }}>
              {secondary ? " · " : ""}{item.branch}
            </span>
          )}
        </span>
      </div>
      {current && (
        <span className="micro shrink-0" style={{ color: "var(--overlay1)" }}>
          here
        </span>
      )}
      {item.kind === "project" && (
        <span className="micro shrink-0" style={{ color: "var(--overlay1)" }}>
          project
        </span>
      )}
    </li>
  );
}

function SearchGlyph() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--overlay1)" strokeWidth="2.2" strokeLinecap="square" aria-hidden="true" className="shrink-0">
      <circle cx="10.5" cy="10.5" r="6.5" />
      <path d="M15.5 15.5L21 21" />
    </svg>
  );
}

/** Kind glyphs — inline SVG/letters, no emoji, colours from tokens. */
function KindGlyph({ kind }: { kind: string }) {
  const common = { width: 13, height: 13, viewBox: "0 0 24 24", "aria-hidden": true, className: "shrink-0" } as const;
  if (kind === "terminal") {
    return (
      <svg {...common} fill="none" stroke="var(--overlay1)" strokeWidth="2.4" strokeLinecap="square">
        <path d="M5 7l5 5-5 5M13 17h6" />
      </svg>
    );
  }
  if (kind === "dsh") {
    return (
      <span aria-hidden="true" className="w-[13px] shrink-0 text-center text-[9px] font-bold leading-[13px]" style={{ color: "var(--subtext0)" }}>
        DS
      </span>
    );
  }
  if (kind === "pi") {
    return (
      <span aria-hidden="true" className="w-[13px] shrink-0 text-center text-[13px] font-bold leading-[13px]" style={{ color: "var(--subtext0)" }}>
        π
      </span>
    );
  }
  if (kind === "codex") {
    return (
      <span aria-hidden="true" className="w-[13px] shrink-0 text-center text-[10px] font-bold leading-[13px]" style={{ color: "var(--subtext0)", letterSpacing: "-0.06em" }}>
        {">_"}
      </span>
    );
  }
  if (kind === "project") {
    return (
      <svg {...common} fill="none" stroke="var(--overlay1)" strokeWidth="2" strokeLinecap="square">
        <path d="M3 6h7l2 2h9v11H3z" />
      </svg>
    );
  }
  // Claude: an eight-ray spark, drawn rather than fetched.
  return (
    <svg {...common} fill="none" stroke="var(--subtext0)" strokeWidth="2.4" strokeLinecap="square">
      <path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6L5.6 18.4" />
    </svg>
  );
}
