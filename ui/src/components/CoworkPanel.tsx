import { useState } from "react";

import { api, type SignedIn } from "../lib/api";
import { SignIn, SERVER_KEY } from "./SignIn";

/**
 * The Cowork session.
 *
 * Framed as optional throughout, because it is: terminals, files and Claude are
 * a direct SSH connection that never touches this server. An account adds the
 * shared parts — the server registry, messaging, the leaderboard — and nothing
 * here gates the workbench.
 */
export function CoworkPanel({
  session,
  onSession,
}: {
  session: SignedIn | null;
  onSession: (session: SignedIn | null) => void;
}) {
  const [signingIn, setSigningIn] = useState(false);
  const [busy, setBusy] = useState(false);

  const serverUrl = session?.serverUrl ?? localStorage.getItem(SERVER_KEY) ?? "";

  return (
    <section className="flex max-w-[520px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">COWORK ACCOUNT</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Optional. rmux is a working IDE with no account at all — this is for the parts that
          are genuinely shared: the server registry, messaging and the leaderboard.
        </p>
      </header>

      <Row label="SIGNED IN AS">
        {session ? session.account.displayName || session.account.username : "not signed in"}
      </Row>
      {serverUrl && <Row label="SERVER">{serverUrl}</Row>}
      {session?.account.division && <Row label="DIVISION">{session.account.division}</Row>}

      <div className="flex gap-2">
        {session ? (
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              // Signing out also forgets this machine's face pairing — otherwise
              // a "signed-out" machine could still mint a session from a face.
              void api
                .signOut(serverUrl || undefined)
                .finally(() => {
                  onSession(null);
                  setBusy(false);
                });
            }}
          >
            {busy ? "Signing out…" : "Sign out"}
          </button>
        ) : (
          <button type="button" className="btn btn-primary" onClick={() => setSigningIn(true)}>
            Sign in
          </button>
        )}
      </div>

      {signingIn && (
        <SignIn
          onClose={() => setSigningIn(false)}
          onSignedIn={(next) => {
            onSession(next);
            setSigningIn(false);
          }}
        />
      )}
    </section>
  );
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
