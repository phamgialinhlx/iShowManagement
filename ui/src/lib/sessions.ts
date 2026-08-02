import { create } from "zustand";

import { invoke } from "@tauri-apps/api/core";

import { api, isTauri, type FileContent, type TargetRef } from "./api";

/**
 * Coding sessions — the thing rmux is actually for.
 *
 * A session is **a place you are working**: a target (this machine, or an SSH
 * host) plus a project folder. It owns its own open files, terminals and Claude
 * session, and it keeps them for as long as it exists.
 *
 * This is the model the whole app is built around, and getting it wrong is what
 * made the first layout useless. An IDE bound to one host answers "what is in
 * this folder". The question that actually matters when you are running work
 * across several machines is **"which of my sessions needs me right now?"** —
 * so sessions are first-class, always listed, always showing status, and
 * switching between them is instant because nothing is ever torn down.
 */

export type SessionStatus =
  /** Claude is asking something. The operator must act. */
  | "waiting"
  /** Claude is working. Nothing to do. */
  | "working"
  /** No Claude session, or it is idle. */
  | "idle";

export type Session = {
  id: string;
  /**
   * Shown in the rail.
   *
   * A session *is* a Claude conversation, so this is the conversation's name —
   * Claude's own title when it has one, the folder's basename until then. Two
   * sessions in the same folder are normal and expected; that is the whole point
   * of being able to switch between them.
   */
  name: string;
  /**
   * True once the operator has typed a name.
   *
   * Claude keeps retitling a conversation as it goes, and silently overwriting a
   * name someone chose deliberately is worse than showing a stale one — so
   * auto-adoption stops permanently at the first manual rename.
   */
  renamed?: boolean;
  target: TargetRef;
  /** The project root. The file tree opens here and terminals start here. */
  folder: string;
  /** Claude conversation to resume on first open; undefined starts a new one. */
  resume?: string;
  /**
   * Run Claude in its fullscreen TUI.
   *
   * Off by default, and deliberately so: fullscreen takes the mouse, which stops
   * text being selectable and turns every scroll into a network round trip. See
   * `Rendering` in `crates/rmux-claude`.
   */
  fullscreen?: boolean;
  /**
   * The Claude credential this session runs as, if it should not use the
   * default one.
   *
   * Per-session because one machine routinely works across accounts — a
   * personal subscription on your own projects, the org's Console key on the
   * team's. A single global credential would silently bill the wrong one.
   */
  claudeAccount?: string;
  /**
   * The Jira project key (e.g. `RMX`) whose issues this session manages.
   *
   * Its presence is what makes the Jira tab appear: a tab that is always there
   * but empty for most sessions is a tab everyone learns to ignore.
   */
  jiraProject?: string;
  /**
   * The Claude context window, in tokens.
   *
   * Set by hand because nothing reports it: the transcript records the model
   * name, and the same model ships in 200k and 1M variants.
   */
  contextWindow?: number;
  status: SessionStatus;
  /** Set when the target could not be reached, so the rail can show it. */
  error: string | null;
};

export type BufferKey = string;

export type Buffer = {
  key: BufferKey;
  sessionId: string;
  path: string;
  saved: string;
  text: string;
  content: FileContent | null;
  loading: boolean;
  error: string | null;
  saving: boolean;
  saveError: string | null;
};

/**
 * Buffers are keyed by **session**, not by path.
 *
 * Two sessions routinely have a file at the same path — `src/main.rs` in two
 * checkouts, `/etc/nginx.conf` on two servers. Keying by path alone would let an
 * edit made in one session be saved into another. That is data loss, not a
 * cosmetic bug.
 */
export const bufferKey = (sessionId: string, path: string): BufferKey =>
  `${sessionId}${path}`;

export const isDirty = (b: Buffer): boolean =>
  b.content?.kind === "text" && b.text !== b.saved;

export type TerminalTab = {
  id: string;
  sessionId: string;
  title: string;
  /**
   * The PTY this tab is bound to, once opened.
   *
   * Held in the store rather than in component state so a remount **reattaches**
   * to the running shell instead of spawning a new one. Without it, every
   * remount — including React's development double-mount — kills the shell and
   * starts another, which reads as the terminal flashing and resetting.
   */
  ptyId?: string;
};

type State = {
  sessions: Session[];
  activeSession: string | null;
  railCollapsed: boolean;

  buffers: Record<BufferKey, Buffer>;
  /** Tab order, per session. */
  openOrder: Record<string, BufferKey[]>;
  activeBuffer: Record<string, BufferKey | null>;

  terminals: TerminalTab[];
  activeTerminal: Record<string, string | null>;

  /**
   * Live Claude PTY handles, per coding session.
   *
   * **Runtime only — never persisted.** Like `TerminalTab.ptyId`, this names an
   * attachment inside *this* run of the app; after a restart it refers to
   * nothing, and asking to reattach to it gives "no such session". What survives
   * a restart is the agent session name derived from the coding session's id,
   * which `ClaudePanel` reconstructs.
   */
  claudeSessions: Record<string, string>;

  addSession: (target: TargetRef, folder: string, name?: string, resume?: string) => Promise<string>;
  removeSession: (id: string) => void;
  activate: (id: string) => void;
  renameSession: (id: string, name: string) => void;
  /** Switch a session between inline and fullscreen Claude rendering. */
  setFullscreen: (id: string, fullscreen: boolean) => void;
  /** Pin the conversation a session resumes, so a restart keeps its context. */
  setResume: (id: string, resume: string) => void;
  /** 1 = focus one session; 2/3/4 = an NxN grid of them. */
  grid: number;
  setGrid: (n: number) => void;
  /**
   * Which session is in which grid cell, by index.
   *
   * Sparse and explicit. Deriving the grid from list order — the first N
   * sessions — meant that with eight sessions in a 2x2 the other four were
   * simply unreachable, and there was no way to say "that host, in that cell".
   * A cell left `null` is filled automatically, so the grid still works before
   * anyone has arranged anything.
   */
  gridSlots: (string | null)[];
  /** Put a session in a cell, or `null` to hand the cell back to auto-fill. */
  assignSlot: (index: number, sessionId: string | null) => void;
  /**
   * The grid cell the operator has selected, if any.
   *
   * This is what makes "click a cell, then pick a session in the rail" work,
   * which is the only interaction that answers "I have eight sessions and four
   * cells". Runtime only — a selection is a thing you are doing right now, not
   * a property of the workspace, and restoring one on launch would leave the
   * next rail click silently replacing a pane.
   */
  focusedCell: number | null;
  focusCell: (index: number | null) => void;
  /** Update the per-session settings. Absent fields are left alone. */
  configureSession: (
    id: string,
    settings: { claudeAccount?: string; jiraProject?: string; contextWindow?: number },
  ) => void;
  /** Adopt Claude's own conversation title, unless the name was set by hand. */
  adoptClaudeTitle: (id: string, title: string) => void;
  setStatus: (id: string, status: SessionStatus) => void;
  setSessionError: (id: string, error: string | null) => void;
  toggleRail: () => void;

  openFile: (sessionId: string, path: string) => Promise<void>;
  closeBuffer: (key: BufferKey) => void;
  activateBuffer: (sessionId: string, key: BufferKey) => void;
  edit: (key: BufferKey, text: string) => void;
  save: (key: BufferKey) => Promise<void>;

  addTerminal: (sessionId: string) => void;
  closeTerminal: (id: string) => void;
  activateTerminal: (sessionId: string, id: string) => void;
  renameTerminal: (id: string, title: string) => void;
  /** Reopen the files this session had open when the app last closed. */
  restoreFiles: (sessionId: string) => void;

  setTerminalPty: (terminalId: string, ptyId: string) => void;
  setClaudeSession: (sessionId: string, claudeId: string) => void;
  /** Forget a Claude handle that no longer resolves. */
  clearClaudeSession: (sessionId: string) => void;
};

/**
 * Bumped from v1: the stored shape now covers the whole workspace, not just the
 * session list. An old v1 blob is ignored rather than migrated — it holds no
 * terminal identity, so there would be nothing to reattach to anyway.
 */
const STORAGE_KEY = "rmux.workspace.v2";

type Persisted = {
  sessions: Omit<Session, "status" | "error">[];
  activeSession: string | null;
  /** Terminal tabs, with the ids that name their shells on the far end. */
  terminals: Omit<TerminalTab, "ptyId">[];
  activeTerminal: Record<string, string | null>;
  /** Open file paths per session. Contents are re-read; edits are not persisted. */
  openPaths: Record<string, string[]>;
  activePath: Record<string, string | null>;
  /** Which session sits in which grid cell. See `State.gridSlots`. */
  gridSlots: (string | null)[];
};

const EMPTY: Persisted = {
  sessions: [],
  activeSession: null,
  terminals: [],
  activeTerminal: {},
  openPaths: {},
  activePath: {},
  gridSlots: [],
};

function load(): Persisted {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return EMPTY;
    // Spread over the defaults: a blob written by an older build is missing
    // whichever keys were added since, and `undefined.map` is a blank app.
    return { ...EMPTY, ...(JSON.parse(raw) as Partial<Persisted>) };
  } catch {
    return EMPTY;
  }
}

/**
 * Snapshot the workspace.
 *
 * **`ptyId` is deliberately not stored.** It identifies a live attachment inside
 * *this* run of the app; after a restart it refers to nothing. What survives is
 * the terminal's own `id`, which is what names the shell on the target — so a
 * reopened tab reattaches by name rather than by a handle that died with the
 * process that made it.
 */
function persist(state: State) {
  try {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        // Status is derived at runtime; persisting it would show a stale
        // "waiting" badge for a session whose Claude is long gone.
        sessions: state.sessions.map(
          ({
            id,
            name,
            renamed,
            target,
            folder,
            resume,
            fullscreen,
            claudeAccount,
            jiraProject,
            contextWindow,
          }) => ({
            id,
            name,
            renamed,
            target,
            folder,
            resume,
            fullscreen,
            // Per-session settings are persisted state, not runtime handles:
            // this list is explicit, so anything added to `Session` and not
            // added here is silently forgotten on the next start.
            claudeAccount,
            jiraProject,
            contextWindow,
          }),
        ),
        activeSession: state.activeSession,
        // The grid arrangement is something the operator built, so it outlives
        // the run that built it.
        gridSlots: state.gridSlots,
        terminals: state.terminals.map(({ id, sessionId, title }) => ({ id, sessionId, title })),
        activeTerminal: state.activeTerminal,
        openPaths: Object.fromEntries(
          Object.entries(state.openOrder).map(([sessionId, keys]) => [
            sessionId,
            (keys ?? []).map((k) => state.buffers[k]?.path).filter((p): p is string => !!p),
          ]),
        ),
        activePath: Object.fromEntries(
          Object.entries(state.activeBuffer).map(([sessionId, key]) => [
            sessionId,
            key ? (state.buffers[key]?.path ?? null) : null,
          ]),
        ),
      } satisfies Persisted),
    );
  } catch {
    // A full or disabled localStorage must never stop the app working.
  }
}

/**
 * Persist on the next microtask instead of immediately.
 *
 * Several actions mutate in sequence — creating a session also opens a terminal
 * — and each would otherwise serialise and stringify the entire workspace. This
 * coalesces a burst into one write, and always snapshots the *final* state
 * rather than an intermediate one.
 */
let pending = false;
function schedulePersist(read: () => State) {
  if (pending) return;
  pending = true;
  queueMicrotask(() => {
    pending = false;
    persist(read());
  });
}

/** The folder's last component — what a session is called by default. */
export const basename = (path: string): string =>
  path.replace(/\/+$/, "").split("/").filter(Boolean).pop() ?? path;

let seq = 0;
const nextId = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${++seq}`;

const restored = load();

export const useSessions = create<State>((set, get) => ({
  sessions: restored.sessions.map((s) => ({ ...s, status: "idle", error: null })),
  activeSession: restored.activeSession,
  railCollapsed: false,

  // Buffers are rebuilt lazily by `restoreFiles` — their contents come from the
  // target, and a stale copy of a file someone else has since changed is worse
  // than a brief load.
  buffers: {},
  openOrder: {},
  activeBuffer: {},
  // Terminals come back **without** `ptyId`, so mounting one reattaches to the
  // shell still running under that terminal's id on the target.
  terminals: restored.terminals,
  activeTerminal: restored.activeTerminal,
  // Deliberately empty on boot — see the field's note. Reattaching happens by
  // name, through the agent, not through a handle from a dead process.
  claudeSessions: {},

  addSession: async (target, folder, name, resume) => {
    const id = nextId("session");
    const session: Session = {
      id,
      name: name?.trim() || basename(folder) || (target.host ?? "local"),
      target,
      folder,
      resume,
      status: "idle",
      error: null,
    };

    set((s) => ({ sessions: [...s.sessions, session], activeSession: id }));
    schedulePersist(get);

    // Open a terminal so the session is immediately usable rather than an empty
    // shell you have to set up before it does anything.
    get().addTerminal(id);
    return id;
  },

  /**
   * Close a session, and end the work running under it.
   *
   * **The kills are the point.** A session's shells and its Claude do not run
   * inside rmux — they run under `rmux-agent` on the target, precisely so they
   * survive quitting the app and losing the network. That is what makes closing
   * one different from closing a window: dropping it from this list alone left
   * every shell and the conversation running on the host with nothing able to
   * reach them again, and no way to find them short of `ps` on the server.
   *
   * Best effort and never awaited. The session is gone from the operator's
   * point of view the moment they confirm; an unreachable host must not leave a
   * row they cannot remove.
   */
  removeSession: (id) => {
    const state = get();
    const session = state.sessions.find((x) => x.id === id);
    if (session && isTauri()) {
      for (const terminal of state.terminals.filter((t) => t.sessionId === id)) {
        void invoke("terminal_close", {
          id: terminal.ptyId ?? "",
          target: session.target,
          session: terminal.id,
        }).catch(() => {});
      }
      // The agent session name Claude runs under, reconstructed the same way
      // `ClaudePanel` does — it is derived from the session id rather than
      // stored, so it survives a restart.
      void api.claudeEndSession(session.target, `claude-${id}`).catch(() => {});
    }

    set((s) => {
      const sessions = s.sessions.filter((x) => x.id !== id);
      const activeSession =
        s.activeSession === id ? (sessions[sessions.length - 1]?.id ?? null) : s.activeSession;

      // Drop everything belonging to the session, or its buffers and terminals
      // leak for the life of the app.
      const buffers = Object.fromEntries(
        Object.entries(s.buffers).filter(([, b]) => b.sessionId !== id),
      );
      const { [id]: _order, ...openOrder } = s.openOrder;
      const { [id]: _active, ...activeBuffer } = s.activeBuffer;
      const { [id]: _claude, ...claudeSessions } = s.claudeSessions;

      queueMicrotask(() => schedulePersist(get));
      return {
        sessions,
        activeSession,
        buffers,
        openOrder,
        activeBuffer,
        claudeSessions,
        terminals: s.terminals.filter((t) => t.sessionId !== id),
        // A slot pointing at a closed session would hold a cell empty forever
        // rather than letting auto-fill use it.
        gridSlots: s.gridSlots.map((slot) => (slot === id ? null : slot)),
      };
    });
  },

  activate: (id) => {
    set({ activeSession: id });
    schedulePersist(get);
  },

  renameSession: (id, name) => {
    set((s) => ({
      sessions: s.sessions.map((x) =>
        // `renamed` latches: from here on Claude's own title is ignored for this
        // session, because a name someone typed outranks one a model generated.
        x.id === id ? { ...x, name: name.trim() || x.name, renamed: true } : x,
      ),
    }));
    schedulePersist(get);
  },

  /**
   * Take Claude's title for a session that has not been named by hand.
   *
   * Claude retitles a conversation as it learns what the work is, so this runs
   * repeatedly and must be idempotent — and must never fire once `renamed` is
   * set, or it would silently undo a deliberate rename.
   */
  adoptClaudeTitle: (id, title) => {
    const clean = title.trim();
    if (!clean) return;
    const current = get().sessions.find((x) => x.id === id);
    if (!current || current.renamed || current.name === clean) return;

    set((s) => ({
      sessions: s.sessions.map((x) => (x.id === id ? { ...x, name: clean } : x)),
    }));
    schedulePersist(get);
  },

  setFullscreen: (id, fullscreen) => {
    set((s) => ({
      sessions: s.sessions.map((x) => (x.id === id ? { ...x, fullscreen } : x)),
    }));
    schedulePersist(get);
  },

  grid: Number(localStorage.getItem("rmux.grid")) || 1,
  setGrid: (n) => {
    localStorage.setItem("rmux.grid", String(n));
    // Leaving a cell selected across a layout change would point at a cell that
    // may no longer exist — and the next rail click would assign into nothing.
    set({ grid: n, focusedCell: null });
  },

  gridSlots: restored.gridSlots,
  focusedCell: null,
  focusCell: (index) => set({ focusedCell: index }),
  assignSlot: (index, sessionId) => {
    set((s) => {
      const slots = [...s.gridSlots];
      while (slots.length <= index) slots.push(null);

      // A session may only be in one cell. Two cells holding the same session
      // would mount its view twice, which means two Claude panes attaching to
      // one conversation — so putting it here takes it out of wherever it was.
      if (sessionId) {
        for (let i = 0; i < slots.length; i += 1) {
          if (slots[i] === sessionId) slots[i] = null;
        }
      }
      slots[index] = sessionId;
      return { gridSlots: slots };
    });
    schedulePersist(get);
  },

  configureSession: (id, settings) => {
    set((state) => ({
      sessions: state.sessions.map((session) =>
        session.id === id
          ? {
              ...session,
              // An empty string means "clear it", which is different from
              // "leave it alone" — so undefined and "" must not collapse.
              claudeAccount:
                settings.claudeAccount === undefined
                  ? session.claudeAccount
                  : settings.claudeAccount || undefined,
              jiraProject:
                settings.jiraProject === undefined
                  ? session.jiraProject
                  : settings.jiraProject || undefined,
              contextWindow:
                settings.contextWindow === undefined
                  ? session.contextWindow
                  : settings.contextWindow || undefined,
            }
          : session,
      ),
    }));
    // These are persisted state, not runtime handles — a Claude account and a
    // Jira project chosen once must survive a restart. This was missing, so
    // every setting silently reverted the next time the app opened.
    schedulePersist(get);
  },

  setResume: (id, resume) => {
    if (!resume) return;
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === id ? { ...x, resume } : x)) }));
    schedulePersist(get);
  },

  setStatus: (id, status) =>
    set((s) => {
      const current = s.sessions.find((x) => x.id === id);
      // Avoid a state update on every poll when nothing changed — the rail
      // re-renders otherwise, several times a second, forever.
      if (!current || current.status === status) return s;
      return { sessions: s.sessions.map((x) => (x.id === id ? { ...x, status } : x)) };
    }),

  setSessionError: (id, error) =>
    set((s) => {
      const current = s.sessions.find((x) => x.id === id);
      if (!current || current.error === error) return s;
      return { sessions: s.sessions.map((x) => (x.id === id ? { ...x, error } : x)) };
    }),

  toggleRail: () => set((s) => ({ railCollapsed: !s.railCollapsed })),

  openFile: async (sessionId, path) => {
    const key = bufferKey(sessionId, path);
    const session = get().sessions.find((s) => s.id === sessionId);
    if (!session) return;

    if (get().buffers[key]) {
      set((s) => ({
        activeBuffer: { ...s.activeBuffer, [sessionId]: key },
        openOrder: {
          ...s.openOrder,
          [sessionId]: s.openOrder[sessionId]?.includes(key)
            ? s.openOrder[sessionId]!
            : [...(s.openOrder[sessionId] ?? []), key],
        },
      }));
      return;
    }

    set((s) => ({
      buffers: {
        ...s.buffers,
        [key]: {
          key,
          sessionId,
          path,
          saved: "",
          text: "",
          content: null,
          loading: true,
          error: null,
          saving: false,
          saveError: null,
        },
      },
      openOrder: { ...s.openOrder, [sessionId]: [...(s.openOrder[sessionId] ?? []), key] },
      activeBuffer: { ...s.activeBuffer, [sessionId]: key },
    }));
    schedulePersist(get);

    try {
      const content = await api.fsRead(session.target, path);
      set((s) => {
        const b = s.buffers[key];
        if (!b) return s;
        const text = content.kind === "text" ? content.text : "";
        return { buffers: { ...s.buffers, [key]: { ...b, content, text, saved: text, loading: false } } };
      });
    } catch (e) {
      set((s) => {
        const b = s.buffers[key];
        if (!b) return s;
        return {
          buffers: {
            ...s.buffers,
            [key]: { ...b, loading: false, error: e instanceof Error ? e.message : String(e) },
          },
        };
      });
    }
  },

  closeBuffer: (key) =>
    set((s) => {
      const buffer = s.buffers[key];
      if (!buffer) return s;
      const sessionId = buffer.sessionId;

      const { [key]: _dropped, ...buffers } = s.buffers;
      const order = (s.openOrder[sessionId] ?? []).filter((k) => k !== key);
      const active =
        s.activeBuffer[sessionId] === key ? (order[order.length - 1] ?? null) : s.activeBuffer[sessionId] ?? null;

      queueMicrotask(() => schedulePersist(get));
      return {
        buffers,
        openOrder: { ...s.openOrder, [sessionId]: order },
        activeBuffer: { ...s.activeBuffer, [sessionId]: active },
      };
    }),

  activateBuffer: (sessionId, key) => {
    set((s) => ({ activeBuffer: { ...s.activeBuffer, [sessionId]: key } }));
    schedulePersist(get);
  },

  edit: (key, text) =>
    set((s) => {
      const b = s.buffers[key];
      if (!b) return s;
      return { buffers: { ...s.buffers, [key]: { ...b, text } } };
    }),

  save: async (key) => {
    const buffer = get().buffers[key];
    if (!buffer || buffer.saving || buffer.content?.kind !== "text") return;

    const session = get().sessions.find((s) => s.id === buffer.sessionId);
    if (!session) return;

    set((s) => ({ buffers: { ...s.buffers, [key]: { ...s.buffers[key]!, saving: true, saveError: null } } }));

    // Captured before the await: the user may keep typing during the write, and
    // marking the newer text as saved would hide those edits.
    const written = buffer.text;

    try {
      await api.fsWrite(session.target, buffer.path, written);
      set((s) => {
        const cur = s.buffers[key];
        if (!cur) return s;
        return { buffers: { ...s.buffers, [key]: { ...cur, saved: written, saving: false } } };
      });
    } catch (e) {
      set((s) => {
        const cur = s.buffers[key];
        if (!cur) return s;
        return {
          buffers: {
            ...s.buffers,
            [key]: { ...cur, saving: false, saveError: e instanceof Error ? e.message : String(e) },
          },
        };
      });
    }
  },

  addTerminal: (sessionId) => {
    const id = nextId("term");
    const count = get().terminals.filter((t) => t.sessionId === sessionId).length + 1;
    set((s) => ({
      terminals: [...s.terminals, { id, sessionId, title: `sh ${count}` }],
      activeTerminal: { ...s.activeTerminal, [sessionId]: id },
    }));
    schedulePersist(get);
  },

  renameTerminal: (id, title) => {
    const clean = title.trim();
    if (!clean) return;
    set((s) => ({
      terminals: s.terminals.map((t) => (t.id === id ? { ...t, title: clean } : t)),
    }));
    schedulePersist(get);
  },

  /**
   * Reopen the files a session had open last time.
   *
   * Lazy — run when the session is first shown, not at boot. Restoring every
   * session's files at startup would fire a read per file per host, on
   * connections that may not even be up yet, to populate tabs the operator may
   * never look at.
   */
  restoreFiles: (sessionId) => {
    const paths = restored.openPaths[sessionId];
    if (!paths?.length) return;
    // Once consumed, forget it: otherwise closing a tab and revisiting the
    // session would resurrect the file the operator just closed.
    delete restored.openPaths[sessionId];

    const activePath = restored.activePath[sessionId];
    delete restored.activePath[sessionId];

    void (async () => {
      for (const path of paths) {
        await get().openFile(sessionId, path);
      }
      if (activePath) {
        get().activateBuffer(sessionId, bufferKey(sessionId, activePath));
      }
    })();
  },

  closeTerminal: (id) =>
    set((s) => {
      const terminal = s.terminals.find((t) => t.id === id);
      if (!terminal) return s;
      const remaining = s.terminals.filter((t) => t.id !== id);
      const siblings = remaining.filter((t) => t.sessionId === terminal.sessionId);
      queueMicrotask(() => schedulePersist(get));
      return {
        terminals: remaining,
        activeTerminal: {
          ...s.activeTerminal,
          [terminal.sessionId]:
            s.activeTerminal[terminal.sessionId] === id
              ? (siblings[siblings.length - 1]?.id ?? null)
              : s.activeTerminal[terminal.sessionId] ?? null,
        },
      };
    }),

  activateTerminal: (sessionId, id) => {
    set((s) => ({ activeTerminal: { ...s.activeTerminal, [sessionId]: id } }));
    schedulePersist(get);
  },

  setTerminalPty: (terminalId, ptyId) =>
    set((s) => ({
      terminals: s.terminals.map((t) => (t.id === terminalId ? { ...t, ptyId } : t)),
    })),

  setClaudeSession: (sessionId, claudeId) => {
    set((s) => ({ claudeSessions: { ...s.claudeSessions, [sessionId]: claudeId } }));
  },

  clearClaudeSession: (sessionId) =>
    set((s) => {
      const { [sessionId]: _gone, ...claudeSessions } = s.claudeSessions;
      return { claudeSessions };
    }),
}));

// Dev-only handle so the workbench can be driven without the Tauri IPC bridge.
if (import.meta.env.DEV) {
  (window as unknown as Record<string, unknown>).__rmuxSessions = useSessions;
}
