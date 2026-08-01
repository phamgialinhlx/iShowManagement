import { useCallback, useEffect, useRef, useState } from "react";
import { motion } from "motion/react";

import { api, type LockStatus, type SignedIn } from "../lib/api";
import { describeFace, faceRetryable, openCamera } from "../lib/face";

/**
 * The lock, on reopen.
 *
 * This is the one screen in rmux that genuinely does gate the app, and it earns
 * that by being **opt-in**: it appears only when there is a sealed session to
 * open. With the lock off the workbench opens straight away, because it works
 * with no account at all.
 *
 * The PIN is the floor and the face is a shortcut over it. That ordering is not
 * a UI preference — it is what the two mechanisms actually are. The PIN
 * *decrypts* the stored session, so it works with no network and a wrong one
 * yields nothing. A face cannot derive a key, so it instead proves identity to
 * the server, which mints a fresh session; that needs the network, and there is
 * no liveness check anywhere in the chain, so a photograph would pass it.
 *
 * Hence: face is offered first when it is available because it is faster, and
 * "use my PIN" is always one click away and never hidden behind a failure.
 */

/** Long enough for the sensor to expose properly; a dark first frame finds no face. */
const WARMUP_MS = 1200;
/** Between frames that found no face at all. */
const SEARCH_MS = 450;
/** After a descriptor was computed and the server said no. Slower on purpose. */
const RETRY_MS = 1200;

type Phase = "idle" | "scanning" | "checking" | "pin";

export function LockScreen({
  status,
  onUnlocked,
  onSignOut,
}: {
  status: LockStatus;
  onUnlocked: (session: SignedIn) => void;
  onSignOut: () => void;
}) {
  const [phase, setPhase] = useState<Phase>(status.face ? "scanning" : "pin");
  const [pin, setPin] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hint, setHint] = useState("");

  const videoRef = useRef<HTMLVideoElement>(null);
  const pinRef = useRef<HTMLInputElement>(null);
  // Read by the capture loop, which outlives any single render.
  const runningRef = useRef(false);

  const toPin = useCallback(() => {
    runningRef.current = false;
    setPhase("pin");
    setHint("");
  }, []);

  useEffect(() => {
    if (phase === "pin") pinRef.current?.focus();
  }, [phase]);

  const unlockWithPin = async () => {
    setBusy(true);
    setError(null);
    try {
      onUnlocked(await api.lockUnlock(status.serverUrl, pin));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      // Clear it: leaving a rejected PIN in the box means the next Enter
      // resubmits the same wrong one, which reads as the app being stuck.
      setPin("");
      setBusy(false);
    }
  };

  // The camera loop. Runs only while the face phase is showing, and every exit
  // path stops the tracks — a camera light left on after unlocking would be a
  // fair thing to be alarmed by.
  useEffect(() => {
    if (phase !== "scanning") return;

    let stream: MediaStream | null = null;
    let timer: number | undefined;
    runningRef.current = true;

    const stop = () => {
      runningRef.current = false;
      window.clearTimeout(timer);
      stream?.getTracks().forEach((t) => t.stop());
      stream = null;
    };

    (async () => {
      try {
        setHint("STARTING THE CAMERA");
        stream = await openCamera();
        if (!runningRef.current) return stop();

        const video = videoRef.current;
        if (!video) return stop();
        video.srcObject = stream;
        await video.play();

        setHint("LOOK AT THE CAMERA");
        await new Promise((r) => {
          timer = window.setTimeout(r, WARMUP_MS);
        });

        let attempts = 0;
        while (runningRef.current) {
          const descriptor = await describeFace(video);
          if (!runningRef.current) break;

          if (!descriptor) {
            await new Promise((r) => {
              timer = window.setTimeout(r, SEARCH_MS);
            });
            continue;
          }

          setPhase("checking");
          try {
            const session = await api.lockUnlockFace(status.serverUrl, descriptor);
            stop();
            onUnlocked(session);
            return;
          } catch (e) {
            if (!runningRef.current) break;
            attempts += 1;
            const message = e instanceof Error ? e.message : String(e);

            // Some rejections can never start working by trying again — this
            // machine is not trusted, or nothing is enrolled. Looping on those
            // would spin forever against a camera.
            if (!faceRetryable(message)) {
              setError(message);
              stop();
              toPin();
              return;
            }

            setPhase("scanning");
            setHint(attempts >= 3 ? "STILL LOOKING — OR USE YOUR PIN" : "NOT RECOGNISED");
            await new Promise((r) => {
              timer = window.setTimeout(r, RETRY_MS);
            });
          }
        }
      } catch (e) {
        // No camera, or permission refused. Both are answered by the PIN, so
        // this is a fallback rather than a failure.
        setError(e instanceof Error ? e.message : String(e));
        stop();
        toPin();
      }
    })();

    return stop;
  }, [phase, status.serverUrl, onUnlocked, toPin]);

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: 0.18 }}
      className="fixed inset-0 z-[200] grid place-items-center"
      style={{ background: "var(--bg)" }}
    >
      <div className="menu corner flex w-full max-w-[360px] flex-col gap-4 p-6">
        <header className="flex flex-col gap-1">
          <span className="kicker">RMUX IS LOCKED</span>
          <span className="data text-[12px]" style={{ color: "var(--text)" }}>
            {status.username || "signed in"}
          </span>
        </header>

        {(phase === "scanning" || phase === "checking") && (
          <div className="flex flex-col gap-3">
            <div
              className="relative w-full overflow-hidden"
              style={{ aspectRatio: "1 / 1", background: "rgba(0,0,0,0.45)" }}
            >
              <video
                ref={videoRef}
                muted
                playsInline
                className="h-full w-full object-cover"
                // Mirrored, because an unmirrored self-view makes people move
                // the wrong way when they try to centre themselves.
                style={{ transform: "scaleX(-1)" }}
              />
              <div
                className="pointer-events-none absolute inset-0"
                style={{ border: "1px solid var(--border-strong)" }}
              />
            </div>
            <div
              className="w-full overflow-hidden"
              style={{ height: 2, background: "rgba(232,230,225,0.10)" }}
            >
              <div className="sweep" style={{ height: "100%", width: "38%" }} />
            </div>
            <span className="micro">{phase === "checking" ? "CHECKING" : hint}</span>
          </div>
        )}

        {phase === "pin" && (
          <label className="flex flex-col gap-1">
            <span className="micro">ENTER YOUR PIN</span>
            <input
              ref={pinRef}
              type="password"
              inputMode="numeric"
              autoComplete="off"
              value={pin}
              maxLength={8}
              disabled={busy}
              className="data inset px-2 py-[6px] text-[16px] tracking-[0.4em] outline-none"
              style={{ border: "1px solid var(--border-strong)", color: "var(--text)" }}
              onChange={(e) => {
                setPin(e.target.value.replace(/\D/g, ""));
                setError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && pin.length >= 4 && !busy) void unlockWithPin();
              }}
            />
            <button
              type="button"
              className="btn btn-primary mt-1 w-full"
              disabled={busy || pin.length < 4}
              onClick={() => void unlockWithPin()}
            >
              {busy ? "Unlocking…" : "Unlock"}
            </button>
          </label>
        )}

        <div className="flex items-center justify-between gap-2">
          {status.face && phase === "pin" ? (
            <button type="button" className="micro" onClick={() => setPhase("scanning")}>
              use my face
            </button>
          ) : status.face ? (
            <button type="button" className="micro" onClick={toPin}>
              use my PIN
            </button>
          ) : (
            <span />
          )}

          {/* The way out when the PIN is genuinely forgotten. It is a sign-out,
              not a bypass: the sealed session is discarded and the next start
              asks for the server and a full sign-in. */}
          <button
            type="button"
            className="micro"
            style={{ color: "var(--text-faint)" }}
            onClick={onSignOut}
          >
            forget this session
          </button>
        </div>

        {error && (
          <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}
      </div>
    </motion.div>
  );
}
