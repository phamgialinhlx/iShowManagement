import { useEffect, useState } from "react";

import { api, isTauri, type ClaudeAccount, type UsageReport } from "../lib/api";
import { compactTokens } from "../lib/context-window";

/**
 * The Claude credential, and how to get one.
 *
 * The sign-in is the real `claude setup-token` flow driven in a pty — rmux shows
 * the link it prints, you authorise in your own browser, and paste the code
 * back. rmux does not reimplement the OAuth exchange: the client id, endpoints
 * and scope are Anthropic's to change, and a reimplementation would break
 * silently on the release that changed them.
 *
 * Pasting a credential directly is offered alongside, because someone who
 * already ran `setup-token` elsewhere, or who has a Console API key, should not
 * have to run a browser flow to use it here.
 *
 * **The token never comes back down to this component** — only the last four
 * characters, enough to tell two accounts apart.
 */

type Flow =
  | { step: "idle" }
  | { step: "starting" }
  | { step: "code"; authUrl: string }
  | { step: "paste" };

export function ClaudeAccountPanel() {
  const [account, setAccount] = useState<ClaudeAccount | null>(null);
  const [flow, setFlow] = useState<Flow>({ step: "idle" });
  const [code, setCode] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

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

  const startLogin = async () => {
    setBusy(true);
    setError(null);
    setNote(null);
    setFlow({ step: "starting" });
    try {
      const started = await api.claudeLoginStart();
      setFlow({ step: "code", authUrl: started.authUrl });
      // Opened for convenience; the link stays on screen because a browser that
      // silently fails to open would otherwise leave nothing to act on.
      await api.openExternal(started.authUrl).catch(() => {});
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setFlow({ step: "idle" });
    } finally {
      setBusy(false);
    }
  };

  const submitCode = async () => {
    setBusy(true);
    setError(null);
    try {
      setAccount(await api.claudeLoginSubmit(code));
      setCode("");
      setFlow({ step: "idle" });
      setNote("Signed in.");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const savePasted = async () => {
    setBusy(true);
    setError(null);
    try {
      setAccount(await api.claudeAccountSave(token));
      setToken("");
      setFlow({ step: "idle" });
      setNote("Saved.");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const cancel = () => {
    void api.claudeLoginCancel().catch(() => {});
    setFlow({ step: "idle" });
    setCode("");
    setError(null);
  };

  return (
    <section className="flex max-w-[520px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">CLAUDE ACCOUNT</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          One sign-in, applied to every host you code on. Without it, sessions use whatever
          account each host happens to be signed in to.
        </p>
      </header>

      <Row label="STATUS">
        {account === null
          ? "—"
          : account.connected
            ? `${account.hint ?? "signed in"}${account.kind ? ` · ${kindLabel(account.kind)}` : ""}`
            : "using each host's own login"}
      </Row>

      {flow.step === "code" && (
        <div className="flex flex-col gap-2 p-3" style={{ border: "1px solid var(--border-strong)" }}>
          <span className="micro">1 — AUTHORISE IN YOUR BROWSER</span>
          <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            If the browser did not open, copy this link:
          </p>
          <code
            className="data inset px-2 py-1 text-[10px] break-all"
            style={{ color: "var(--text)" }}
          >
            {flow.authUrl}
          </code>
          <div className="flex gap-2">
            <button
              type="button"
              className="chip"
              onClick={() => void navigator.clipboard.writeText(flow.authUrl)}
            >
              copy link
            </button>
            <button type="button" className="chip" onClick={() => void api.openExternal(flow.authUrl)}>
              open again
            </button>
          </div>

          <span className="micro mt-2">2 — PASTE THE CODE IT GIVES YOU</span>
          <input
            autoFocus
            value={code}
            spellCheck={false}
            placeholder="paste the code here"
            className="data inset px-2 py-[5px] text-[11px] outline-none"
            style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
            onChange={(e) => setCode(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && code.trim() && void submitCode()}
          />
          <div className="flex gap-2">
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || !code.trim()}
              onClick={() => void submitCode()}
            >
              {busy ? "Finishing…" : "Finish sign-in"}
            </button>
            <button type="button" className="btn" onClick={cancel}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {flow.step === "paste" && (
        <div className="flex flex-col gap-2 p-3" style={{ border: "1px solid var(--border-strong)" }}>
          <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            A Console API key, or the output of <span style={{ color: "var(--text)" }}>claude
            setup-token</span> run elsewhere.
          </p>
          <input
            autoFocus
            type="password"
            value={token}
            spellCheck={false}
            placeholder="sk-ant-api… / oat… / admin…"
            aria-label="Claude credential"
            className="data inset px-2 py-[5px] text-[11px] outline-none"
            style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
            onChange={(e) => setToken(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && token.trim() && void savePasted()}
          />
          <div className="flex gap-2">
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || !token.trim()}
              onClick={() => void savePasted()}
            >
              {busy ? "Saving…" : "Save"}
            </button>
            <button type="button" className="btn" onClick={() => setFlow({ step: "idle" })}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {(flow.step === "idle" || flow.step === "starting") && (
        <div className="flex flex-wrap gap-2">
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy}
            onClick={() => void startLogin()}
          >
            {flow.step === "starting" ? "Starting…" : account?.connected ? "Sign in again" : "Sign in to Claude"}
          </button>
          <button type="button" className="btn" disabled={busy} onClick={() => setFlow({ step: "paste" })}>
            Paste a credential
          </button>
          {account?.connected && (
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() =>
                void api
                  .claudeAccountForget()
                  .then(setAccount)
                  .then(() => setNote("Forgotten."))
                  .catch((e) => setError(String(e)))
              }
            >
              Forget
            </button>
          )}
        </div>
      )}

      <OrgUsage available={account?.usageAvailable ?? false} />

      {note && (
        <span className="micro" style={{ color: "var(--text)" }}>
          {note}
        </span>
      )}
      {error && (
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}
    </section>
  );
}

function kindLabel(kind: string) {
  return kind === "apiKey" ? "console key" : kind === "oauthToken" ? "subscription" : "admin key";
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <span className="micro shrink-0">{label}</span>
      <span className="data truncate text-[11px]" style={{ color: "var(--text)" }}>
        {children}
      </span>
    </div>
  );
}

/**
 * Organisation usage, read on demand and never polled — this is a billing API,
 * and the figure moves on the scale of hours.
 */
function OrgUsage({ available }: { available: boolean }) {
  const [report, setReport] = useState<UsageReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!available) {
    return (
      <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        Add an admin key (sk-ant-admin…) to read real organisation usage. A subscription token
        cannot — it can only make model requests.
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-1 pt-2" style={{ borderTop: "1px solid var(--border)" }}>
      <div className="flex items-baseline justify-between">
        <span className="micro">ORG USAGE {report ? `· ${report.days}D` : ""}</span>
        <button
          type="button"
          className="chip"
          disabled={busy}
          onClick={() => {
            setBusy(true);
            setError(null);
            api
              .claudeUsageReport(7)
              .then(setReport)
              .catch((e) => setError(e instanceof Error ? e.message : String(e)))
              .finally(() => setBusy(false));
          }}
        >
          {busy ? "reading…" : report ? "refresh" : "read"}
        </button>
      </div>
      {report && (
        <>
          <Row label="OUT">{compactTokens(report.output)}</Row>
          <Row label="IN">{compactTokens(report.uncachedInput)}</Row>
          <Row label="CACHED">{compactTokens(report.cacheRead)}</Row>
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
