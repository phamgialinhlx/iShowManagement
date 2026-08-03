import { useEffect, useRef, useState } from "react";
import { motion } from "motion/react";

import { api, type Account, type LockStatus } from "../lib/api";
import { cameraAvailable, describeFace, formatBytes, openCamera } from "../lib/face";

/**
 * Turning the lock on and off.
 *
 * Two things are being decided here and they are deliberately not the same
 * switch. **The PIN is the lock**: it encrypts the stored session, so it is
 * required to turn the lock on at all. **Face is a shortcut**, added on top, and
 * it can be declined without losing the lock.
 *
 * Face also has a real cost the operator should agree to before it is spent —
 * 6.7 MB of model weights that are not shipped in the app — so the download is
 * named and asked about rather than started quietly.
 */

type Step = "idle" | "pin" | "face" | "done";

export function LockSettings({
  status,
  onChanged,
  onClose,
}: {
  status: LockStatus;
  onChanged: (status: LockStatus) => void;
  onClose: () => void;
}) {
  const [step, setStep] = useState<Step>("idle");
  const [pin, setPin] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [account, setAccount] = useState<Account | null>(null);

  const videoRef = useRef<HTMLVideoElement>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .faceStatus()
      .then((a) => !cancelled && setAccount(a))
      .catch(() => {
        // Only affects whether face can be offered; the PIN half works
        // regardless, so this is not worth an error message.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const enable = async (withFace: boolean) => {
    setBusy(true);
    setError(null);
    setProgress(withFace ? "Preparing face unlock…" : "Locking…");
    try {
      onChanged(await api.lockEnable(pin, withFace));
      setStep("done");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  };

  /**
   * Enrol a face, if the account has none.
   *
   * An account that already has descriptors is *not* enrolled again: the server
   * appends samples rather than replacing them, so re-enrolling on every machine
   * would grow the set that each future login is matched against — which loosens
   * the match rather than tightening it.
   */
  const setUpFace = async () => {
    setBusy(true);
    setError(null);
    try {
      const models = await api.faceModelsStatus();
      if (!models.installed) {
        setProgress(`Downloading the face models (${formatBytes(models.bytes)})…`);
        await api.faceModelsInstall();
      }

      if (account?.hasFace) {
        setProgress("Trusting this machine…");
        await enable(true);
        return;
      }

      setProgress("Look at the camera…");
      const stream = await openCamera();
      try {
        const video = videoRef.current;
        if (!video) throw new Error("no camera preview");
        video.srcObject = stream;
        await video.play();
        await new Promise((r) => setTimeout(r, 1200));

        let descriptor: number[] | null = null;
        for (let i = 0; i < 12 && !descriptor; i += 1) {
          descriptor = await describeFace(video);
          if (!descriptor) await new Promise((r) => setTimeout(r, 400));
        }
        if (!descriptor) throw new Error("no face was found — try better lighting");

        setProgress("Enrolling…");
        await api.faceEnroll(descriptor);
      } finally {
        // Whatever happened, the camera goes off.
        stream.getTracks().forEach((t) => t.stop());
      }

      await enable(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
      setProgress(null);
    }
  };

  const disable = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.lockDisable(pin);
      onChanged({ locked: false, face: false, username: "", serverUrl: status.serverUrl });
      setStep("done");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setPin("");
    } finally {
      setBusy(false);
    }
  };

  const pinsMatch = pin.length >= 4 && pin === confirm;

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
        className="menu corner flex w-full max-w-[420px] flex-col gap-4 p-5"
      >
        <header className="flex items-baseline justify-between">
          <span className="kicker">LOCK RMUX</span>
          <button type="button" className="chip" onClick={onClose}>
            close
          </button>
        </header>

        {step === "done" && (
          <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            {/* `status` is already the *new* state by the time this renders —
                `onChanged` updated it. Reading it as the old one is what made
                this message say the opposite of what had just happened. */}
            {status.locked
              ? "Locked. rmux will ask for this every time it starts."
              : "The lock is off. rmux will open straight into the workbench."}
          </p>
        )}

        {step !== "done" && status.locked && (
          <>
            <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              rmux asks for your PIN on every start{status.face ? ", or your face" : ""}. Turning
              the lock off stores your session unencrypted again.
            </p>
            <label className="flex flex-col gap-1">
              <span className="micro">CONFIRM YOUR PIN</span>
              <input
                type="password"
                inputMode="numeric"
                maxLength={8}
                value={pin}
                className="data inset px-2 py-[5px] text-[13px] tracking-[0.3em] outline-none"
                style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
                onChange={(e) => setPin(e.target.value.replace(/\D/g, ""))}
              />
            </label>
            <button
              type="button"
              className="btn w-full"
              disabled={busy || pin.length < 4}
              onClick={() => void disable()}
            >
              {busy ? "Unlocking…" : "Turn the lock off"}
            </button>
          </>
        )}

        {step !== "done" && !status.locked && (
          <>
            <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              A PIN encrypts your stored session, so rmux cannot restore it without one. It does
              not protect your files or SSH keys — those work with no account at all.
            </p>

            <label className="flex flex-col gap-1">
              <span className="micro">NEW PIN — 4 TO 8 DIGITS</span>
              <input
                autoFocus
                type="password"
                inputMode="numeric"
                maxLength={8}
                value={pin}
                className="data inset px-2 py-[5px] text-[13px] tracking-[0.3em] outline-none"
                style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
                onChange={(e) => setPin(e.target.value.replace(/\D/g, ""))}
              />
            </label>
            <label className="flex flex-col gap-1">
              <span className="micro">AGAIN</span>
              <input
                type="password"
                inputMode="numeric"
                maxLength={8}
                value={confirm}
                className="data inset px-2 py-[5px] text-[13px] tracking-[0.3em] outline-none"
                style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
                onChange={(e) => setConfirm(e.target.value.replace(/\D/g, ""))}
              />
            </label>
            {/* Confirmed because there is no recovery: nobody can open the vault
                without this PIN, including us. A typo would cost the session. */}
            {confirm.length > 0 && !pinsMatch && (
              <span className="micro" style={{ color: "rgb(var(--primary))" }}>
                THOSE DO NOT MATCH
              </span>
            )}

            <button
              type="button"
              className="btn btn-primary w-full"
              disabled={busy || !pinsMatch}
              onClick={() => void enable(false)}
            >
              {busy && !progress ? "Locking…" : "Lock with a PIN"}
            </button>

            {cameraAvailable() && (
              <>
                <button
                  type="button"
                  className="btn w-full"
                  disabled={busy || !pinsMatch}
                  onClick={() => void setUpFace()}
                >
                  Also unlock with my face
                </button>
                <p
                  className="data text-[10px] leading-relaxed"
                  style={{ color: "var(--text-soft)" }}
                >
                  Downloads 6.7 MB of face models the first time. Your camera image stays on this
                  machine — only a numeric signature is sent. There is no liveness check, so a
                  photo of you would pass; your PIN always still works.
                </p>
              </>
            )}
          </>
        )}

        {/* Off-screen but *rendered*. `display: none` would be the obvious way to
            hide this and would break it — a video that is not being painted has
            no frames to read, so detection would simply never find a face. */}
        <video
          ref={videoRef}
          muted
          playsInline
          aria-hidden
          style={{
            position: "absolute",
            width: 1,
            height: 1,
            opacity: 0,
            pointerEvents: "none",
          }}
        />

        {progress && <span className="micro">{progress}</span>}

        {error && (
          <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}
      </motion.div>
    </motion.div>
  );
}
