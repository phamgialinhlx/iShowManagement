# 3. Rename rmux to zmux (clean break)

Date: 2026-08-16
Status: Accepted

## Context

ADR-0001 and ADR-0002 rewrote the frontend from Tauri/webview to native gpui
on the `zmux` branch, and `zmux` was already the product name used in those
ADRs and in the shipped binary (`crates/zmux-app`'s `[[bin]] name = "zmux"`).
But the backend still carried the old identity everywhere else: every crate
was `rmux-*`, the session daemon binary was `rmux-agent`, on-disk state and
sockets lived under `~/.rmux/`, and dev env vars were `RMUX_*`. zmux was a
frontend wearing rmux's backend, not a project with its own identity.

Separately, the daemon's own name was worth re-examining. `rmux-agent` was
named in the very first commit with no recorded rationale, and the code's own
doc comments already describe it as a "daemon" (`lib.rs`: "a small daemon
that owns terminal sessions"; the v1 demolition manifest: "Persistent session
daemon"). In 2026, "agent" strongly connotes an autonomous AI agent, which
misdescribes this program: it holds PTYs and outlives connections, but it
takes no autonomous action and makes no LLM calls itself — the user drives
everything through the frontend, and the daemon happens to host Claude Code
sessions among others.

## Decision

Full identity break, not a coat of paint: every `rmux-*` crate, binary, path,
and env var becomes `zmux-*` / `ZMUX_*`. No migration and no backward-compat
adoption of old rmux daemons — a clean break, matching how the frontend
rewrite already treated v1.

The session daemon is renamed `rmux-agent` → **`zmuxd`**, not `zmux-agent`.
`zmuxd` follows Unix daemon convention (`sshd`, `dockerd`) and matches what
the code already calls itself. This is a compound collapse (two words → one),
not a mechanical hyphen swap, so it needed hand-verification: `socket_stem` in
`crates/zmuxd/src/ipc.rs` used to rely on an accidental non-collision between
the lib crate name (`rmux_agent`, underscore — Rust forces this) and the
install-binary prefix (`rmux-agent-`, hyphen) to tell an installed daemon
apart from cargo's own test binary. Collapsing both to the single word
`zmuxd` removed that accident, so the check now disambiguates on purpose: an
install name always has the shape `zmuxd-<version>-<fingerprint>` (two
hyphens after the prefix), which cargo's `zmuxd-<hash>` test binary (one
hyphen) never has.

All other crates follow the flat pattern: `rmux-{transport,ssh,fs,git,term,
claude,app,askpass}` → `zmux-*`.

## Consequences

- **Existing rmux sessions are invisible to zmux.** Old `rmux-agent` daemons
  on hosts keep running and holding their sessions — quitting rmux does not
  kill them — but zmux looks for `zmuxd` under `~/.zmux/bin/` and will not
  find or list them. Reconnecting under zmux starts fresh.
- **SSH keys are not reused.** zmux generates its own keys under
  `~/.ssh/zmux_<host>_ed25519`; the old `~/.ssh/rmux_<host>_ed25519` keys are
  orphaned, requiring re-authorization on the target host.
- **ControlMaster sockets are not reused.** `~/.rmux/mux/` → `~/.zmux/mux/`;
  existing multiplexed connections are not picked up.
- **`TERM_PROGRAM` changes from `rmux` to `zmux`.** Any shell config or
  script checking `TERM_PROGRAM=rmux` stops matching.
- **Dev env vars are renamed:** `RMUX_LIVE_HOST`/`RMUX_LIVE_REPO`/
  `RMUX_LIVE_WINDOWS` → `ZMUX_LIVE_*`; `RMUX_AGENT_DIR`/`RMUX_AGENT_HANDOFF` →
  `ZMUXD_DIR`/`ZMUXD_HANDOFF` (the daemon-specific ones followed the daemon's
  name, not the generic `ZMUX_` prefix).
- Bare English "agent" as an informal descriptor in prose and doc-comments
  (e.g. "provision agent", "an agent is already listening") was left alone —
  rewriting ~40+ prose instances to "daemon" is a stylistic pass with no
  effect on anything that builds or runs, out of scope for this rename.
