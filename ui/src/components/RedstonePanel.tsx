import { useCallback, useEffect, useState } from "react";

import {
  api,
  isTauri,
  type RedstoneCapabilities,
  type RedstoneHost,
  type SessionView,
} from "../lib/api";

/**
 * Letting Redstone drive a host's Claude sessions.
 *
 * ## Two fields, and one of them is optional
 *
 * Type the Redstone address, sign in the way you already do, press Enrol. That
 * is the whole flow — the first version asked for a bridge endpoint *and* a
 * pasted token, which put the operator in the position of carrying a credential
 * by hand between two apps that can talk to each other perfectly well.
 *
 * Sign-in opens **Redstone's own login page** in a window, which is what its
 * desktop specification prescribes: no native login form, and no password ever
 * typed into rmux. That window loads a remote origin, so it has no access to any
 * rmux command — a remote domain must be listed in `dangerousRemoteDomainIpcAccess`
 * to reach one, and rmux lists none.
 *
 * ## Why the token path is still here
 *
 * A self-hosted deployment may be reachable by a token its admin can read while
 * its web login is behind an SSO rmux cannot render. That is a real situation
 * and it costs one collapsed section to support, so it stays — below, folded
 * away, where it is not the first thing anyone sees.
 *
 * ## Enrolment is per host and deliberate
 *
 * There is no "enrol everything". Which machines an outside service may drive is
 * exactly the decision to make one at a time. The blast radius is stated on the
 * panel rather than left to be inferred.
 */

/** Remembered so a second host does not mean typing the address again. */
const ADDRESS_KEY = "rmux.redstone.address";

export function RedstonePanel() {
  const [address, setAddress] = useState(() => localStorage.getItem(ADDRESS_KEY) ?? "");
  const [session, setSession] = useState<SessionView | null>(null);
  const [host, setHost] = useState("");

  const [status, setStatus] = useState<RedstoneHost | null>(null);
  const [caps, setCaps] = useState<RedstoneCapabilities | null>(null);
  const [busy, setBusy] = useState<
    null | "signing" | "checking" | "enrolling" | "removing"
  >(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  // The fallback, folded away. Open only if someone goes looking for it.
  const [manual, setManual] = useState(false);
  const [endpoint, setEndpoint] = useState("");
  const [token, setToken] = useState("");

  const target = { host: host.trim() || undefined };
  const label = host.trim() || "this machine";

  /** Errors persist until the next attempt; successes fade. */
  const report = (message: string) => {
    setDone(message);
    setTimeout(() => setDone((d) => (d === message ? null : d)), 2500);
  };

  const run = async (
    phase: NonNullable<typeof busy>,
    work: () => Promise<string | null>,
  ) => {
    setBusy(phase);
    setError(null);
    setDone(null);
    try {
      const message = await work();
      if (message) report(message);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  // Whatever rmux is already signed in to, so reopening Settings does not look
  // like a fresh install.
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    api
      .redstoneSession()
      .then((s) => {
        if (cancelled || !s) return;
        setSession(s);
        setAddress((a) => a || s.baseUrl);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // Asked before any control is offered, so one that cannot work is *absent*
  // rather than disabled — a greyed switch invites "how do I enable this".
  useEffect(() => {
    const typed = address.trim();
    if (!isTauri() || !typed) return setCaps(null);
    const base = typed.startsWith("http") ? typed : `https://${typed}`;

    let cancelled = false;
    api
      .redstoneCapabilities(base.replace(/\/+$/, ""))
      .then((c) => !cancelled && setCaps(c))
      .catch(() => !cancelled && setCaps(null));
    return () => {
      cancelled = true;
    };
  }, [address]);

  const check = useCallback(
    () =>
      run("checking", async () => {
        setStatus(await api.redstoneHostStatus({ host: host.trim() || undefined }));
        return null;
      }),
    [host],
  );

  const signIn = () =>
    run("signing", async () => {
      const s = await api.redstoneSignIn(address.trim());
      localStorage.setItem(ADDRESS_KEY, address.trim());
      setSession(s);
      return `SIGNED IN${s.user ? ` AS ${s.user.toUpperCase()}` : ""}`;
    });

  const signOut = () =>
    run("signing", async () => {
      await api.redstoneSignOut();
      setSession(null);
      return "SIGNED OUT";
    });

  const enrol = () =>
    run("enrolling", async () => {
      const next = manual
        ? await api.redstoneEnrolWithToken(target, endpoint.trim(), token.trim())
        : await api.redstoneEnrol(target);
      setStatus(next);
      // Not kept. It has been written to the host, and this window has no reason
      // to hold a credential it will never send again.
      setToken("");
      return next.running ? "ENROLLED — BRIDGE RUNNING" : "ENROLLED — BRIDGE NOT RUNNING";
    });

  const unenrol = () =>
    run("removing", async () => {
      setStatus(await api.redstoneUnenrol(target));
      return "REMOVED — TOKEN DELETED AND BRIDGE STOPPED";
    });

  const canSignIn = !!address.trim() && !busy && isTauri();
  const canEnrol =
    !busy &&
    isTauri() &&
    (manual ? !!endpoint.trim() && !!token.trim() : !!session);

  return (
    <section className="flex max-w-[560px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">REDSTONE</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Let Redstone&rsquo;s agent see and drive the Claude sessions on a server. It runs on the
          host itself, so it keeps working with rmux closed &mdash; a session you started this
          morning is still reachable after you shut the lid.
        </p>
      </header>

      {/* 1 — where */}
      <label className="flex flex-col gap-1">
        <span className="micro">REDSTONE ADDRESS</span>
        <input
          className="field data"
          placeholder="redstone.example"
          value={address}
          spellCheck={false}
          disabled={!!session}
          onChange={(e) => setAddress(e.target.value)}
        />
        <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
          Just the address. rmux works out the rest and asks the deployment what it supports.
        </span>
      </label>

      {caps && !caps.bridge && (
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--warn)" }}>
          That deployment does not offer the rmux bridge, so there is nothing to enrol against
          yet.
        </p>
      )}

      {/* 2 — who */}
      <div className="flex flex-wrap items-center gap-3">
        {session ? (
          <>
            <span className="data text-[11px]" style={{ color: "var(--text)" }}>
              Signed in{session.user ? ` as ${session.user}` : ""}
            </span>
            <button type="button" className="btn" disabled={!!busy} onClick={() => void signOut()}>
              Sign out
            </button>
          </>
        ) : (
          <button type="button" className="btn" disabled={!canSignIn} onClick={() => void signIn()}>
            {busy === "signing" ? "waiting for sign-in…" : "Sign in to Redstone"}
          </button>
        )}
      </div>

      {busy === "signing" && !session && (
        <span className="data text-[10px]" style={{ color: "var(--text-faint)" }}>
          A window has opened on Redstone&rsquo;s own login page. rmux never sees your password.
        </span>
      )}

      {/* 3 — which machine */}
      {(session || manual) && (
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
          <button type="button" className="btn" disabled={!!busy} onClick={() => void unenrol()}>
            {busy === "removing" ? "removing…" : "Remove"}
          </button>
        )}
      </div>

      {done && <span className="micro">{done}</span>}
      {error && (
        <span className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </span>
      )}

      {status && (
        <div className="flex flex-col gap-1 border p-3" style={{ borderColor: "var(--border)" }}>
          <span className="micro">{label.toUpperCase()}</span>
          {status.enrolled ? (
            <>
              {/*
                Two facts, not one. A host whose token is present but whose bridge
                died is the failure that otherwise reads as success.
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

      {/* The fallback, folded away rather than absent — see the module note. */}
      <div className="flex flex-col gap-2 border-t pt-4" style={{ borderColor: "var(--border)" }}>
        <button
          type="button"
          className="micro self-start"
          style={{ color: "var(--text-soft)" }}
          onClick={() => setManual((m) => !m)}
        >
          {manual ? "− " : "+ "}ENROL WITH A TOKEN INSTEAD
        </button>
        {manual && (
          <>
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
              For a deployment whose web sign-in rmux cannot render. Paste what its admin screen
              gives you.
            </span>
            <input
              className="field data"
              placeholder="wss://redstone.example/api/v1/rmux/bridge"
              value={endpoint}
              spellCheck={false}
              onChange={(e) => setEndpoint(e.target.value)}
            />
            <input
              className="field data"
              type="password"
              placeholder="rbt_…"
              value={token}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setToken(e.target.value)}
            />
          </>
        )}
      </div>

      <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
        What an enrolled host allows, and nothing more: list what is running, read a conversation,
        send a message to one, interrupt it, and start a new Claude in a folder that already
        exists. There is no way to run a command &mdash; anything Claude runs still goes through
        its own permission prompts, where you can watch and interrupt it. The bridge has no
        privilege beyond the account it runs as, and each host&rsquo;s token can be revoked on its
        own.
      </p>
    </section>
  );
}
