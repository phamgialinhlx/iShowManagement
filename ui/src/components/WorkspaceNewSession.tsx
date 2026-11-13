import { useEffect, useMemo, useRef, useState } from "react";
import { motion, AnimatePresence } from "motion/react";

import { api, type ClaudeSessionInfo, type ConfigHost, type TargetRef } from "../lib/api";
import { FolderBrowser } from "./FolderBrowser";
import { KeyChoiceField, TestConnection } from "./ConnectionFields";
import { useWorkspace } from "../lib/workspace";
import { formTarget, parsePort, targetLabel, type ConnectForm } from "../lib/connect-form";

/**
 * Connect a Server, or add a Project to one.
 *
 * The v3 split of the old `NewSession` wizard (which produced a fused session).
 * Two modes:
 *
 * - **server** — pick a host (`~/.ssh/config` aliases, or type one), optionally
 *   a user, a port and a key, connect to surface a bad alias / credential
 *   prompt, then register the Server.
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
  /** GENERATE AN RMUX KEY: connected and registered; now installing the key. */
  | { kind: "installing"; label: string }
  /** The outcome of the key install, said before the dialog closes. */
  | { kind: "done"; label: string; text: string; ok: boolean }
  | { kind: "folder"; target: TargetRef; label: string; home: string; serverId: string };

export function WorkspaceNewSession({ mode, onClose }: { mode: NewMode; onClose: () => void }) {
  const servers = useWorkspace((s) => s.servers);
  const connectServer = useWorkspace((s) => s.connectServer);
  const setServerIdentity = useWorkspace((s) => s.setServerIdentity);
  const createProject = useWorkspace((s) => s.createProject);
  const addSession = useWorkspace((s) => s.addSession);
  const openSession = useWorkspace((s) => s.openSession);
  const projects = useWorkspace((s) => s.projects);

  const [step, setStep] = useState<Step>({ kind: "host" });
  const [configHosts, setConfigHosts] = useState<ConfigHost[]>([]);
  const [filter, setFilter] = useState("");
  const [form, setForm] = useState<ConnectForm>({ host: "", user: "", port: "", key: "default", keyPath: "" });
  /** The config host the form was filled from — its HostName/User/Port are *hints*. */
  const [hinted, setHinted] = useState<ConfigHost | null>(null);
  const [keyBlocked, setKeyBlocked] = useState(false);
  const hostRef = useRef<HTMLInputElement>(null);
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

  const serverLabelOf = targetLabel;
  const parsed = formTarget(form);
  const portError = parsePort(form.port).error;
  const setField = (patch: Partial<ConnectForm>) => setForm((f) => ({ ...f, ...patch }));

  /**
   * Fill the form from a `~/.ssh/config` host. **Only the alias is copied** —
   * ssh resolves it with the operator's own config. HostName/User/Port become
   * placeholders in the empty fields, display hints that never reach the
   * target (CLAUDE.md: never resolve `~/.ssh/config` ourselves).
   */
  const pickConfigHost = (h: ConfigHost) => {
    setForm((f) => ({ ...f, host: h.alias, user: "", port: "" }));
    setHinted(h);
    setError(null);
  };

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
  const connect = async (target: TargetRef, label: string, generateKey = false) => {
    setError(null);
    setStep({ kind: "connecting", label });
    let serverId: string;
    let home: string;
    try {
      // Resolving home establishes the connection — a bad alias or credential
      // prompt surfaces here, before anything is registered.
      home = await api.fsHome(target);
      serverId = connectServer(target);
      // Re-adding a server that exists is how its key choice is changed here
      // too: the dialog's choice is the one the operator just made.
      if (target.host) setServerIdentity(serverId, target.identity);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setStep({ kind: "host" });
      return;
    }
    if (generateKey && target.host) {
      // Over the connection just authenticated — the password typed a moment
      // ago is what authorises writing `authorized_keys`, once.
      setStep({ kind: "installing", label });
      try {
        const text = await api.sshKeyInstall(target);
        setStep({ kind: "done", label, ok: true, text: `${text} · later connections use the key` });
      } catch (e) {
        setStep({
          kind: "done",
          label,
          ok: false,
          text: `connected and added, but the key was not installed: ${e instanceof Error ? e.message : String(e)}`,
        });
      }
      return;
    }
    if (mode.kind === "server") {
      onClose();
      return;
    }
    setStep({ kind: "folder", target, label, home, serverId });
  };

  const connectForm = () => {
    if (!parsed.target || keyBlocked) return;
    void connect(parsed.target, serverLabelOf(parsed.target), form.key === "generate");
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
        className="menu corner flex w-full max-w-[560px] flex-col gap-4 overflow-y-auto p-5"
        // A ceiling of its own, not only on the host list: the form under the
        // list can outgrow a short window by itself (CLAUDE.md, "A dialog needs
        // its own max-h").
        style={{ height: projectMode ? 520 : 640, maxHeight: "calc(91vh - 16px)" }}
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
                : step.kind === "installing" || step.kind === "done"
                  ? "rmux key"
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
              placeholder="filter ~/.ssh/config hosts…"
              spellCheck={false}
              autoComplete="off"
            />

            {/* The ceiling is on the element that scrolls, not on a wrapper: a
                config with sixty hosts must not push the form off the dialog. */}
            <div
              className="inset min-h-[96px] flex-1 overflow-y-auto"
              style={{ border: "1px solid var(--border)", maxHeight: 210 }}
            >
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
                  // One click fills the form (port and key are still to choose);
                  // a double click connects with the defaults, as it used to.
                  onClick={() => pickConfigHost(h)}
                  onDoubleClick={() => void connect({ host: h.alias }, h.alias)}
                  className="flex w-full flex-col px-2 py-[6px] text-left"
                  title={`ssh ${h.alias} — click to fill in, double-click to connect`}
                  aria-pressed={hinted?.alias === h.alias && form.host === h.alias}
                  style={{
                    background: hinted?.alias === h.alias && form.host === h.alias ? "var(--hover)" : "transparent",
                  }}
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
                    {h.port ? `:${h.port}` : ""}
                  </span>
                </button>
              ))}
            </div>

            <form
              className="flex flex-col gap-3"
              onSubmit={(e) => {
                e.preventDefault();
                connectForm();
              }}
            >
              <div className="grid gap-2" style={{ gridTemplateColumns: "minmax(0,1fr) 120px 76px" }}>
                <label className="flex min-w-0 flex-col gap-1">
                  <span className="micro">HOST</span>
                  <input
                    ref={hostRef}
                    className="field"
                    value={form.host}
                    onChange={(e) => {
                      setField({ host: e.target.value });
                      if (hinted && e.target.value !== hinted.alias) setHinted(null);
                    }}
                    placeholder="alias or hostname"
                    spellCheck={false}
                    autoComplete="off"
                  />
                </label>
                <label className="flex min-w-0 flex-col gap-1">
                  <span className="micro">USER</span>
                  <input
                    className="field"
                    value={form.user}
                    onChange={(e) => setField({ user: e.target.value })}
                    // A hint from the config, never a value: left empty, ssh
                    // applies the config's own User.
                    placeholder={hinted?.user ? `${hinted.user} (config)` : "optional"}
                    spellCheck={false}
                    autoComplete="off"
                  />
                </label>
                <label className="flex min-w-0 flex-col gap-1">
                  <span className="micro">PORT</span>
                  <input
                    className="field"
                    value={form.port}
                    onChange={(e) => setField({ port: e.target.value })}
                    placeholder={hinted?.port ? `${hinted.port}` : "22"}
                    inputMode="numeric"
                    aria-invalid={portError ? true : undefined}
                    spellCheck={false}
                    autoComplete="off"
                  />
                </label>
              </div>
              {portError && (
                <p role="alert" className="data -mt-2 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
                  {portError}
                </p>
              )}
              {hinted?.hostname && form.host === hinted.alias && (
                <p className="micro -mt-2" style={{ color: "var(--text-faint)" }}>
                  from ~/.ssh/config: {[hinted.user, hinted.hostname].filter(Boolean).join("@")}
                  {hinted.port ? `:${hinted.port}` : ""} — ssh resolves the alias itself
                </p>
              )}

              <KeyChoiceField
                choice={form.key}
                path={form.keyPath}
                onChoice={(key) => setField({ key })}
                onPath={(keyPath) => setField({ keyPath })}
                onBlockingChange={setKeyBlocked}
              />

              <div className="flex items-center gap-2">
                <TestConnection target={parsed.target} disabled={keyBlocked} />
                <button
                  className="btn-primary btn"
                  type="submit"
                  disabled={!parsed.target || keyBlocked}
                  title={parsed.error ?? (keyBlocked ? "the chosen key cannot be used" : undefined)}
                >
                  Connect
                </button>
              </div>
            </form>
          </>
        )}

        {step.kind === "installing" && (
          <div className="grid min-h-0 flex-1 place-items-center">
            <div className="flex flex-col items-center gap-3">
              <div className="flex h-[16px] items-end gap-[3px]">
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
              </div>
              <span className="micro">connected to {step.label} · installing rmux's key</span>
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                only the public half is sent; the private key stays on this machine
              </span>
            </div>
          </div>
        )}

        {step.kind === "done" && (
          <div className="grid min-h-0 flex-1 place-items-center">
            <div className="flex max-w-[440px] flex-col items-center gap-3 text-center">
              <span
                role={step.ok ? "status" : "alert"}
                className="data text-[12px]"
                style={{ color: step.ok ? "var(--text)" : "rgb(var(--primary))" }}
              >
                {step.text}
              </span>
              <button type="button" className="btn" onClick={onClose} autoFocus>
                Done
              </button>
            </div>
          </div>
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
