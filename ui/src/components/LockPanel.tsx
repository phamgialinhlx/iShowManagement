import { useEffect, useState } from "react";

import { api, isTauri, type LockStatus, type SignedIn } from "../lib/api";
import { LockSettings } from "./LockSettings";

/**
 * The app lock, in Settings.
 *
 * The lock protects the **Cowork session**, so it needs one to protect — hence
 * the explicit "sign in first" state rather than a disabled button with no
 * explanation. It is worth being plain about what the lock does and does not
 * cover: it encrypts the stored session token, and it does not touch the SSH
 * keys the workbench uses without any account at all.
 */
export function LockPanel({ session }: { session: SignedIn | null }) {
  const [status, setStatus] = useState<LockStatus | null>(null);
  const [editing, setEditing] = useState(false);

  useEffect(() => {
    if (!isTauri() || !session) return;
    let cancelled = false;
    api
      .lockStatus(session.serverUrl)
      .then((s) => !cancelled && setStatus(s))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [session]);

  if (!session) {
    return (
      <section className="flex max-w-[520px] flex-col gap-3">
        <h2 className="kicker">APP LOCK</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          The lock encrypts your stored Cowork session, so there has to be one first. Sign in
          under COWORK, then come back.
        </p>
      </section>
    );
  }

  return (
    <section className="flex max-w-[520px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">APP LOCK</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Off by default. When on, rmux asks for a PIN before restoring your session — the PIN
          is the encryption key, so a wrong one yields nothing rather than being refused by a
          check that could be bypassed.
        </p>
      </header>

      <div className="flex items-baseline justify-between gap-3">
        <span className="micro">STATE</span>
        <span className="data text-[11px]" style={{ color: "var(--text)" }}>
          {status === null ? "—" : status.locked ? (status.face ? "on · PIN and face" : "on · PIN") : "off"}
        </span>
      </div>

      <div>
        <button type="button" className="btn btn-primary" onClick={() => setEditing(true)}>
          {status?.locked ? "Change the lock" : "Turn the lock on"}
        </button>
      </div>

      <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
        The lock covers the Cowork session only. Terminals, files and Claude are a direct SSH
        connection that works with no account, and this cannot protect the SSH keys they use.
      </p>

      {editing && status && (
        <LockSettings
          status={status}
          onChanged={setStatus}
          onClose={() => setEditing(false)}
        />
      )}
    </section>
  );
}
