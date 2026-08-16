import { useCallback, useEffect, useState } from "react";

import { api, isTauri, type RedstoneCapabilities, type RedstoneHost } from "../lib/api";

/**
 * Letting Redstone drive a host's Claude sessions.
 *
 * rmux's part is small and entirely front-loaded: write a token to the host and
 * start `rmux-agent bridge` there. After that rmux is not in the path at all —
 * the bridge dials Redstone itself and keeps working with this app closed, which
 * is the whole point.
 *
 * ## Why this asks for a pasted token rather than a sign-in
 *
 * A host only ever needs an endpoint and a token. Whether Redstone handed those
 * to rmux over HTTP or to a person through its web UI makes no difference to
 * anything downstream, so requiring a sign-in would gate the feature on an OAuth
 * flow a self-hosted deployment may simply not have enabled. Minting is the
 * convenience; this is the mechanism.
 *
 * ## Enrolment is per host and deliberate
 *
 * There is no "enrol everything". Which machines an outside service may drive is
 * exactly the decision to make one at a time — a checkbox that quietly enrolled
 * every host in `~/.ssh/config` is the kind of default nobody remembers agreeing
 * to. The blast radius is stated on the panel rather than left to be inferred.
 */

/** Remembered so the second host does not mean typing the URL again. */
const ENDPOINT_KEY = "rmux.redstone.endpoint";

export function RedstonePanel() {
  const [endpoint, setEndpoint] = useState(() => localStorage.getItem(ENDPOINT_KEY) ?? "");
  const [token, setToken] = useState("");
  const [host, setHost] = useState("");

  const [status, setStatus] = useState<RedstoneHost | null>(null);
  const [caps, setCaps] = useState<RedstoneCapabilities | null>(null);
  const [busy, setBusy] = useState<null | "checking" | "enrolling" | "removing">(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const target = { host: host.trim() || undefined };

  /** Errors persist until the next attempt; successes fade. */
  const report = (message: string) => {
    setDone(message);
    setTimeout(() => setDone((d) => (d === message ? null : d)), 2500);
  };

  const check = useCallback(async () => {
    if (!isTauri()) return;
    setBusy("checking");
    setError(null);
    try {
      setStatus(await api.redstoneHostStatus({ host: host.trim() || undefined }));
    } catch (e) {
      setStatus(null);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [host]);

  // Asked once for the deployment, so a control that cannot work is *absent*
  // rather than disabled — a greyed switch invites "how do I enable this", a
  // missing one asks nothing.
  useEffect(() => {
    const url = endpoint.trim();
    if (!isTauri() || !url) return setCaps(null);

    // The bridge endpoint is a websocket URL; the config probe is HTTP on the
    // same origin. Derived rather than asked for twice — two fields that must
    // agree is two chances to mistype one.
    const base = url.replace(/^ws/, "http").replace(/\/api\/v1\/rmux\/bridge\/?$/, "");
    let cancelled = false;
    api
      .redstoneCapabilities(base)
      .then((c) => !cancelled && setCaps(c))
      .catch(() => !cancelled && setCaps(null));
    return () => {
      cancelled = true;
    };
  }, [endpoint]);

  const enrol = async () => {
    setBusy("enrolling");
    setError(null);
    setDone(null);
    try {
      const next = await api.redstoneEnrolWithToken(target, endpoint.trim(), token.trim());
      localStorage.setItem(ENDPOINT_KEY, endpoint.trim());
      setStatus(next);
      // The token is not kept here. It has been written to the host, and this
      // window has no reason to hold a credential it will never send again.
      setToken("");
      report(next.running ? "ENROLLED — BRIDGE RUNNING" : "ENROLLED — BRIDGE NOT RUNNING");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const unenrol = async () => {
    setBusy("removing");
    setError(null);
    setDone(null);
    try {
      setStatus(await api.redstoneUnenrol(target));
      report("REMOVED — TOKEN DELETED AND BRIDGE STOPPED");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const label = host.trim() || "this machine";
  const canEnrol = !!endpoint.trim() && !!token.trim() && !busy && isTauri();

  return (
    <section className="flex max-w-[560px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">REDSTONE</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Let Redstone&rsquo;s agent see and drive the Claude sessions on a server. It runs on the
          host itself, so it keeps working with rmux closed &mdash; which is the point: a session
          you started this morning is still reachable after you shut the lid.
        </p>
      </header>

      <div className="flex flex-col gap-3">
        <label className="flex flex-col gap-1">
          <span className="micro">BRIDGE ENDPOINT</span>
          <input
            className="field data"
            placeholder="wss://redstone.example/api/v1/rmux/bridge"
            value={endpoint}
            spellCheck={false}
            onChange={(e) => setEndpoint(e.target.value)}
          />
          <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
            From Redstone, when it minted the host. Copy it exactly &mdash; a deployment may serve
            websockets from somewhere other than its web address.
          </span>
        </label>

        <label className="flex flex-col gap-1">
          <span className="micro">HOST TOKEN</span>
          <input
            className="field data"
            type="password"
            placeholder="rbt_…"
            value={token}
            spellCheck={false}
            autoComplete="off"
            onChange={(e) => setToken(e.target.value)}
          />
          <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
            Belongs to one machine and can be revoked on its own. rmux writes it to the host over
            stdin, never as a command-line argument &mdash; <code>ps</code> shows one user&rsquo;s
            command line to every account on a server.
          </span>
        </label>

        <label className="flex flex-col gap-1">
          <span className="micro">SERVER</span>
          <input
            className="field data"
            placeholder="an ssh alias, or blank for this machine"
            value={host}
            spellCheck={false}
            onChange={(e) => setHost(e.target.value)}
          />
          <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
            The name as <code>~/.ssh/config</code> knows it. rmux passes it to <code>ssh</code>{" "}
            verbatim and never resolves that file itself.
          </span>
        </label>
      </div>

      {/*
        Never show a control that cannot work. A deployment without the bridge
        gets a sentence rather than a dead button.
      */}
      {caps && !caps.bridge && (
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--warn)" }}>
          That deployment does not offer the rmux bridge. Its{" "}
          <code>/api/v1/rmux/config</code> reports it is not available, so there is nothing to
          enrol against yet.
        </p>
      )}

      <div className="flex flex-wrap items-center gap-3">
        <button type="button" className="btn" disabled={!canEnrol} onClick={() => void enrol()}>
          {busy === "enrolling" ? "enrolling…" : `Enrol ${label}`}
        </button>
        <button
          type="button"
          className="btn"
          disabled={!isTauri() || !!busy}
          onClick={() => void check()}
        >
          {busy === "checking" ? "checking…" : "Check"}
        </button>
        {status?.enrolled && (
          <button
            type="button"
            className="btn"
            disabled={!!busy}
            onClick={() => void unenrol()}
          >
            {busy === "removing" ? "removing…" : "Remove"}
          </button>
        )}
      </div>

      {/* Inline, beside the control that caused it. */}
      {done && <span className="micro">{done}</span>}
      {error && (
        <span className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </span>
      )}

      {status && (
        <div
          className="flex flex-col gap-1 border p-3"
          style={{ borderColor: "var(--border)" }}
        >
          <span className="micro">{label.toUpperCase()}</span>
          {status.enrolled ? (
            <>
              {/*
                Two facts, not one. A host whose token is present but whose
                bridge died is the failure that otherwise reads as success, and
                it is the only one worth putting on screen separately.
              */}
              <span className="data text-[11px]" style={{ color: "var(--text)" }}>
                Enrolled ·{" "}
                <span style={{ color: status.running ? "var(--text)" : "var(--warn)" }}>
                  {status.running ? "bridge running" : "bridge not running"}
                </span>
              </span>
              {status.hostId && (
                <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
                  {status.hostId}
                </span>
              )}
              {status.endpoint && (
                <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
                  {status.endpoint}
                </span>
              )}
            </>
          ) : (
            <span className="data text-[11px]" style={{ color: "var(--text-soft)" }}>
              Not enrolled. Redstone cannot see this machine.
            </span>
          )}
        </div>
      )}

      <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
        What an enrolled host allows, and nothing more: list what is running, read a conversation,
        send a message to one, interrupt it, and start a new Claude in a folder that already
        exists. There is no way to run a command &mdash; anything Claude runs still goes through
        its own permission prompts, where you can watch and interrupt it. The bridge has no
        privilege beyond the account it runs as.
      </p>
    </section>
  );
}
