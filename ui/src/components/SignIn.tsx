import { useEffect, useRef, useState } from "react";
import { motion } from "motion/react";

import { api, ApiError, type AuthConfig, type SignedIn } from "../lib/api";

/**
 * Signing in to a Cowork server.
 *
 * **Signing in is optional.** rmux is a working IDE with no account at all —
 * terminals, files and Claude are a direct SSH connection and never touch this
 * server. An account adds the shared parts: the server registry, messaging, the
 * leaderboard. So this is a modal reached from the footer, not a gate in front of
 * the app.
 *
 * The flow starts with a **server URL** because everything else follows from it:
 * which sign-in methods exist, which Jira, what the organisation is called, are
 * all that server's configuration. Asking "Jira or password?" before knowing
 * which server would be guessing.
 *
 * Jira sign-in opens the operator's **real browser** rather than an embedded
 * webview. An existing Jira session, a password manager and SSO all work there
 * and none of them work in a webview — and rmux never handles the password.
 */

const SERVER_KEY = "rmux.serverUrl";

/** The server's own OAuth outcome expires after ten minutes; stop before that. */
const POLL_TIMEOUT_MS = 9 * 60_000;
const POLL_INTERVAL_MS = 2000;

type Step =
  | { kind: "server" }
  | { kind: "method"; config: AuthConfig }
  | { kind: "waiting"; config: AuthConfig; url: string };

export function SignIn({
  onSignedIn,
  onClose,
}: {
  onSignedIn: (session: SignedIn) => void;
  onClose: () => void;
}) {
  const [serverUrl, setServerUrl] = useState(() => localStorage.getItem(SERVER_KEY) ?? "");
  const [step, setStep] = useState<Step>({ kind: "server" });
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const serverRef = useRef<HTMLInputElement>(null);
  useEffect(() => serverRef.current?.focus(), []);

  const normalised = () => serverUrl.trim().replace(/\/+$/, "");

  const connect = async () => {
    const url = normalised();
    if (!url) return;
    setBusy(true);
    setError(null);
    try {
      const config = await api.authConfig(url);
      // Remembered separately from the session, so signing out never makes you
      // retype where you work.
      localStorage.setItem(SERVER_KEY, url);
      setStep({ kind: "method", config });
    } catch (e) {
      setError(
        e instanceof ApiError || e instanceof Error
          ? `could not reach that server — ${e.message}`
          : String(e),
      );
    } finally {
      setBusy(false);
    }
  };

  const withPassword = async () => {
    setBusy(true);
    setError(null);
    try {
      onSignedIn(await api.signIn(normalised(), username, password));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const withJira = async (config: AuthConfig) => {
    setBusy(true);
    setError(null);
    const url = normalised();
    try {
      const start = await api.jiraStart(url);
      await api.openExternal(start.authUrl);
      setStep({ kind: "waiting", config, url: start.authUrl });

      // Poll until the browser half completes. The server *drains* the outcome
      // on read, so a success must be used immediately — never re-fetched.
      const deadline = Date.now() + POLL_TIMEOUT_MS;
      for (;;) {
        await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));
        if (Date.now() > deadline) {
          setError("the sign-in link expired — try again");
          setStep({ kind: "method", config });
          return;
        }
        const session = await api.jiraPoll(url, start.state);
        if (session) {
          onSignedIn(session);
          return;
        }
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setStep({ kind: "method", config });
    } finally {
      setBusy(false);
    }
  };

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.12 }}
      className="fixed inset-0 z-[95] grid place-items-start justify-center pt-[14vh]"
      style={{ background: "rgba(6,6,6,0.62)" }}
      onClick={onClose}
    >
      <motion.div
        initial={{ opacity: 0, y: -8 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ type: "spring", stiffness: 300, damping: 28 }}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.key === "Escape" && onClose()}
        className="menu corner flex w-full max-w-[420px] flex-col gap-4 p-5"
      >
        <header className="flex items-baseline justify-between">
          <span className="kicker">
            {step.kind === "server" ? "CONNECT" : (step.config.orgName ?? "SIGN IN")}
          </span>
          <button type="button" className="micro" onClick={onClose}>
            close
          </button>
        </header>

        {step.kind === "server" && (
          <>
            <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              rmux works without an account. Sign in to use your team's server registry,
              messaging and leaderboard.
            </p>
            <label className="flex flex-col gap-1">
              <span className="micro">SERVER URL</span>
              <input
                ref={serverRef}
                value={serverUrl}
                spellCheck={false}
                placeholder="https://cowork.example.com"
                className="data inset px-2 py-[5px] text-[12px] outline-none"
                style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
                onChange={(e) => setServerUrl(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && void connect()}
              />
            </label>
            <button
              type="button"
              className="btn btn-primary w-full"
              disabled={busy || !serverUrl.trim()}
              onClick={() => void connect()}
            >
              {busy ? "Connecting…" : "Continue"}
            </button>
          </>
        )}

        {step.kind === "method" && (
          <>
            <span className="micro truncate">{normalised()}</span>

            {step.config.jira && (
              <button
                type="button"
                className="btn btn-primary w-full"
                disabled={busy}
                onClick={() => void withJira(step.config)}
              >
                {busy ? "Opening browser…" : "Continue with Jira"}
              </button>
            )}

            {step.config.accounts && (
              <div className="flex flex-col gap-2">
                {step.config.jira && (
                  <span className="micro" style={{ textAlign: "center" }}>
                    or an account
                  </span>
                )}
                <input
                  value={username}
                  placeholder="username"
                  autoComplete="username"
                  className="data inset px-2 py-[5px] text-[12px] outline-none"
                  style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
                  onChange={(e) => setUsername(e.target.value)}
                />
                <input
                  type="password"
                  value={password}
                  placeholder="password"
                  autoComplete="current-password"
                  className="data inset px-2 py-[5px] text-[12px] outline-none"
                  style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
                  onChange={(e) => setPassword(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && void withPassword()}
                />
                <button
                  type="button"
                  className="btn w-full"
                  disabled={busy || !username || !password}
                  onClick={() => void withPassword()}
                >
                  Sign in
                </button>
              </div>
            )}

            {!step.config.jira && !step.config.accounts && (
              <p className="data text-[11px]" style={{ color: "var(--text-soft)" }}>
                That server offers no sign-in method rmux can use.
              </p>
            )}

            <button
              type="button"
              className="micro"
              onClick={() => setStep({ kind: "server" })}
            >
              use a different server
            </button>
          </>
        )}

        {step.kind === "waiting" && (
          <>
            <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              Finish signing in with Jira in your browser. This window will continue on its
              own.
            </p>
            <div
              className="w-full overflow-hidden"
              style={{ height: 2, background: "rgba(232,230,225,0.10)" }}
            >
              <div className="sweep" style={{ height: "100%", width: "38%" }} />
            </div>
            <button
              type="button"
              className="micro"
              onClick={() => void api.openExternal(step.url)}
            >
              reopen the browser link
            </button>
          </>
        )}

        {error && (
          <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}
      </motion.div>
    </motion.div>
  );
}

export { SERVER_KEY };
