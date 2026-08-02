import { useCallback, useEffect, useState } from "react";

import { Workbench } from "./screens/Workbench";
import { SshPrompt } from "./components/SshPrompt";
import { LockScreen } from "./components/LockScreen";
import { SERVER_KEY } from "./components/SignIn";
import { api, isTauri, type LockStatus, type SignedIn, type Unlocked } from "./lib/api";
import { startControlBridge } from "./lib/control";
import { startNotifications } from "./lib/notify";

/**
 * **There is no login gate.**
 *
 * rmux is a working IDE with no account at all: terminals, files and Claude are a
 * direct SSH connection from this machine to the target and never touch the
 * Cowork server. Demanding an account before any of that works would be asking
 * people to authenticate to use something the server plays no part in.
 *
 * So the app opens straight into the workbench. A stored session is restored in
 * the background if there is one, and signing in is a button in the footer — for
 * the parts that genuinely are shared: the server registry, messaging, the
 * leaderboard.
 *
 * **The lock is the one exception, and only because it was asked for.** If a
 * sealed session is stored, the workbench waits behind [`LockScreen`] until a PIN
 * or a face opens it. That is a deliberate choice the operator made; it is never
 * the default, and it never appears for someone who has not turned it on.
 */
export function App() {
  const [session, setSession] = useState<SignedIn | null>(null);
  const [lock, setLock] = useState<LockStatus | null>(null);

  // rmux's backend face. Started here rather than lazily, because a client is
  // free to connect before the operator has touched anything — and it must find
  // the session list already mirrored rather than empty.
  useEffect(() => startControlBridge(), []);

  // Nothing acted on the status the rail already tracked, so a session that
  // finished while the operator was elsewhere sat idle until someone looked.
  useEffect(() => startNotifications(), []);

  // Restoring runs *beside* the app rather than in front of it. Nothing here
  // blocks the workbench, so a slow or unreachable server delays a footer label
  // and nothing else.
  useEffect(() => {
    if (!isTauri()) return;

    const serverUrl = localStorage.getItem(SERVER_KEY);
    if (!serverUrl) return;

    let cancelled = false;

    (async () => {
      // The lock is checked first and answers from the keychain alone, so a
      // locked app does not flash the workbench while a network call decides.
      try {
        const status = await api.lockStatus(serverUrl);
        if (cancelled) return;
        if (status.locked) {
          setLock(status);
          return;
        }
      } catch {
        // No lock state readable — fall through to the ordinary resume. Failing
        // open is right here: the lock protects a session that, without this
        // call, we cannot restore anyway.
      }

      try {
        const restored = await api.resumeSession(serverUrl);
        if (!cancelled && restored) setSession(restored);
      } catch {
        // Nothing stored, or the server is unreachable. Either way the app works;
        // the footer will simply say "not signed in".
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  const unlocked = useCallback((result: Unlocked) => {
    // A null account means the vault opened but the stored session is stale.
    // The app still unlocks; the footer simply reads "not signed in", and
    // signing in again is a button there rather than a wall here.
    setSession(
      result.account ? { account: result.account, serverUrl: result.serverUrl } : null,
    );
    setLock(null);
  }, []);

  // Forgetting the session is the way past a PIN nobody remembers. It discards
  // the sealed vault rather than bypassing it — the next start asks for a full
  // sign-in, and the workbench works meanwhile.
  const forget = useCallback(() => {
    // The URL is passed explicitly: nothing was restored, so Rust has no active
    // session to read it from and would otherwise clear nothing at all.
    const serverUrl = lock?.serverUrl ?? localStorage.getItem(SERVER_KEY) ?? undefined;
    void api.signOut(serverUrl).catch(() => {});
    setSession(null);
    setLock(null);
  }, [lock]);

  // Rendered *instead of* the workbench, never over it.
  //
  // An overlay would leave the whole workbench mounted and readable behind the
  // dialog — the transcript, open files, terminal scrollback, everything. A
  // lock you can read through is not a lock, and the fact that it looks like
  // one is what makes it worse than none.
  if (lock?.locked) {
    return <LockScreen status={lock} onUnlocked={unlocked} onSignOut={forget} />;
  }

  return (
    <>
      <Workbench session={session} onSession={setSession} />
      {/* Mounted above everything: ssh can ask for a credential at any moment,
          including while signed out and working locally. */}
      {isTauri() && <SshPrompt />}
    </>
  );
}
