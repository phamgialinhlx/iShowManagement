/**
 * "You just typed a password. Would you like to stop doing that?"
 *
 * A password host asks on *every* connection, and rmux opens many per session —
 * a terminal, a Claude run, a metrics sample, a file read. Each one is a dialog.
 * So the moment an operator types a password is the moment the offer makes
 * obvious sense, and it is the only moment rmux can be sure the host needs one:
 * a key host never produces the prompt at all.
 *
 * ## Why the offer is remembered rather than repeated
 *
 * The prompt fires several times a minute on a password host. Asking every time
 * would be worse than the problem — so a host is offered **once per run**, and
 * a "never" is written down permanently. Declining must be as durable as
 * accepting, or the feature becomes the thing people learn to dismiss.
 *
 * ## The password itself never comes near this
 *
 * Only the host is recorded. `SshPrompt` clears the secret from component state
 * before its IPC round-trip even completes, and nothing here is given it.
 */

const NEVER_KEY = "rmux.sshKey.declined";

/** Hosts offered in this run, so the offer does not reappear on every prompt. */
const offered = new Set<string>();

export type KeyOffer = { host: string; label: string };

/** Fired when a host has just been authenticated with a password. */
export const OFFER_EVENT = "rmux:ssh-key-offer";

function declined(): string[] {
  try {
    const raw = localStorage.getItem(NEVER_KEY);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
  }
}

/** Stop asking about this host, permanently. */
export function declineForever(host: string): void {
  try {
    localStorage.setItem(NEVER_KEY, JSON.stringify([...new Set([...declined(), host])]));
  } catch {
    /* a full localStorage must not turn a declined offer into a repeating one */
  }
}

/** Ask again about a host previously declined — the escape hatch for "not now, ever". */
export function allowAgain(host: string): void {
  try {
    localStorage.setItem(NEVER_KEY, JSON.stringify(declined().filter((h) => h !== host)));
    offered.delete(host);
  } catch {
    /* ignore */
  }
}

/**
 * Note that `host` needed a password, and offer a key if it is worth offering.
 *
 * Returns whether an offer was raised, so the caller can tell "asked" from
 * "already handled" without reaching into the state itself.
 */
export function passwordUsed(host: string, label: string): boolean {
  if (!host || offered.has(host) || declined().includes(host)) return false;
  offered.add(host);
  window.dispatchEvent(new CustomEvent<KeyOffer>(OFFER_EVENT, { detail: { host, label } }));
  return true;
}

/** For tests and for "ask me again this run". */
export function resetOffered(): void {
  offered.clear();
}

/**
 * The host an OpenSSH prompt is about, or `null`.
 *
 * OpenSSH hands the askpass helper a human-readable string and nothing else —
 * there is no structured field to read — so the destination has to come out of
 * the text. It is worth doing rather than guessing from context: the offer
 * *acts* on this name, and installing a key on the wrong machine is a change to
 * the wrong server's `authorized_keys`.
 *
 * Returning `null` on anything unrecognised is the point. No offer is a small
 * loss; an offer naming a host the operator is not connecting to is a control
 * that does the wrong thing, which the interface rules forbid outright — and it
 * would be pressed, because the dialog that preceded it was real.
 */
/**
 * The offer's `user@host` string as the target rmux addresses elsewhere.
 *
 * The prompt names a host the way OpenSSH does — `yitec@192.168.100.22` — while
 * every command in the app takes `{ host, user, port }`. Passing the joined
 * string through as the *alias* would work, in that `ssh` accepts it, but it
 * would be a different `TargetId` from the one this server already has: a second
 * ControlMaster, a second authentication, and the password prompt this offer
 * exists to end. Splitting it keeps the key install on the connection that is
 * already open.
 */
export function targetOfOfferHost(host: string): { host: string; user?: string } {
  const at = host.indexOf("@");
  if (at <= 0) return { host };
  return { host: host.slice(at + 1), user: host.slice(0, at) };
}

export function hostFromPrompt(message: string): string | null {
  // `anh@build-box's password:` and `anh@build-box's password for ...:` —
  // the form OpenSSH uses for password and keyboard-interactive auth.
  const owned = /(?:^|\s)(?:([^\s@]+)@)?([^\s@']+)'s password/i.exec(message);
  if (owned?.[2]) return owned[1] ? `${owned[1]}@${owned[2]}` : owned[2];

  // `Password for user@host:` — some servers and PAM stacks phrase it this way.
  const forHost = /password for (?:([^\s@]+)@)?([^\s@:]+)/i.exec(message);
  if (forHost?.[2]) return forHost[1] ? `${forHost[1]}@${forHost[2]}` : forHost[2];

  return null;
}
