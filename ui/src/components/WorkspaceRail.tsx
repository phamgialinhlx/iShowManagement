import { useMemo, useState } from "react";
import { motion, AnimatePresence } from "motion/react";

import { useRailWidth } from "../lib/rail-width";
import { isSessionUnseen, useWorkspace } from "../lib/workspace";
import { RailGrip } from "./RailGrip";
import type { Project, Server, SessionStatus, SessionV3 } from "../lib/workspace-model";
import { QuickAdd } from "./QuickAdd";

/**
 * The session rail, v3 — a **Server → Project → Session** tree.
 *
 * Replaces the flat `SessionRail`, whose `(host, folder)` groups were *derived*
 * at render time (`session-groups.ts`). Here the tree is the stored structure:
 * servers hold projects, projects hold sessions of either kind. It still answers
 * the one question the rail exists for — "which of my sessions needs me?" — with
 * the same status marks, now only on Claude sessions (a terminal is neutral in
 * v1; see ADR-001).
 *
 * The create/open verbs map one interaction to each node (ADR-002): click a
 * session opens/focuses its pane; click a project opens its files pane; the
 * server's HOST button opens its host pane; `[+]`/`[✦]` add a terminal/Claude.
 */

const RAIL_DEFAULT = 216;
// Collapsed = fully hidden; the footer's sidebar icon is the one toggle.
const RAIL_COLLAPSED = 0;

const FOLD_KEY = "rmux.rail.folded.v3";
function readFolded(): Set<string> {
  try {
    const raw = localStorage.getItem(FOLD_KEY);
    return new Set(raw ? (JSON.parse(raw) as string[]) : []);
  } catch {
    return new Set();
  }
}
function writeFolded(set: Set<string>) {
  try {
    localStorage.setItem(FOLD_KEY, JSON.stringify([...set]));
  } catch {
    // A full localStorage must not stop the rail folding.
  }
}

/** `user@host:port`, or `local` for the local machine. */
function serverLabel(server: Server): string {
  const t = server.target;
  if (!t.host) return "local";
  return `${t.user ? `${t.user}@` : ""}${t.host}${t.port ? `:${t.port}` : ""}`;
}

/** Rule 0: red means *you* must act, and nothing else does. */
function statusColor(status: SessionStatus): string {
  switch (status) {
    case "waiting":
      return "rgb(var(--primary))";
    case "working":
      return "rgb(var(--busy))";
    default:
      return "var(--text-faint)";
  }
}

/** The attention mark — see the long note in the old rail. Only Claude sessions
 *  carry one; a terminal renders its kind glyph instead. */
function StatusDot({ status, finished }: { status: SessionStatus; finished?: boolean }) {
  const done = finished && status !== "working";
  if (status === "working") {
    return (
      <span
        className="round ring-spin shrink-0"
        title="working"
        style={{
          width: 9,
          height: 9,
          border: "1.5px solid rgb(var(--busy) / 0.28)",
          borderTopColor: "rgb(var(--busy))",
          boxSizing: "border-box",
        }}
      />
    );
  }
  return (
    <span
      className={`round shrink-0${done ? " done-swell" : ""}`}
      title={done ? "finished — not looked at yet" : status}
      style={{ width: 7, height: 7, background: done ? "rgb(var(--primary))" : statusColor(status) }}
    />
  );
}

/**
 * The server's aggregate dot — the reference shows a green "online" light, but
 * green is not in the palette (rule 0). So a present server reads as a soft
 * monochrome dot, and it inherits the real signal from its sessions: the working
 * spinner if any Claude is running, the accent dot if any needs the operator.
 */
function ServerDot({ status }: { status: SessionStatus }) {
  if (status === "working") return <StatusDot status="working" />;
  return (
    <span
      className="round shrink-0"
      title={status === "waiting" ? "a session needs you" : "connected"}
      style={{
        width: 8,
        height: 8,
        background: status === "waiting" ? "rgb(var(--primary))" : "var(--text-soft)",
      }}
    />
  );
}

/** Terminal glyph: a prompt mark (`>_`). Rule 3 — inline SVG, square caps. */
function TerminalMark({ color = "var(--text-faint)" }: { color?: string }) {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth="2.4"
      strokeLinecap="square"
      aria-hidden="true"
      className="shrink-0"
    >
      <path d="M5 7l5 5-5 5M13 17h6" />
    </svg>
  );
}

/**
 * The Claude mark — the official lobehub Claude glyph
 * (https://lobehub.com/icons/claude). Rule 3: inline SVG, no glyph font, no
 * emoji. A fill path, not a stroke, so it takes `fill`, not `stroke`; the colour
 * comes from the tokens so it follows the theme.
 */
function ClaudeMark({ color = "var(--text-soft)" }: { color?: string }) {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill={color}
      fillRule="evenodd"
      aria-hidden="true"
      className="shrink-0"
    >
      <path d="M4.709 15.955l4.72-2.647.08-.23-.08-.128H9.2l-.79-.048-2.698-.073-2.339-.097-2.266-.122-.571-.121L0 11.784l.055-.352.48-.321.686.06 1.52.103 2.278.158 1.652.097 2.449.255h.389l.055-.157-.134-.098-.103-.097-2.358-1.596-2.552-1.688-1.336-.972-.724-.491-.364-.462-.158-1.008.656-.722.881.06.225.061.893.686 1.908 1.476 2.491 1.833.365.304.145-.103.019-.073-.164-.274-1.355-2.446-1.446-2.49-.644-1.032-.17-.619a2.97 2.97 0 01-.104-.729L6.283.134 6.696 0l.996.134.42.364.62 1.414 1.002 2.229 1.555 3.03.456.898.243.832.091.255h.158V9.01l.128-1.706.237-2.095.23-2.695.08-.76.376-.91.747-.492.584.28.48.685-.067.444-.286 1.851-.559 2.903-.364 1.942h.212l.243-.242.985-1.306 1.652-2.064.73-.82.85-.904.547-.431h1.033l.76 1.129-.34 1.166-1.064 1.347-.881 1.142-1.264 1.7-.79 1.36.073.11.188-.02 2.856-.606 1.543-.28 1.841-.315.833.388.091.395-.328.807-1.969.486-2.309.462-3.439.813-.042.03.049.061 1.549.146.662.036h1.622l3.02.225.79.522.474.638-.079.485-1.215.62-1.64-.389-3.829-.91-1.312-.329h-.182v.11l1.093 1.068 2.006 1.81 2.509 2.33.127.578-.322.455-.34-.049-2.205-1.657-.851-.747-1.926-1.62h-.128v.17l.444.649 2.345 3.521.122 1.08-.17.353-.608.213-.668-.122-1.374-1.925-1.415-2.167-1.143-1.943-.14.08-.674 7.254-.316.37-.729.28-.607-.461-.322-.747.322-1.476.389-1.924.315-1.53.286-1.9.17-.632-.012-.042-.14.018-1.434 1.967-2.18 2.945-1.726 1.845-.414.164-.717-.37.067-.662.401-.589 2.388-3.036 1.44-1.882.93-1.086-.006-.158h-.055L4.132 18.56l-1.13.146-.487-.456.061-.746.231-.243 1.908-1.312-.006.006z" />
    </svg>
  );
}

export function rowSurface(active: boolean, onScreen: boolean) {
  return {
    background: active
      ? "color-mix(in srgb, var(--text) 14%, transparent)"
      : onScreen
        ? "color-mix(in srgb, var(--text) 3.5%, transparent)"
        : "transparent",
    boxShadow: active
      ? "inset 3px 0 0 var(--text)"
      : onScreen
        ? "inset 2px 0 0 var(--text-faint)"
        : "none",
  };
}

function Chevron({ folded }: { folded: boolean }) {
  return (
    <svg
      width="8"
      height="8"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="3"
      strokeLinecap="square"
      aria-hidden="true"
      style={{
        color: "var(--text-soft)",
        transform: folded ? "rotate(-90deg)" : "none",
        transition: "transform 140ms cubic-bezier(0.2,0.9,0.3,1)",
        flexShrink: 0,
      }}
    >
      <path d="M6 9l6 6 6-6" />
    </svg>
  );
}

function CloseIcon() {
  return (
    <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="square" aria-hidden="true">
      <path d="M18 6L6 18M6 6l12 12" />
    </svg>
  );
}

function PlusIcon() {
  return (
    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="square" aria-hidden="true">
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

/** Folder glyph — the explicit "open this project's files" affordance, because
 *  the uppercase label reads as a section header, not a button. Rule 3. */
function FolderIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true">
      <path d="M3 6v13h18V8h-9l-2-2H3z" />
    </svg>
  );
}

/** Server-rack glyph for HOST — the machine itself, not one of its readouts
 *  (processes, ports, metrics all belong to the box). Stacked rectangles fit the
 *  zero-radius language; the two square LEDs are where a status tint could go. */
function HostIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true">
      <rect x="3" y="4" width="18" height="7" />
      <rect x="3" y="13" width="18" height="7" />
      <path d="M7 7.5h.01" />
      <path d="M7 16.5h.01" />
    </svg>
  );
}

function SessionRow({
  session,
  active,
  onScreen,
  onOpen,
  onKill,
}: {
  session: SessionV3;
  active: boolean;
  onScreen: boolean;
  onOpen: () => void;
  onKill: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [editing, setEditing] = useState(false);
  const rename = useWorkspace((s) => s.renameSession);
  const status = useWorkspace((s) => s.statusOf(session.id));
  // Subscribed to the derived boolean, so the mark clears the moment the session
  // is opened (its watermark advances) or lights when a newer change arrives.
  const unseen = useWorkspace((s) => isSessionUnseen(s.runtime, s.seen, session.id));

  // A child row: the tree line supplies the left edge, so selection is a filled
  // block (as in the reference) rather than the top-level bar `rowSurface` draws.
  const surface = active
    ? { background: "color-mix(in srgb, var(--text) 12%, transparent)" }
    : onScreen
      ? { background: "color-mix(in srgb, var(--text) 4%, transparent)" }
      : undefined;

  return (
    <div className="group relative" style={surface}>
      {editing ? (
        <div className="flex items-center gap-2 px-3 py-[6px] pl-3">
          {session.kind === "claude" ? (
            <StatusDot status={status.status} finished={unseen} />
          ) : (
            <TerminalMark />
          )}
          <input
            autoFocus
            defaultValue={session.name}
            aria-label="Session name"
            className="data inset min-w-0 flex-1 px-1 py-[1px] text-[12px] outline-none"
            style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
            onFocus={(e) => e.currentTarget.select()}
            onBlur={(e) => {
              rename(session.id, e.currentTarget.value);
              setEditing(false);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") {
                e.currentTarget.value = session.name;
                e.currentTarget.blur();
              }
            }}
          />
        </div>
      ) : (
        <button
          type="button"
          onClick={onOpen}
          onDoubleClick={() => setEditing(true)}
          title={`${session.name}\nDouble-click to rename`}
          className="flex w-full items-center gap-2 px-3 py-[6px] pl-3 text-left"
        >
          {session.kind === "claude" ? (
            <StatusDot status={status.status} finished={unseen} />
          ) : (
            <TerminalMark />
          )}
          <span
            className="data min-w-0 flex-1 truncate text-[12px]"
            style={{ color: active ? "var(--text)" : "var(--text-soft)" }}
          >
            {session.name}
          </span>
        </button>
      )}

      {status.error && (
        <p className="data px-3 pb-1 pl-3 text-[10px]" style={{ color: "rgb(var(--primary))" }}>
          {status.error}
        </p>
      )}

      {confirming ? (
        <div className="flex flex-col gap-1 px-3 pb-2 pl-3">
          <span className="micro" style={{ color: "var(--text-soft)" }}>
            {session.kind === "claude" ? "ENDS THIS CLAUDE SESSION" : "ENDS THIS SHELL"} ON THE SERVER
          </span>
          <div className="flex gap-2">
            <button type="button" className="chip" style={{ color: "rgb(var(--primary))" }} onClick={onKill}>
              close it
            </button>
            <button type="button" className="chip" onClick={() => setConfirming(false)}>
              cancel
            </button>
          </div>
        </div>
      ) : (
        <button
          type="button"
          aria-label={`Close ${session.name}`}
          className="absolute right-2 top-[9px] opacity-0 group-hover:opacity-100"
          style={{ color: "var(--text-faint)" }}
          onClick={(e) => {
            e.stopPropagation();
            setConfirming(true);
          }}
        >
          <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="square" aria-hidden="true">
            <path d="M18 6L6 18M6 6l12 12" />
          </svg>
        </button>
      )}
    </div>
  );
}

function ProjectNode({
  project,
  folded,
  onToggle,
}: {
  project: Project;
  folded: boolean;
  onToggle: () => void;
}) {
  const sessions = useWorkspace((s) => s.sessions);
  const active = useWorkspace((s) => s.activeSession);
  const panes = useWorkspace((s) => s.panes);
  const openFiles = useWorkspace((s) => s.openFiles);
  const openSession = useWorkspace((s) => s.openSession);
  const addSession = useWorkspace((s) => s.addSession);
  const removeSession = useWorkspace((s) => s.removeSession);
  const removeProject = useWorkspace((s) => s.removeProject);
  const target = useWorkspace((s) => s.targetOfProject(project.id));
  const allServers = useWorkspace((s) => s.servers);
  // Stated, never assumed: two projects can share a folder name on different
  // machines, and this control is reached without naming one.
  const host = allServers.find((sv) => sv.id === project.serverId);

  const mine = sessions.filter((s) => s.projectId === project.id);
  const [confirming, setConfirming] = useState(false);
  const [adding, setAdding] = useState(false);
  const onScreen = new Set(
    panes.flatMap((p) => (p && p.kind === "session" ? [p.id] : [])),
  );

  /**
   * `+` and `✦` **ask** before creating.
   *
   * They used to call `addSession` outright, which decided three things the
   * operator is supposed to decide: whether to skip permissions, which provider
   * the credential goes to, and — for Claude — whether this is new work at all
   * or a continuation of yesterday's. `QuickAdd` and `ClaudeSessionPicker` were
   * both written for exactly this and were left unwired when the rail was
   * rebuilt as a Server → Project tree, so the two components existed, worked,
   * and were reachable from nowhere.
   */
  const add = (choice: {
    skipPermissions: boolean;
    modelProfile?: string;
    resume?: string;
    name?: string;
  }) => {
    const id = addSession(project.id, "claude", {
      skipPermissions: choice.skipPermissions,
      modelProfile: choice.modelProfile,
      resume: choice.resume,
      // A resumed conversation is named for what it was about; every session in
      // a folder would otherwise share that folder's name.
      name: choice.name?.trim() || undefined,
    });
    openSession(id);
    setAdding(false);
  };

  return (
    <div>
      {/* Group header — the "▾ TMUX" band: caret, an uppercase label, then the
          verbs: open files (folder), new terminal (+), new Claude (✦). */}
      <div className="group/prj flex items-center gap-1.5 py-[5px] pl-4 pr-2">
        <button type="button" className="shrink-0" onClick={onToggle} aria-expanded={!folded} title={project.folder}>
          <Chevron folded={folded} />
        </button>
        <button
          type="button"
          className="data min-w-0 flex-1 truncate text-left text-[10px]"
          style={{ color: "var(--text-soft)", fontWeight: 600, letterSpacing: "0.09em", textTransform: "uppercase" }}
          onClick={() => openFiles(project.id)}
          title={`${project.folder}\nOpen files`}
        >
          {project.label}
        </button>
        <button
          type="button"
          onClick={() => openFiles(project.id)}
          aria-label={`Open files in ${project.label}`}
          title="Open files"
          className="shrink-0 px-1 opacity-70 hover:opacity-100"
          style={{ color: "var(--text-soft)" }}
        >
          <FolderIcon />
        </button>
        {/* A **terminal** glyph, not a `+`.
            The plus sat next to the Claude mark and read as a generic "add", so
            the two verbs looked like one control with a preference — and it was
            routed through `QuickAdd`, which asks the two questions that only
            make sense for Claude (skip permissions, which provider). A shell has
            neither, so the panel made `+` look like a second way to start a
            Claude session. It creates a terminal directly now, and says so. */}
        <button
          type="button"
          onClick={() => {
            const id = addSession(project.id, "terminal");
            openSession(id);
          }}
          aria-label={`New terminal in ${project.label}`}
          title="New terminal"
          className="shrink-0 px-1 opacity-70 hover:opacity-100"
          style={{ color: "var(--text-soft)" }}
        >
          <TerminalMark color="currentColor" />
        </button>
        <button
          type="button"
          onClick={() => setAdding(true)}
          aria-label={`New Claude session in ${project.label}`}
          title="New Claude session"
          className="shrink-0 px-1 opacity-70 hover:opacity-100"
        >
          <ClaudeMark />
        </button>
        <button
          type="button"
          onClick={() => setConfirming(true)}
          aria-label={`Remove ${project.label}`}
          title="Remove this project"
          className="shrink-0 px-1 opacity-0 group-hover/prj:opacity-70 hover:!opacity-100"
          style={{ color: "var(--text-soft)" }}
        >
          <CloseIcon />
        </button>
      </div>

      {adding && (
        /*
         * A modal, not an inline panel.
         *
         * It was rendered inside the project row, which put a conversation
         * picker and two settings into a 216px column — and pushed every
         * session below it down the rail while open. The app already opens
         * "Connect a server" and "New project" this way, so an inline third
         * shape was a second convention for the same act.
         *
         * The earlier reasoning against a modal still stands where it applies:
         * a dialog that appears *on its own* over the workbench steals the
         * keystrokes of whoever is typing in a terminal. This one is opened by
         * a deliberate click, which is the case that rule was never about.
         */
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.12 }}
          className="fixed inset-0 z-[95] grid place-items-start justify-center pt-[9vh]"
          style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
          // Clicking the backdrop dismisses; clicking the panel must not, or
          // every press inside the form would close it.
          onClick={() => setAdding(false)}
        >
          <motion.div
            initial={{ opacity: 0, y: -8 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.14, ease: [0.2, 0.9, 0.3, 1] }}
            className="window w-[min(560px,92vw)]"
            onClick={(e) => e.stopPropagation()}
          >
            <QuickAdd
              label={project.label}
              where={`${project.folder} on ${host ? serverLabel(host) : "this machine"}`}
              inheritedProfile={mine.find((s) => s.modelProfile)?.modelProfile}
              busy={false}
              target={target}
              folder={project.folder}
              onCancel={() => setAdding(false)}
              onCreate={add}
            />
          </motion.div>
        </motion.div>
      )}

      {/* Same reasoning as the server's: `removeProject` ends every session in
          it, and each one sends a real kill to the host. The count is the whole
          message — removing an empty project costs nothing, removing one with
          two Claude sessions in it ends both. */}
      {confirming && (
        <div className="flex flex-col gap-1 py-1 pl-6 pr-2">
          <span className="micro leading-relaxed" style={{ color: "var(--text-soft)" }}>
            {mine.length > 0
              ? `ENDS ${mine.length} SESSION${mine.length === 1 ? "" : "S"} ON THE SERVER`
              : "REMOVES THIS PROJECT FROM THE LIST"}
          </span>
          <div className="flex gap-2">
            <button
              type="button"
              className="chip"
              style={{ color: "rgb(var(--primary))" }}
              onClick={() => {
                setConfirming(false);
                removeProject(project.id);
              }}
            >
              remove it
            </button>
            <button type="button" className="chip" onClick={() => setConfirming(false)}>
              cancel
            </button>
          </div>
        </div>
      )}

      {/* Children hang off a vertical guide line, as in the reference. */}
      <AnimatePresence initial={false}>
        {!folded && mine.length > 0 && (
          <motion.div
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: "auto" }}
            exit={{ opacity: 0, height: 0 }}
            transition={{ duration: 0.18, ease: [0.2, 0.9, 0.3, 1] }}
            className="relative ml-[18px] overflow-hidden"
            style={{ borderLeft: "1px solid var(--border)" }}
          >
            {mine.map((session) => (
              <SessionRow
                key={session.id}
                session={session}
                active={session.id === active}
                onScreen={onScreen.has(session.id)}
                onOpen={() => openSession(session.id)}
                onKill={() => removeSession(session.id)}
              />
            ))}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function ServerNode({
  server,
  folded,
  foldedNodes,
  onToggle,
  onToggleProject,
  onNewProject,
}: {
  server: Server;
  folded: boolean;
  foldedNodes: Set<string>;
  onToggle: () => void;
  onToggleProject: (id: string) => void;
  onNewProject: (serverId: string) => void;
}) {
  // Select the stable arrays and derive with useMemo — filtering *inside* the
  // selector returns a new array every call, which breaks useSyncExternalStore's
  // cache and spins into "Maximum update depth exceeded" (a white screen).
  const allProjects = useWorkspace((s) => s.projects);
  const projects = useMemo(() => allProjects.filter((p) => p.serverId === server.id), [allProjects, server.id]);
  const allSessions = useWorkspace((s) => s.sessions);
  const runtime = useWorkspace((s) => s.runtime);
  const seen = useWorkspace((s) => s.seen);
  const openHost = useWorkspace((s) => s.openHost);
  const removeServer = useWorkspace((s) => s.removeServer);
  const [confirming, setConfirming] = useState(false);

  const projectIds = useMemo(() => new Set(projects.map((p) => p.id)), [projects]);
  const mine = useMemo(
    () => allSessions.filter((s) => projectIds.has(s.projectId)),
    [allSessions, projectIds],
  );
  // Roll the server's Claude sessions up into one dot: working wins, then needs-you.
  const agg = useMemo<SessionStatus>(() => {
    let waiting = false;
    for (const s of mine) {
      if (s.kind !== "claude") continue;
      const r = runtime[s.id];
      if (r?.status === "working") return "working";
      // Already returned above if working, so waiting or finished-unseen needs you.
      if (r?.status === "waiting" || isSessionUnseen(runtime, seen, s.id)) waiting = true;
    }
    return waiting ? "waiting" : "idle";
  }, [mine, runtime, seen]);

  return (
    <div className="mb-1">
      {/* Server card — accent bar, status dot, bold name; the HOST / + verbs. */}
      <div
        className="group/srv relative mx-1.5 mt-1.5 flex items-center gap-1.5 py-[7px] pl-2.5 pr-2"
        style={{
          background: "color-mix(in srgb, var(--text) 6%, transparent)",
          boxShadow: "inset 3px 0 0 var(--text-soft)",
        }}
      >
        <button type="button" className="shrink-0" onClick={onToggle} aria-expanded={!folded}>
          <Chevron folded={folded} />
        </button>
        <ServerDot status={agg} />
        <button
          type="button"
          className="data min-w-0 flex-1 truncate text-left text-[12px]"
          style={{ color: "var(--text)", fontWeight: 700 }}
          onClick={onToggle}
          title={`${serverLabel(server)}\nClick to ${folded ? "expand" : "collapse"}`}
        >
          {serverLabel(server)}
        </button>
        <button
          type="button"
          onClick={() => openHost(server.id)}
          aria-label="Host"
          title="Host: processes, ports, metrics"
          className="shrink-0 px-1 opacity-70 hover:opacity-100"
          style={{ color: "var(--text-soft)" }}
        >
          <HostIcon />
        </button>
        <button
          type="button"
          onClick={() => onNewProject(server.id)}
          aria-label={`New project on ${serverLabel(server)}`}
          title="Add a project (folder) on this server"
          className="shrink-0 px-1 opacity-70 hover:opacity-100"
          style={{ color: "var(--text-soft)" }}
        >
          <PlusIcon />
        </button>
        {/* Hidden until the row is hovered, like the session's close: a
            destructive control sitting permanently beside HOST and + would be
            the easiest thing on the row to hit by accident. */}
        <button
          type="button"
          onClick={() => setConfirming(true)}
          aria-label={`Remove ${serverLabel(server)}`}
          title="Remove this server"
          className="shrink-0 px-1 opacity-0 group-hover/srv:opacity-70 hover:!opacity-100"
          style={{ color: "var(--text-soft)" }}
        >
          <CloseIcon />
        </button>
      </div>

      {/*
        Removing a server is not "hide it from the list": `removeServer`
        cascades to its projects and then to its sessions, and each of those
        sends a real kill to the host. So the counts are stated before the
        press — "3 sessions" is the difference between tidying the rail and
        ending three running conversations, and nothing else on screen says it.
      */}
      {confirming && (
        <div className="mx-1.5 flex flex-col gap-1 px-2.5 py-2">
          <span className="micro leading-relaxed" style={{ color: "var(--text-soft)" }}>
            {mine.length > 0
              ? `ENDS ${mine.length} SESSION${mine.length === 1 ? "" : "S"} AND ${projects.length} PROJECT${projects.length === 1 ? "" : "S"} ON THE SERVER`
              : "REMOVES THIS SERVER FROM THE LIST"}
          </span>
          <div className="flex gap-2">
            <button
              type="button"
              className="chip"
              style={{ color: "rgb(var(--primary))" }}
              onClick={() => {
                setConfirming(false);
                removeServer(server.id);
              }}
            >
              remove it
            </button>
            <button type="button" className="chip" onClick={() => setConfirming(false)}>
              cancel
            </button>
          </div>
        </div>
      )}

      {!folded &&
        projects.map((project) => (
          <ProjectNode
            key={project.id}
            project={project}
            folded={foldedNodes.has(project.id)}
            onToggle={() => onToggleProject(project.id)}
          />
        ))}
      {!folded && projects.length === 0 && (
        <p className="micro px-3 py-2 pl-6" style={{ color: "var(--text-faint)" }}>
          no projects — add one with +
        </p>
      )}
    </div>
  );
}

export function WorkspaceRail({
  onConnectServer,
  onNewProject,
}: {
  /** Opens the host-picker (ssh-config aliases + manual) to connect a Server. */
  onConnectServer: () => void;
  /** Opens the folder-picker on a Server to create a Project. */
  onNewProject: (serverId: string) => void;
}) {
  const servers = useWorkspace((s) => s.servers);
  const sessions = useWorkspace((s) => s.sessions);
  const collapsed = useWorkspace((s) => s.railCollapsed);
  const runtime = useWorkspace((s) => s.runtime);
  const seen = useWorkspace((s) => s.seen);

  const { width: railWidth, startResize } = useRailWidth("rmux.rail.width", RAIL_DEFAULT);

  const [folded, setFolded] = useState<Set<string>>(readFolded);
  const toggleNode = (id: string) =>
    setFolded((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      writeFolded(next);
      return next;
    });

  // Both kinds of "needs you", Claude only: asking, or finished-and-unseen.
  const needing = sessions.filter((s) => {
    if (s.kind !== "claude") return false;
    return runtime[s.id]?.status === "waiting" || isSessionUnseen(runtime, seen, s.id);
  }).length;

  return (
    <motion.aside
      className="panel chrome-black relative flex shrink-0 flex-col overflow-hidden"
      animate={{ width: collapsed ? RAIL_COLLAPSED : railWidth }}
      // The spring is for the collapse toggle. A *drag* must not spring: the
      // edge would lag the pointer and overshoot on release, which reads as the
      // rail resisting you. `duration: 0` while dragging keeps it welded to the
      // cursor; motion re-animates the moment `collapsed` changes instead.
      transition={{ type: "spring", stiffness: 320, damping: 34 }}
      style={{ width: collapsed ? RAIL_COLLAPSED : railWidth }}
    >
      {!collapsed && <RailGrip side="right" onPointerDown={(e) => startResize(e, "left")} />}
      <header className="flex shrink-0 items-center border-b px-3 py-2" style={{ borderColor: "var(--border)" }}>
        <span className="micro">
          SERVERS
          {needing > 0 && <span style={{ color: "rgb(var(--primary))" }}> · {needing} need you</span>}
        </span>
      </header>

      {!collapsed && (
        <div className="shrink-0 border-b p-2" style={{ borderColor: "var(--border)" }}>
          <button type="button" className="btn w-full" style={{ padding: "8px 12px", fontSize: 11 }} onClick={onConnectServer} title="Connect a server (⌘N)">
            Connect a server
          </button>
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden">
        {!collapsed &&
          servers.map((server) => (
            <ServerNode
              key={server.id}
              server={server}
              folded={folded.has(server.id)}
              foldedNodes={folded}
              onToggle={() => toggleNode(server.id)}
              onToggleProject={toggleNode}
              onNewProject={onNewProject}
            />
          ))}

        {servers.length === 0 && !collapsed && (
          <p className="micro px-3 py-3 leading-relaxed">no servers yet — connect one, or add a local folder</p>
        )}
      </div>
    </motion.aside>
  );
}
