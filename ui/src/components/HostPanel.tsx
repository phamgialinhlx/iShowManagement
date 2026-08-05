import { useCallback, useEffect, useState } from "react";

import { api, isTauri, type Forward, type ListeningPort, type ProcessInfo, type TargetRef } from "../lib/api";

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

export function HostPanel({ target }: { target: TargetRef }) {
  return (
    <div className="flex h-full min-h-0 flex-col overflow-y-auto">
      <Processes target={target} />
      <Ports target={target} />
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
        <p className="micro p-4">reading…</p>
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
