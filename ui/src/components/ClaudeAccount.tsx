import { useEffect, useState } from "react";

import { api, isTauri, type ClaudeAccount as Account, type UsageReport } from "../lib/api";
import { compactTokens } from "../lib/context-window";

/**
 * Real organisation usage, read on demand.
 *
 * On demand and never polled: this is a billing API, and the figure moves on the
 * scale of hours. It also needs an **admin key**, which is a different and more
 * powerful credential than the one that runs sessions — so it is asked for
 * separately and never leaves this machine.
 */
function OrgUsage({ available }: { available: boolean }) {
  const [report, setReport] = useState<UsageReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!available) {
    return (
      <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        Add an admin key (sk-ant-admin…) to read real organisation usage.
      </p>
    );
  }

  const load = async () => {
    setBusy(true);
    setError(null);
    try {
      setReport(await api.claudeUsageReport(7));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2">
        <span className="micro shrink-0">ORG · {report ? `${report.days}D` : "USAGE"}</span>
        <button
          type="button"
          className="micro"
          disabled={busy}
          onClick={() => void load()}
          style={{ color: busy ? "var(--text-faint)" : "var(--text)" }}
        >
          {busy ? "reading…" : report ? "refresh" : "read"}
        </button>
      </div>

      {report && (
        <>
          <Line label="OUT" value={compactTokens(report.output)} />
          <Line label="IN" value={compactTokens(report.uncachedInput)} />
          <Line label="CACHED" value={compactTokens(report.cacheRead)} />
          {report.byModel.slice(0, 3).map((m) => (
            <Line
              key={m.model}
              label={m.model.replace(/^claude-/, "").slice(0, 14)}
              value={compactTokens(m.output)}
              dim
            />
          ))}
        </>
      )}

      {error && (
        <p role="alert" className="data text-[10px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}
    </div>
  );
}

function Line({ label, value, dim }: { label: string; value: string; dim?: boolean }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="micro shrink-0">{label}</span>
      <span
        className="data text-[10.5px]"
        style={{ color: dim ? "var(--text-soft)" : "var(--text)" }}
      >
        {value}
      </span>
    </div>
  );
}

/**
 * The Claude account rmux signs sessions in with.
 *
 * The point is one login rather than one per host. `claude setup-token` produces
 * a long-lived token; rmux keeps it in the OS keychain and hands it to each
 * host's agent as a session starts, so a new server works immediately instead of
 * needing its own browser login.
 *
 * The token itself never reaches this component — only the last few characters,
 * enough to tell two accounts apart. A credential in the webview is one XSS away
 * from leaving the machine, and there is nothing here that needs it.
 *
 * Pasting a token is offered alongside signing in because `setup-token` prints it
 * to a terminal, and someone who already has one should not have to run the flow
 * again to use it here.
 */
export function ClaudeAccountWidget() {
  const [account, setAccount] = useState<Account | null>(null);
  const [pasting, setPasting] = useState(false);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    api
      .claudeAccountStatus()
      .then((a) => !cancelled && setAccount(a))
      .catch(() => !cancelled && setAccount({ connected: false, usageAvailable: false }));
    return () => {
      cancelled = true;
    };
  }, []);

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await api.claudeAccountSave(token);
      setAccount(next);
      setToken("");
      setPasting(false);
      setSaved(true);
      setTimeout(() => setSaved(false), 2500);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const forget = async () => {
    setBusy(true);
    setError(null);
    try {
      setAccount(await api.claudeAccountForget());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-baseline justify-between gap-2">
        <span className="micro shrink-0">ACCOUNT</span>
        <span
          className="data truncate text-[10.5px]"
          style={{ color: account?.connected ? "var(--text)" : "var(--text-faint)" }}
        >
          {account === null ? "—" : account.connected ? (account.hint ?? "signed in") : "host login"}
        </span>
      </div>

      {account?.connected && account.kind && (
        <div className="flex items-baseline justify-between gap-2">
          <span className="micro shrink-0">TYPE</span>
          <span className="data text-[10.5px]" style={{ color: "var(--text-soft)" }}>
            {account.kind === "apiKey"
              ? "console key"
              : account.kind === "oauthToken"
                ? "subscription"
                : "admin"}
          </span>
        </div>
      )}

      <OrgUsage available={account?.usageAvailable ?? false} />

      {/* Not signed in through rmux is a normal, working state — a host may
          already have its own login. So this never reads as an error. */}
      {account !== null && !account.connected && !pasting && (
        <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Sessions use whatever account each host is signed in to. Add a credential to use one
          account everywhere.
        </p>
      )}

      {pasting ? (
        <div className="flex flex-col gap-1">
          <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            A Console API key, or the output of{" "}
            <span style={{ color: "var(--text)" }}>claude setup-token</span>.
          </p>
          <input
            autoFocus
            type="password"
            value={token}
            spellCheck={false}
            placeholder="sk-ant-api… / oat… / admin…"
            aria-label="Claude credential"
            className="data inset px-1 py-[2px] text-[10.5px] outline-none"
            style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
            onChange={(e) => setToken(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && token.trim()) void save();
              if (e.key === "Escape") {
                setPasting(false);
                setToken("");
              }
            }}
          />
          <div className="flex gap-2">
            <button
              type="button"
              className="micro"
              disabled={busy || !token.trim()}
              onClick={() => void save()}
              style={{ color: token.trim() ? "var(--text)" : "var(--text-faint)" }}
            >
              {busy ? "saving…" : "save"}
            </button>
            <button
              type="button"
              className="micro"
              onClick={() => {
                setPasting(false);
                setToken("");
              }}
            >
              cancel
            </button>
          </div>
        </div>
      ) : (
        <div className="flex gap-2">
          <button type="button" className="micro" onClick={() => setPasting(true)}>
            {account?.connected ? "replace" : "add credential"}
          </button>
          {account?.connected && (
            <button
              type="button"
              className="micro"
              disabled={busy}
              onClick={() => void forget()}
              style={{ color: "var(--text-faint)" }}
            >
              forget
            </button>
          )}
          {saved && <span className="micro" style={{ color: "var(--text)" }}>saved</span>}
        </div>
      )}

      {error && (
        <p role="alert" className="data text-[10px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}
    </div>
  );
}
