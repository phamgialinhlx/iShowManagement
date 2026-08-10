import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { PanelLoader } from "./PanelLoader";
import { useWorkspace } from "../lib/workspace";
import { isClaudeSession, reattachName, type ServerId } from "../lib/workspace-model";
import {
  api,
  isTauri,
  type AgentSession,
  type DockerReport,
  type Forward,
  type ListeningPort,
  type ProcessInfo,
  type TargetRef,
} from "../lib/api";

/**
 * What the host is running, and how to reach it.
 *
 * Two jobs that belong together because they answer the same question — "what
 * is going on over there" — and because both are about the machine rather than
 * the conversation. The rail widget shows the top five processes as a glance;
 * this is where you go to *act*.
 *
 * ## Killing things
 *
 * The pid crosses the IPC bridge as a number, which is the security property:
 * it cannot become a shell fragment with a `kill` in front of it no matter what
 * the webview sends.
 *
 * `TERM` is the button. `KILL` exists but is deliberately second and is never
 * what a first click does — it gives the process no chance to flush, close
 * sockets or clean up, and on a dev host that routinely means a corrupted build
 * or a stale lock. Both confirm first, because this ends someone's work,
 * possibly someone else's: a shared dev box is exactly where a stray `kill`
 * hurts, and rmux is built for shared dev boxes.
 */

const REFRESH_MS = 4000;
/** `docker stats` alone takes seconds; polling it at REFRESH_MS overlaps. */
const DOCKER_REFRESH_MS = 8000;

type Tab = "processes" | "ports" | "docker" | "sessions";

const TABS: { id: Tab; label: string }[] = [
  { id: "processes", label: "PROCESSES" },
  { id: "ports", label: "PORTS" },
  { id: "docker", label: "DOCKER" },
  { id: "sessions", label: "SESSIONS" },
];

/**
 * Tabs, not a stack.
 *
 * These were laid out one above the other, and it did not survive contact: the
 * process list is the one you go to the host pane *for*, and it was squeezed
 * into a band tall enough for one line — permanently reading "READING…" while
 * the port list, which is much longer and far less urgent, took the rest of the
 * pane. Reported as "the UX here is too bad", which it was.
 *
 * Four lists that each want the whole height cannot share one column. Tabs also
 * mean only the visible one polls: each of these is an SSH round trip on a
 * timer, and stacking them meant paying for all of them to look at one.
 */
export function HostPanel({ target, serverId }: { target: TargetRef; serverId: ServerId }) {
  const [tab, setTab] = useState<Tab>("processes");

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className="flex shrink-0 items-center gap-1 border-b px-3 py-1"
        style={{ borderColor: "var(--border)" }}
      >
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            className="chip"
            aria-pressed={tab === t.id}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>

      {/* Only the chosen one is mounted, so the others stop polling — the "a
          widget that is switched off must not run" rule, applied to a pane. */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {tab === "processes" && <Processes target={target} />}
        {tab === "ports" && <Ports target={target} />}
        {tab === "docker" && <Docker target={target} />}
        {tab === "sessions" && <Sessions target={target} serverId={serverId} />}
      </div>
    </div>
  );
}

function Processes({ target }: { target: TargetRef }) {
  const [rows, setRows] = useState<ProcessInfo[] | null>(null);
  const [by, setBy] = useState<"cpu" | "memory">("cpu");
  const [filter, setFilter] = useState("");
  const [confirming, setConfirming] = useState<number | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      // Far more than the widget's five: this is the view you open *because*
      // the interesting process was not in the top five.
      setRows(await api.metricsProcesses(target, by, 40));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [target, by]);

  useEffect(() => {
    if (!isTauri()) return;
    void load();
    const timer = setInterval(load, REFRESH_MS);
    return () => clearInterval(timer);
  }, [load]);

  const signal = async (pid: number, hard: boolean) => {
    setBusy(pid);
    setError(null);
    setNote(null);
    try {
      await api.metricsKill(target, pid, hard);
      setNote(`Sent SIG${hard ? "KILL" : "TERM"} to ${pid}.`);
      setConfirming(null);
      // Re-read rather than removing the row: a process that ignored TERM is
      // still there, and showing it gone would be a lie the operator acts on.
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const shown = (rows ?? []).filter((p) =>
    filter ? p.name.toLowerCase().includes(filter.toLowerCase()) || String(p.pid) === filter : true,
  );

  return (
    <section className="flex shrink-0 flex-col">
      <header
        className="flex items-center gap-3 border-b px-4 py-2"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="kicker">PROCESSES</span>
        <input
          value={filter}
          spellCheck={false}
          placeholder="filter by name or pid"
          className="data inset px-2 py-[3px] text-[11px] outline-none"
          style={{ border: "1px solid var(--border)", color: "var(--text)", background: "transparent", width: 200 }}
          onChange={(e) => setFilter(e.target.value)}
        />
        <div className="seg ml-auto">
          {(["cpu", "memory"] as const).map((k) => (
            <button
              key={k}
              type="button"
              aria-pressed={by === k}
              onClick={() => setBy(k)}
            >
              {k.toUpperCase()}
            </button>
          ))}
        </div>
      </header>

      {note && (
        <p className="micro px-4 py-1" style={{ color: "rgb(var(--busy))" }}>
          {note.toUpperCase()}
        </p>
      )}
      {error && (
        <p role="alert" className="data px-4 py-1 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}

      {rows === null ? (
        <PanelLoader variant="rows" phase="READING PROCESSES" rows={5} />
      ) : shown.length === 0 ? (
        <p className="micro p-4">nothing matches</p>
      ) : (
        shown.map((p) => (
          <div
            key={p.pid}
            className="flex items-baseline gap-3 border-b px-4 py-[5px]"
            style={{ borderColor: "var(--border)" }}
          >
            <span className="data w-[64px] shrink-0 text-[11px]" style={{ color: "var(--text-soft)" }}>
              {p.pid}
            </span>
            <span className="data min-w-0 flex-1 truncate text-[11.5px]" style={{ color: "var(--text)" }}>
              {p.name}
            </span>
            <span className="data w-[56px] shrink-0 text-right text-[11px]" style={{ color: "var(--text-soft)" }}>
              {p.cpuPercent.toFixed(1)}%
            </span>
            <span className="data w-[56px] shrink-0 text-right text-[11px]" style={{ color: "var(--text-soft)" }}>
              {p.memoryPercent.toFixed(1)}%
            </span>

            {confirming === p.pid ? (
              <span className="flex shrink-0 items-center gap-2">
                {/* Red here is correct and is the one place in this panel it
                    appears: the operator is about to end something. */}
                <button
                  type="button"
                  className="chip"
                  disabled={busy !== null}
                  style={{ color: "rgb(var(--primary))" }}
                  onClick={() => void signal(p.pid, false)}
                >
                  {busy === p.pid ? "…" : "TERM"}
                </button>
                <button
                  type="button"
                  className="chip"
                  disabled={busy !== null}
                  style={{ color: "rgb(var(--primary))" }}
                  title="No chance to flush or clean up. Use TERM unless it has stopped responding."
                  onClick={() => void signal(p.pid, true)}
                >
                  KILL
                </button>
                <button type="button" className="chip" onClick={() => setConfirming(null)}>
                  CANCEL
                </button>
              </span>
            ) : (
              <button
                type="button"
                className="chip shrink-0"
                style={{ color: "var(--text-faint)" }}
                onClick={() => setConfirming(p.pid)}
              >
                END
              </button>
            )}
          </div>
        ))
      )}
    </section>
  );
}

/**
 * Ports on the target, and tunnels to them.
 *
 * Discovered rather than typed — requiring the number up front leaves the
 * manual step this exists to remove. A forward makes `localhost:<port>` on this
 * machine reach the same port over there, so the address you would have typed
 * anyway is the correct one; there is no proxy and no URL rewriting.
 *
 * The SOCKS proxy is the other half and is shown for what it is: rmux's own
 * webview cannot use it (there is one of it, and proxying it would route the
 * app's own interface through the operator's server), so the port is printed
 * for something else to point at.
 */
function Ports({ target }: { target: TargetRef }) {
  const [listening, setListening] = useState<ListeningPort[] | null>(null);
  const [forwards, setForwards] = useState<Forward[]>([]);
  const [manual, setManual] = useState("");
  const [busy, setBusy] = useState<number | null>(null);
  const [proxy, setProxy] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const local = !target.host;

  const refresh = useCallback(async () => {
    try {
      setForwards(await api.portsForwarded(target));
    } catch {
      // The list is a convenience; a failure here must not blank the panel.
    }
  }, [target]);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;

    void api
      .portsDiscover(target)
      .then((list) => !cancelled && setListening(list))
      .catch((e) => !cancelled && setError(e instanceof Error ? e.message : String(e)));
    void refresh();

    // Forwards settle asynchronously — ssh has to survive a grace window before
    // it counts as up — so the state is re-read rather than assumed.
    const timer = setInterval(refresh, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [target, refresh]);

  const stateOf = (port: number) => forwards.find((f) => f.port === port);

  const toggle = async (port: number) => {
    setBusy(port);
    setError(null);
    try {
      const open = stateOf(port);
      if (open && (open.state === "active" || open.state === "starting")) {
        await api.portUnforward(target, port);
      } else {
        await api.portForward(target, port);
      }
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const row = (port: number, process?: string) => {
    const forward = stateOf(port);
    const on = forward?.state === "active" || forward?.state === "starting";

    return (
      <div
        key={port}
        className="flex items-baseline gap-3 border-b px-4 py-[5px]"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="data w-[64px] shrink-0 text-[11px]" style={{ color: "var(--text)" }}>
          {port}
        </span>
        <span className="data min-w-0 flex-1 truncate text-[11px]" style={{ color: "var(--text-soft)" }}>
          {process || "—"}
        </span>

        {forward?.state === "failed" ? (
          // The reason, not just the failure. It is almost always "that local
          // port is already taken", which the operator can only act on if told.
          <span className="data shrink-0 text-[10.5px]" style={{ color: "rgb(var(--primary))" }}>
            {forward.error ?? "failed"}
          </span>
        ) : on ? (
          <span className="micro shrink-0" style={{ color: "rgb(var(--busy))" }}>
            localhost:{port}
          </span>
        ) : null}

        <button
          type="button"
          className="chip w-[72px] shrink-0 text-right"
          disabled={busy === port || local}
          style={{ color: on ? "var(--text)" : "var(--text-faint)" }}
          onClick={() => void toggle(port)}
        >
          {busy === port ? "…" : on ? "STOP" : "FORWARD"}
        </button>
      </div>
    );
  };

  const extra = Number(manual);
  const canAdd = Number.isInteger(extra) && extra > 0 && extra < 65536;

  return (
    <section className="flex shrink-0 flex-col">
      <header
        className="flex items-center gap-3 border-b border-t px-4 py-2"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="kicker">PORTS</span>
        <span className="micro">{target.host ?? "this machine"}</span>

        {!local && (
          <button
            type="button"
            className="chip ml-auto"
            onClick={() =>
              void api
                .portProxy(target)
                .then(setProxy)
                .catch((e) => setError(e instanceof Error ? e.message : String(e)))
            }
          >
            {proxy ? `SOCKS5H://127.0.0.1:${proxy}` : "OPEN SOCKS PROXY"}
          </button>
        )}
      </header>

      {local && (
        <p className="data px-4 py-2 text-[11px]" style={{ color: "var(--text-soft)" }}>
          This session runs on your own machine, so its ports are already local — there is nothing
          to forward.
        </p>
      )}

      {proxy && (
        <p className="data px-4 py-2 text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Every port on <span style={{ color: "var(--text)" }}>{target.host}</span> is reachable
          through <span style={{ color: "var(--text)" }}>socks5h://127.0.0.1:{proxy}</span>, DNS
          included — so an internal hostname resolves over there rather than failing here. Point a
          browser profile or a tool at it; rmux's own window cannot use it, because there is only
          one of it and proxying it would route this interface through your server.
        </p>
      )}

      {error && (
        <p role="alert" className="data px-4 py-1 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}

      {listening === null && !local ? (
        <p className="micro p-4">looking…</p>
      ) : (
        <>
          {(listening ?? []).map((p) => row(p.port, p.process))}
          {/* Forwards for ports that stopped listening, or were typed in — they
              are still open and must remain closable. */}
          {forwards
            .filter((f) => !(listening ?? []).some((l) => l.port === f.port))
            .map((f) => row(f.port))}
          {!local && (listening ?? []).length === 0 && (
            <p className="micro px-4 py-2">
              nothing is listening above port 1024 — or the host has neither `ss` nor `netstat`
            </p>
          )}
        </>
      )}

      {!local && (
        <div className="flex items-center gap-2 px-4 py-2">
          <input
            value={manual}
            inputMode="numeric"
            placeholder="port"
            className="data inset px-2 py-[3px] text-[11px] outline-none"
            style={{ border: "1px solid var(--border)", color: "var(--text)", background: "transparent", width: 90 }}
            onChange={(e) => setManual(e.target.value.replace(/\D/g, ""))}
            onKeyDown={(e) => {
              if (e.key === "Enter" && canAdd) {
                void toggle(extra);
                setManual("");
              }
            }}
          />
          <button
            type="button"
            className="btn"
            disabled={!canAdd}
            onClick={() => {
              void toggle(extra);
              setManual("");
            }}
          >
            Forward
          </button>
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            FOR ANYTHING DISCOVERY MISSED
          </span>
        </div>
      )}
    </section>
  );
}

/** `1.4 GB`, `934 MB` — bytes, at the scale a person reads them. */
function gb(bytes?: number | null): string {
  if (bytes == null) return "—";
  if (bytes >= 1_000_000_000) return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
  if (bytes >= 1_000_000) return `${Math.round(bytes / 1_000_000)} MB`;
  return `${Math.round(bytes / 1000)} kB`;
}

/**
 * Containers on the host, and the three verbs that matter.
 *
 * Stopped containers are listed too. The operator wants to *start* one at least
 * as often as stop it, and a container that is not on screen is one they cannot
 * start — so hiding them would remove half the reason for the tab.
 *
 * **Stop and restart confirm; start does not.** Starting a container is
 * reversible and expected; stopping one interrupts whatever it was serving, on
 * a machine that is frequently shared. Same asymmetry as the process list's
 * TERM.
 */
function Docker({ target }: { target: TargetRef }) {
  const [report, setReport] = useState<DockerReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const inFlight = useRef(false);

  /**
   * One request at a time.
   *
   * `docker stats --no-stream` was measured at **4.4 seconds** against a real
   * host — longer than the 4s refresh that was driving it, so every tick
   * started a request before the previous had answered. Overlapping reads of a
   * remote daemon pile up on one SSH connection and the newest answer is not
   * necessarily the last to arrive, so the list could go backwards.
   */
  const load = useCallback(async () => {
    if (!isTauri() || inFlight.current) return;
    inFlight.current = true;
    try {
      setReport(await api.dockerContainers(target));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      inFlight.current = false;
    }
  }, [target]);

  useEffect(() => {
    void load();
    // Slower than the other tabs on purpose: this is two docker commands on a
    // remote daemon, not a read of /proc.
    const timer = setInterval(() => void load(), DOCKER_REFRESH_MS);
    return () => clearInterval(timer);
  }, [load]);

  const act = async (id: string, action: "start" | "stop" | "restart") => {
    setBusy(id);
    setError(null);
    setConfirming(null);
    try {
      await api.dockerAction(target, id, action);
      await load();
    } catch (e) {
      // Kept until the next attempt: a container that refused to stop and said
      // nothing looks exactly like one that stopped.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  if (error && !report) {
    return <p className="data px-4 py-3 text-[11px]" style={{ color: "rgb(var(--primary))" }}>{error}</p>;
  }
  if (!report) return <PanelLoader variant="rows" phase="READING CONTAINERS" rows={4} />;

  // Said outright rather than shown as an empty list — "docker is not installed"
  // and "no containers running" are different facts with different answers.
  if ("unavailable" in report) {
    return (
      <p className="data px-4 py-3 text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        {report.unavailable.reason}
      </p>
    );
  }

  const containers = report.containers;
  if (!containers.length) {
    return <p className="micro px-4 py-3" style={{ color: "var(--text-faint)" }}>no containers on this host</p>;
  }

  return (
    <div className="flex flex-col">
      {error && (
        <p className="data px-4 py-1 text-[11px]" style={{ color: "rgb(var(--primary))" }}>{error}</p>
      )}
      {containers.map((c) => (
        <div
          key={c.id}
          className="flex items-center gap-3 border-b px-4 py-[6px]"
          style={{ borderColor: "var(--border)" }}
        >
          {/* Monochrome for running, faint for stopped. Rule 0: a stopped
              container is a state, not something to act on. */}
          <span
            className="round shrink-0"
            title={c.status}
            style={{ width: 7, height: 7, background: c.running ? "rgb(var(--busy))" : "var(--text-faint)" }}
          />
          <div className="flex min-w-0 flex-1 flex-col">
            <span className="data truncate text-[12px]" style={{ color: "var(--text)" }}>{c.name}</span>
            <span className="micro truncate" style={{ color: "var(--text-faint)" }}>
              {c.image} · {c.status}
            </span>
          </div>
          <span className="data w-[64px] shrink-0 text-right text-[11px] tabular-nums" style={{ color: "var(--text-soft)" }}>
            {c.cpu != null ? `${c.cpu.toFixed(1)}%` : "—"}
          </span>
          <span className="data w-[76px] shrink-0 text-right text-[11px] tabular-nums" style={{ color: "var(--text-soft)" }}>
            {gb(c.memory)}
          </span>

          <div className="flex shrink-0 gap-1">
            {confirming === c.id ? (
              <>
                <button
                  type="button"
                  className="chip"
                  style={{ color: "rgb(var(--primary))" }}
                  disabled={busy === c.id}
                  onClick={() => void act(c.id, c.running ? "stop" : "start")}
                >
                  {c.running ? "stop it" : "start it"}
                </button>
                <button type="button" className="chip" onClick={() => setConfirming(null)}>cancel</button>
              </>
            ) : (
              <>
                {c.running ? (
                  <button type="button" className="chip" disabled={busy === c.id} onClick={() => setConfirming(c.id)}>
                    {busy === c.id ? "…" : "STOP"}
                  </button>
                ) : (
                  // Starting is reversible and expected, so it goes straight
                  // through; stopping interrupts something and does not.
                  <button type="button" className="chip" disabled={busy === c.id} onClick={() => void act(c.id, "start")}>
                    {busy === c.id ? "…" : "START"}
                  </button>
                )}
                {c.running && (
                  <button type="button" className="chip" disabled={busy === c.id} onClick={() => setConfirming(c.id)}>
                    RESTART
                  </button>
                )}
              </>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * Claude and shell sessions the agent is holding on this host.
 *
 * This is the list that makes an abandoned session findable. From the outside a
 * Claude session is a login shell, indistinguishable from any other process, so
 * `ps` cannot answer "what is still running" — only the daemon knows the names.
 *
 * **Age and attachment are the two columns that matter**: together they separate
 * "I am using this from my other machine" from "this was left behind three days
 * ago", which is the whole question.
 */
/**
 * What is running on the host, and the way back into it.
 *
 * This list already existed; what is new is that its rows are **actionable**.
 * A session started here, detached, or started from another machine entirely
 * shows up the same way — the daemon does not care which laptop opened it — so
 * this is also how you pick up work from a different computer.
 *
 * The alternative was a second picker reached from the rail, which is what
 * PR #9 built. It would have meant a second command listing the same sessions
 * and a second surface to keep in step with this one. A list of what is running
 * is the natural place to put "attach to it".
 */
function Sessions({ target, serverId }: { target: TargetRef; serverId: ServerId }) {
  const [rows, setRows] = useState<AgentSession[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attaching, setAttaching] = useState<string | null>(null);

  const adoptServerSession = useWorkspace((s) => s.adoptServerSession);
  // Selected whole, then derived — a selector that filters returns a fresh array
  // every call, which loops `useSyncExternalStore` into a white screen. The rail
  // carries the same warning for the same reason.
  const sessions = useWorkspace((s) => s.sessions);
  const attachedHere = useMemo(
    () => new Set(sessions.map((s) => reattachName(s))),
    [sessions],
  );

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      setRows(await api.hostSessions(target));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [target]);

  useEffect(() => {
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    return () => clearInterval(timer);
  }, [load]);

  if (error) {
    return <p className="data px-4 py-3 text-[11px] leading-relaxed" style={{ color: "rgb(var(--primary))" }}>{error}</p>;
  }
  if (!rows) return <PanelLoader variant="rows" phase="READING SESSIONS" rows={4} />;
  if (!rows.length) {
    return <p className="micro px-4 py-3" style={{ color: "var(--text-faint)" }}>no sessions running on this host</p>;
  }

  return (
    <div className="flex flex-col">
      {rows.map((s) => (
        <div key={s.name} className="flex items-center gap-3 border-b px-4 py-[6px]" style={{ borderColor: "var(--border)" }}>
          <span
            className="round shrink-0"
            title={s.attached ? "a client is attached" : "detached"}
            style={{ width: 7, height: 7, background: s.attached ? "rgb(var(--busy))" : "var(--text-faint)" }}
          />
          <div className="flex min-w-0 flex-1 flex-col">
            {/* The alias leads when there is one — it is the name a person
                chose, possibly at another computer — with the key underneath,
                because the key is what `rmux-agent kill` and a bug report need. */}
            <span className="data truncate text-[12px]" style={{ color: "var(--text)" }}>
              {s.alias ?? s.name}
            </span>
            <span className="micro truncate" style={{ color: "var(--text-faint)" }}>
              {s.alias ? `${s.name} · ` : ""}
              {s.attached ? "attached" : "detached"} · {age(s.ageSeconds)}
              {s.pid ? ` · pid ${s.pid}` : ""}
            </span>
          </div>
          <span className="data w-[64px] shrink-0 text-right text-[11px] tabular-nums" style={{ color: "var(--text-soft)" }}>
            {s.cpu != null ? `${s.cpu.toFixed(1)}%` : "—"}
          </span>
          <span className="data w-[76px] shrink-0 text-right text-[11px] tabular-nums" style={{ color: "var(--text-soft)" }}>
            {gb(s.memory)}
          </span>

          {/* Present as a state, not a disabled button: a greyed ATTACH invites
              "how do I enable this", while a plain label answers it. */}
          {attachedHere.has(s.name) ? (
            <span className="micro shrink-0" style={{ color: "var(--text-faint)" }}>
              IN RAIL
            </span>
          ) : (
            <button
              type="button"
              className="chip shrink-0"
              disabled={attaching !== null}
              title={`Open ${s.name} in the rail — it keeps running either way`}
              onClick={() => {
                setAttaching(s.name);
                // Kind comes from the command the daemon reports: an adopted
                // session's *name* was chosen by whoever started it and carries
                // no reliable prefix.
                adoptServerSession(
                  serverId,
                  s.name,
                  isClaudeSession(s.command) ? "claude" : "terminal",
                  s.alias,
                );
                setAttaching(null);
              }}
            >
              {attaching === s.name ? "ATTACHING…" : "ATTACH"}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

/** `3h`, `2d` — how long this session has been alive. */
function age(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  if (seconds < 86_400) return `${Math.round(seconds / 3600)}h`;
  return `${Math.round(seconds / 86_400)}d`;
}
