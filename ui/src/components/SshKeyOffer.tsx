import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { invoke } from "@tauri-apps/api/core";

import {
  OFFER_EVENT,
  declineForever,
  targetOfOfferHost,
  type KeyOffer,
} from "../lib/ssh-key-offer";

/**
 * The offer to stop typing a password on this host.
 *
 * ## It is a banner, not a dialog
 *
 * It appears immediately after an authentication succeeded, which is exactly
 * when the operator has got what they wanted and is about to start working. A
 * modal there would take the keystrokes they are already typing into a terminal
 * — the same rule that keeps the Jira done-prompt inside its widget. This sits
 * at the bottom, takes no focus, and waits.
 *
 * ## Three answers, and "never" is one of them
 *
 * `NOT NOW` is per-run; `NEVER` is written down. Declining has to be as durable
 * as accepting or the offer becomes the thing people learn to dismiss without
 * reading — and a prompt that reappears after being refused is worse than no
 * feature, because it trains the reflex that dismisses real warnings too.
 *
 * ## What it says before it acts
 *
 * The host is printed, because the offer *writes to that machine's*
 * `authorized_keys` — and the name was parsed out of OpenSSH's own prompt text,
 * so it is exactly the thing to show rather than assert. The consequence is
 * stated too: only the public half travels.
 */
export function SshKeyOffer() {
  const [offer, setOffer] = useState<KeyOffer | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  useEffect(() => {
    const onOffer = (e: Event) => {
      setOffer((e as CustomEvent<KeyOffer>).detail);
      setResult(null);
      setFailed(null);
    };
    window.addEventListener(OFFER_EVENT, onOffer);
    return () => window.removeEventListener(OFFER_EVENT, onOffer);
  }, []);

  if (!offer) return null;

  const install = async () => {
    setBusy(true);
    setFailed(null);
    try {
      // **The shape Rust actually declares.** This sent
      // `{ kind: "ssh", host: { alias } }` — a nested map — and every attempt
      // died on `invalid type: map, expected a string` before touching the
      // host. `TargetRef` is `{ host, user?, port? }`, the same shape every
      // other command here takes.
      const message = await invoke<string>("ssh_key_install", {
        target: targetOfOfferHost(offer.host),
      });
      setResult(message);
      // Left on screen briefly rather than vanishing: this wrote to a file on
      // someone else's machine, and "it disappeared" is not confirmation.
      window.setTimeout(() => setOffer(null), 2500);
    } catch (e) {
      // Errors persist until the next attempt. A failed write here means the
      // operator will keep typing the password and would otherwise not know why.
      setFailed(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <AnimatePresence>
      <motion.div
        initial={{ opacity: 0, y: 8 }}
        animate={{ opacity: 1, y: 0 }}
        exit={{ opacity: 0, y: 8 }}
        transition={{ duration: 0.14 }}
        className="window fixed bottom-6 left-1/2 z-[80] flex max-w-[560px] -translate-x-1/2 flex-col gap-2 px-4 py-3"
        role="status"
      >
        <span className="micro" style={{ color: "var(--text-soft)" }}>
          {offer.host} ASKED FOR A PASSWORD
        </span>

        {result ? (
          <span className="data text-[12px]" style={{ color: "var(--text)" }}>
            {result} — it will not ask again.
          </span>
        ) : (
          <>
            <p className="data text-[12px] leading-relaxed" style={{ color: "var(--text)" }}>
              rmux opens several connections per session, so this host will keep asking. Add an SSH
              key and it stops.
            </p>
            <p className="micro leading-relaxed" style={{ color: "var(--text-faint)" }}>
              A key is generated on this Mac and only its public half is sent, appended to
              ~/.ssh/authorized_keys. Nothing already in that file is touched.
            </p>
            {failed && (
              <span className="micro leading-relaxed" style={{ color: "rgb(var(--primary))" }}>
                {failed}
              </span>
            )}
            <div className="flex flex-wrap gap-2">
              <button type="button" className="chip" disabled={busy} onClick={() => void install()}>
                {busy ? "ADDING…" : "ADD A KEY"}
              </button>
              <button type="button" className="chip" disabled={busy} onClick={() => setOffer(null)}>
                NOT NOW
              </button>
              <button
                type="button"
                className="chip"
                disabled={busy}
                onClick={() => {
                  declineForever(offer.host);
                  setOffer(null);
                }}
              >
                NEVER FOR THIS HOST
              </button>
            </div>
          </>
        )}
      </motion.div>
    </AnimatePresence>
  );
}
