/**
 * Which host an offer is about, and whether it should be made at all.
 *
 * Open http://localhost:5273/ssh-key-check.html and read the console.
 *
 * The offer *writes to a machine's* `authorized_keys`, so naming the wrong host
 * is not a cosmetic bug. OpenSSH gives the askpass helper a human string and no
 * structured field, so the destination is parsed out of text — and the rule is
 * that anything unrecognised yields no offer rather than a guess.
 */
import { declineForever, allowAgain, hostFromPrompt, passwordUsed, resetOffered , targetOfOfferHost } from "./src/lib/ssh-key-offer";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

const saved = localStorage.getItem("rmux.sshKey.declined");
localStorage.removeItem("rmux.sshKey.declined");
resetOffered();

// ── reading the host out of OpenSSH's own words ──────────────────────────────

check("the ordinary form", hostFromPrompt("anh@build-box's password:") === "anh@build-box");
check("without a user", hostFromPrompt("build-box's password:") === "build-box");
check("a dotted hostname", hostFromPrompt("me@a.b.example.com's password:") === "me@a.b.example.com");
check("the 'password for' phrasing", hostFromPrompt("Password for me@build-box:") === "me@build-box");

// A passphrase prompt is about a *local key file*, not a host — offering to
// install a key because someone unlocked one would be nonsense.
check("a key passphrase names no host", hostFromPrompt("Enter passphrase for key '/Users/a/.ssh/id_ed25519':") === null);
// Host-key confirmation is not a password at all.
check("a host-key question names no host", hostFromPrompt("Are you sure you want to continue connecting (yes/no)?") === null);
check("unrecognised text yields nothing", hostFromPrompt("something else entirely") === null);

// ── when the offer is raised ─────────────────────────────────────────────────

resetOffered();
check("a fresh host is offered", passwordUsed("a@host", "msg") === true);
// The prompt fires several times a minute on a password host; asking each time
// would be worse than the problem it solves.
check("the same host is not offered twice in a run", passwordUsed("a@host", "msg") === false);
check("a different host is still offered", passwordUsed("b@host", "msg") === true);

// Declining must be as durable as accepting, or the offer becomes the thing
// people learn to dismiss without reading.
resetOffered();
declineForever("c@host");
check("a declined host is never offered again", passwordUsed("c@host", "msg") === false);
resetOffered();
check("not even after a restart", passwordUsed("c@host", "msg") === false);
allowAgain("c@host");
check("but it can be re-enabled", passwordUsed("c@host", "msg") === true);

check("an empty host is never offered", passwordUsed("", "msg") === false);

// ── restore ──────────────────────────────────────────────────────────────────

if (saved === null) localStorage.removeItem("rmux.sshKey.declined");
else localStorage.setItem("rmux.sshKey.declined", saved);


// ── The target the install is actually addressed to ────────────────────────
//
// The offer names a host the way OpenSSH does, `user@host`; every rmux command
// takes `{ host, user, port }`. Sending the joined string as a nested map is
// what made this feature fail on every attempt with
// `invalid type: map, expected a string` — before it reached the host at all.
{
  const split = targetOfOfferHost("yitec@192.168.100.22");
  check(
    `user@host splits into the shape Rust declares — ${JSON.stringify(split)}`,
    split.host === "192.168.100.22" && split.user === "yitec",
  );

  const bare = targetOfOfferHost("build-box");
  check(
    `a bare alias carries no user — ${JSON.stringify(bare)}`,
    bare.host === "build-box" && bare.user === undefined,
  );

  // An ssh_config alias goes to the ssh binary verbatim, so it must survive.
  const alias = targetOfOfferHost("contabo2");
  check(`an ssh alias is untouched — ${JSON.stringify(alias)}`, alias.host === "contabo2");

  // Nothing sensible to split on: keep it whole rather than inventing an empty
  // user, which would resolve to a different host entirely.
  const odd = targetOfOfferHost("@weird");
  check(`a leading @ is not treated as a user — ${JSON.stringify(odd)}`, odd.host === "@weird");
}

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
  failures ? "background:#e63b2e;color:#fff" : "background:#2b7;color:#000",
);
