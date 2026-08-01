import { useCallback, useEffect, useState } from "react";

import { Workbench } from "./screens/Workbench";
import { SshPrompt } from "./components/SshPrompt";
import { LockScreen } from "./components/LockScreen";
import { SERVER_KEY } from "./components/SignIn";
import { api, isTauri, type LockStatus, type SignedIn } from "./lib/api";

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

  const unlocked = useCallback((next: SignedIn) => {
    setSession(next);
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

  return (
    <>
      <Workbench session={session} onSession={setSession} />
      {/* Mounted above everything: ssh can ask for a credential at any moment,
          including while signed out and working locally. */}
      {isTauri() && <SshPrompt />}
      {lock?.locked && (
        <LockScreen status={lock} onUnlocked={unlocked} onSignOut={forget} />
      )}
    </>
  );
}
