import { useEffect, useMemo, useRef, useState } from "react";
import { motion, AnimatePresence } from "motion/react";

import { api, type ClaudeSessionInfo, type ConfigHost, type TargetRef } from "../lib/api";
import { FolderBrowser } from "./FolderBrowser";
import { useWorkspace } from "../lib/workspace";

/**
 * Connect a Server, or add a Project to one.
 *
 * The v3 split of the old `NewSession` wizard (which produced a fused session).
 * Two modes:
 *
 * - **server** — pick a host (`~/.ssh/config` aliases, or type one), connect to
 *   surface a bad alias / credential prompt, then register the Server.
 * - **project** — on a Server already chosen, browse and pick a folder, then
 *   register the Project.
 *
 * Sessions themselves are created from the rail (`+` terminal, `✦` Claude);
 * skip-permissions / model-profile at creation is a follow-up.
 *
 * ## Resuming does not go through the folder browser
 *
 * The folder step used to be the only way forward, which put the wrong question
 * first: to come back to yesterday's conversation you had to *remember where it
 * was* and navigate there, and the app already knew — every transcript records
 * its own `cwd`, and `claude_list_all_sessions` reads them host-wide for exactly
 * this. Reported as "not allowing to list all sessions to resume on that server
 * instead of having to select folder", which is precisely it: the operator has a
 * session in mind, not a path.
 *
 * So the step offers both, and **RESUME is the first tab** — returning to work
 * is the common case; starting somewhere new is the occasional one. Picking a
 * session registers the Project from the folder the transcript names, so the
 * folder is still created, just not typed.
 */

export type NewMode = { kind: "server" } | { kind: "project"; serverId: string };

type Step =
  | { kind: "host" }
  | { kind: "connecting"; label: string }
  | { kind: "folder"; target: TargetRef; label: string; home: string; serverId: string };

export function WorkspaceNewSession({ mode, onClose }: { mode: NewMode; onClose: () => void }) {
  const servers = useWorkspace((s) => s.servers);
  const connectServer = useWorkspace((s) => s.connectServer);
  const createProject = useWorkspace((s) => s.createProject);
  const addSession = useWorkspace((s) => s.addSession);
  const openSession = useWorkspace((s) => s.openSession);
  const projects = useWorkspace((s) => s.projects);

  const [step, setStep] = useState<Step>({ kind: "host" });
  const [configHosts, setConfigHosts] = useState<ConfigHost[]>([]);
  const [filter, setFilter] = useState("");
  const [typed, setTyped] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Resume first: coming back to work is the common case, starting somewhere
  // new the occasional one.
  const [tab, setTab] = useState<"resume" | "folder">("resume");
  const filterRef = useRef<HTMLInputElement>(null);

  const projectMode = mode.kind === "project";
  const existingServer = projectMode ? servers.find((s) => s.id === mode.serverId) : undefined;

  useEffect(() => {
    if (mode.kind === "server") {
      filterRef.current?.focus();
      api
        .sshConfigHosts()
        .then((hosts) => setConfigHosts(Array.isArray(hosts) ? hosts : []))
        .catch(() => setConfigHosts([]));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const serverLabelOf = (t: TargetRef) =>
    t.host ? `${t.user ? `${t.user}@` : ""}${t.host}${t.port ? `:${t.port}` : ""}` : "this machine";

  // Project mode: connect to the chosen server (resolving home) as soon as we open.
  useEffect(() => {
    if (mode.kind !== "project") return;
    const server = servers.find((s) => s.id === mode.serverId);
    if (!server) {
      setError("That server no longer exists.");
      return;
    }
    void goToFolder(server.target, serverLabelOf(server.target), server.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const inUse = useMemo(
    () => new Set(servers.map((s) => s.target.host).filter(Boolean) as string[]),
    [servers],
  );

  const matches = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const ranked = [...configHosts].sort((a, b) => {
      const used = Number(inUse.has(b.alias)) - Number(inUse.has(a.alias));
      return used !== 0 ? used : a.alias.localeCompare(b.alias);
    });
    if (!needle) return ranked;
    return ranked.filter(
      (h) =>
        h.alias.toLowerCase().includes(needle) ||
        h.hostname?.toLowerCase().includes(needle) ||
        h.user?.toLowerCase().includes(needle),
    );
  }, [configHosts, filter, inUse]);

  /** Folders already opened on a server — offered as browse shortcuts. */
  const recentsFor = (serverId: string) =>
    projects.filter((p) => p.serverId === serverId).map((p) => p.folder);

  /** Register the Server, then (server mode) close, or (project mode) browse folders. */
  const connect = async (target: TargetRef, label: string) => {
    setError(null);
    setStep({ kind: "connecting", label });
    try {
      // Resolving home establishes the connection — a bad alias or credential
      // prompt surfaces here, before anything is registered.
      const home = await api.fsHome(target);
      const serverId = connectServer(target);
      if (mode.kind === "server") {
        onClose();
        return;
      }
      setStep({ kind: "folder", target, label, home, serverId });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setStep({ kind: "host" });
    }
  };

  /** Project mode: connect to a known server and land on the folder browser. */
  const goToFolder = async (target: TargetRef, label: string, serverId: string) => {
    setError(null);
    setStep({ kind: "connecting", label });
    try {
      const home = await api.fsHome(target);
      setStep({ kind: "folder", target, label, home, serverId });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  /**
   * Resume a conversation: register its folder, then open it with `--resume`.
   *
   * The folder comes from the transcript's own `cwd`, so this creates exactly
   * the Project the session was running in — no guessing, and no second dialog
   * asking where it was.
   */
  const resumeSession = (serverId: string, session: ClaudeSessionInfo) => {
    if (!session.folder) return;
    setBusy(true);
    setError(null);
    try {
      const projectId = createProject(serverId, session.folder);
      const id = addSession(projectId, "claude", {
        resume: session.id,
        // Named for the conversation rather than the folder: the whole reason
        // to resume this one rather than that one is what it was about.
        name: session.title?.trim() || undefined,
      });
      // Opened into the focused tile, or resuming would add a row to the rail
      // and leave the operator to find and click it — the step they came here
      // to skip.
      openSession(id);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const chooseFolder = (serverId: string, folder: string) => {
    setBusy(true);
    setError(null);
    try {
      createProject(serverId, folder);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const title = projectMode ? "New project" : "Connect a server";

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.12 }}
      className="fixed inset-0 z-[95] grid place-items-start justify-center pt-[9vh]"
      style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
      onClick={onClose}
    >
      <motion.div
        initial={{ opacity: 0, y: -8 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ type: "spring", stiffness: 300, damping: 28 }}
        onClick={(e) => e.stopPropagation()}
        className="menu corner flex w-full max-w-[560px] flex-col gap-4 p-5"
        style={{ height: 520 }}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <header className="flex items-baseline gap-3">
          <span className="kicker">{title}</span>
          <span className="micro">
            {step.kind === "host"
              ? "choose a machine"
              : step.kind === "connecting"
                ? "connecting"
                : "choose a folder"}
          </span>
          {existingServer && (
            <span className="micro ml-auto" style={{ color: "var(--text-soft)" }}>
              on {serverLabelOf(existingServer.target)}
            </span>
          )}
        </header>

        {step.kind === "host" && (
          <>
            <input
              ref={filterRef}
              className="field"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="filter hosts…"
              spellCheck={false}
              autoComplete="off"
            />

            <div className="inset min-h-0 flex-1 overflow-y-auto" style={{ border: "1px solid var(--border)" }}>
              <button
                type="button"
                onClick={() => void connect({}, "this machine")}
                className="flex w-full flex-col px-2 py-[6px] text-left"
                onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
                onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
              >
                <span className="data text-[12px]">This machine</span>
                <span className="micro">local — no connection needed</span>
              </button>

              {matches.map((h) => (
                <button
                  key={h.alias}
                  type="button"
                  onClick={() => void connect({ host: h.alias }, h.alias)}
                  className="flex w-full flex-col px-2 py-[6px] text-left"
                  title={`ssh ${h.alias}`}
                  onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
                  onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
                >
                  <span className="data truncate text-[12px]">
                    {h.alias}
                    {inUse.has(h.alias) && (
                      <span className="micro" style={{ marginLeft: 6 }}>
                        connected
                      </span>
                    )}
                  </span>
                  <span className="micro truncate">
                    {[h.user, h.hostname].filter(Boolean).join("@") || "—"}
                  </span>
                </button>
              ))}
            </div>

            <form
              className="flex gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                if (typed.trim()) void connect({ host: typed.trim() }, typed.trim());
              }}
            >
              <input
                className="field flex-1"
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
                placeholder="or type an alias / user@host"
                spellCheck={false}
                autoComplete="off"
              />
              <button className="btn" type="submit" disabled={!typed.trim()}>
                Connect
              </button>
            </form>
          </>
        )}

        {step.kind === "connecting" && (
          <div className="grid min-h-0 flex-1 place-items-center">
            <div className="flex flex-col items-center gap-3">
              <div className="flex h-[16px] items-end gap-[3px]">
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
              </div>
              <span className="micro">connecting to {step.label}</span>
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                a password or 2FA prompt will appear if the host asks for one
              </span>
            </div>
          </div>
        )}

        {step.kind === "folder" && (
          <>
            <div className="flex items-baseline gap-3">
              <span className="micro">
                on <span style={{ color: "var(--text)" }}>{step.label}</span>
              </span>
              {/* Segmented, not a dropdown: two options, and a `<select>` would
                  hide both the choices and the current one behind a click. */}
              <div className="ml-auto flex gap-1">
                {(["resume", "folder"] as const).map((t) => (
                  <button
                    key={t}
                    type="button"
                    className="chip"
                    aria-pressed={tab === t}
                    onClick={() => setTab(t)}
                  >
                    {t === "resume" ? "RESUME A SESSION" : "NEW FOLDER"}
                  </button>
                ))}
              </div>
            </div>

            {tab === "resume" ? (
              <ResumeList
                target={step.target}
                busy={busy}
                onChoose={(session) => resumeSession(step.serverId, session)}
              />
            ) : (
              <FolderBrowser
                target={step.target}
                initialPath={step.home}
                recents={recentsFor(step.serverId)}
                busy={busy}
                onChoose={(folder) => chooseFolder(step.serverId, folder)}
              />
            )}
          </>
        )}

        {error && (
          <p role="alert" className="data text-[11px] leading-relaxed" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}
      </motion.div>
    </motion.div>
  );
}

/**
 * Every Claude conversation on the host, newest first.
 *
 * One request for the whole machine rather than one per folder — the point is
 * that the operator does not know which folder it was in, so asking per folder
 * would be asking the question this exists to answer.
 *
 * A session whose transcript records no `cwd` is **listed but not selectable**,
 * with the reason shown. Hiding it would be a conversation that exists on the
 * host and cannot be found anywhere in rmux; offering it would be a row that
 * does nothing when clicked.
 */
function ResumeList({
  target,
  busy,
  onChoose,
}: {
  target: TargetRef;
  busy: boolean;
  onChoose: (session: ClaudeSessionInfo) => void;
}) {
  const [sessions, setSessions] = useState<ClaudeSessionInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    let cancelled = false;
    setSessions(null);
    setError(null);
    api
      .claudeListAllSessions(target)
      .then((list) => !cancelled && setSessions(Array.isArray(list) ? list : []))
      .catch((e) => !cancelled && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [target]);

  const needle = filter.trim().toLowerCase();
  const shown = (sessions ?? [])
    .filter((s) => !needle || `${s.title ?? ""} ${s.folder ?? ""}`.toLowerCase().includes(needle))
    .slice()
    .sort((a, b) => b.modified - a.modified);

  // A read over SSH of every project directory on the host is genuinely slow, so
  // it says what it is doing rather than showing an empty box.
  if (!sessions && !error) {
    return (
      <p className="data text-[11px]" style={{ color: "var(--text-faint)" }}>
        Reading conversations on this host…
      </p>
    );
  }

  if (error) {
    return (
      <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
        {error}
      </p>
    );
  }

  if (!sessions?.length) {
    return (
      <p className="data text-[11px]" style={{ color: "var(--text-faint)" }}>
        No Claude conversations found on this host. Start one from a folder instead.
      </p>
    );
  }

  return (
    <div className="flex min-h-0 flex-col gap-2">
      <input
        className="field"
        placeholder="filter by title or folder"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
      />
      <ul className="flex max-h-[340px] min-h-0 flex-col overflow-y-auto">
        {shown.map((session) => {
          const resumable = !!session.folder;
          return (
            <li key={session.id} className="border-b" style={{ borderColor: "var(--border)" }}>
              <button
                type="button"
                className="flex w-full flex-col items-start gap-[2px] px-1 py-[6px] text-left disabled:opacity-50"
                disabled={busy || !resumable}
                onClick={() => onChoose(session)}
                style={{ cursor: resumable ? "pointer" : "not-allowed" }}
              >
                <span className="data w-full truncate text-[12px]" style={{ color: "var(--text)" }}>
                  {session.title?.trim() || <em style={{ color: "var(--text-faint)" }}>untitled conversation</em>}
                </span>
                <span className="micro w-full truncate" style={{ color: "var(--text-faint)" }}>
                  {resumable ? session.folder : "no folder recorded — cannot be resumed"} · {ago(session.modified)}
                </span>
              </button>
            </li>
          );
        })}
        {!shown.length && (
          <li className="data py-2 text-[11px]" style={{ color: "var(--text-faint)" }}>
            Nothing matches.
          </li>
        )}
      </ul>
    </div>
  );
}

/** `3h ago`, `yesterday`, `12 Mar` — how long since it was last touched. */
function ago(unixSeconds: number): string {
  const seconds = Math.max(0, Date.now() / 1000 - unixSeconds);
  if (seconds < 3600) return `${Math.max(1, Math.round(seconds / 60))}m ago`;
  if (seconds < 86_400) return `${Math.round(seconds / 3600)}h ago`;
  if (seconds < 172_800) return "yesterday";
  if (seconds < 86_400 * 30) return `${Math.round(seconds / 86_400)}d ago`;
  return new Date(unixSeconds * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short" });
}

export function WorkspaceNewSessionLayer({
  mode,
  onClose,
}: {
  mode: NewMode | null;
  onClose: () => void;
}) {
  return <AnimatePresence>{mode && <WorkspaceNewSession mode={mode} onClose={onClose} />}</AnimatePresence>;
}
