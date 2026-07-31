# Manual port forwarding: type remote + local, establish the tunnel

**Date:** 2026-07-31
**Goal:** Add a *manual* port-forward path — type a server (remote) port and a local port and establish an `ssh -L` tunnel — alongside the existing discovery-driven "forward" buttons, and polish the surrounding UI/UX.

---

## Why

Today forwarding is **discovery-driven only** (`core/src/forward.rs`, `web/src/lib/Managers.svelte`):
you can only forward a port the scan already found, via a per-row "forward" button, and you
never choose the local port — the backend auto-picks it (same as remote, else `20000 + remote%10000`).
You can't forward a port the scan didn't surface (not yet listening, filtered), and you can't pin the
local port to a value you want. This adds a manual entry path and cleans up the feedback UX.

## Glossary

- **Remote port** — the port on the server a service listens on.
- **Local port** — the port on the Mac where the tunnel surfaces (`127.0.0.1:<local>`).
- **Target host** — the host the tunnel points at *from the server's perspective*; defaults to the
  server's own `127.0.0.1`, optionally a third host only the server can reach.
- **Forward** — an `ssh -N -L 127.0.0.1:<local>:<target>:<remote>` process, keyed `(server, remote port)`.
- **Discovery list** — the per-server **Ports** tab that scans listening ports and offers per-row forward buttons.

## Decisions (from the grilling interview)

| # | Decision | Choice |
|---|---|---|
| 1 | **Form placement** | Top of each server's **Ports tab**; server implicit, no dropdown. |
| 2 | **Server selection** | Implicit from the tab (falls out of #1). |
| 3 | **Local defaulting** | Typing remote prefills local (mirror), then a debounced live check bumps to `20000+offset` if busy. Field editable; auto-fill backs off once user edits local. |
| 4 | **Local collision at submit** | **Hard-fail** with inline error ("local port X is in use"); form stays open. No silent bump (differs from the discovery path, where you never chose the local port). |
| 5 | **Duplicate remote** | **Block** with an error ("`:X` already forwarded — unforward first"). Keeps forwards keyed `(server, remote)`. |
| 6 | **Direction** | **Local forward (`-L`) only.** No `-R`. |
| 7 | **Server target** | **Optional target-host field**, pre-filled `127.0.0.1`, overridable to a third host. |
| 8 | **Persistence** | **None this iteration** — in-memory as today; no auto-reconnect at boot / after drop. |
| 9 | **Manage forwards** | Inline "active forwards for this server" list beside the form in the Ports tab **and** the global Home "Active tunnels" panel (unchanged). |
| 10 | **Submit feedback** | **Inline, no modals** — "Connecting…" + disabled button during the up-to-8s wait; success = row appears + form resets; failure = red inline error. |

## Shape of the work

**Backend (`core/`):**
- `ssh::forward_command` — add a `target_host` param; emit `-L 127.0.0.1:<local>:<target>:<remote>`
  (was hardcoded `127.0.0.1` target). `core/src/ssh.rs:183`.
- `forward.rs` — accept an explicit `local` port (and optional `target`) in the request body instead of
  auto-picking; on local busy → hard-fail (no `20000+offset` fallback for the manual path); on remote
  already in `forwards` map → `409`-style block. Keep the existing discovery-path behavior intact
  (auto-pick local) — likely a shared handler with optional `local`/`target`, or a sibling endpoint.
- New tiny endpoint exposing `net::is_port_open` for the client's debounced local-port check.

**Frontend (`web/`):**
- Ports tab (`Managers.svelte`): manual form (remote, local, optional target-host) + inline
  "active forwards for this server" list with kill buttons. Prefill/live-check logic for local.
  Inline "Connecting…" / error states; drop the `alertDialog` for the manual path.
- `api.ts`: `forwardPort` gains `local`/`target`; add the port-check call.

## Explicitly out of scope

Remote (`-R`) forwards; persistence / auto-reconnect across restarts; forwarding the *same* remote
port to multiple local ports at once.

## Open minor points (decide during build)

- **Privileged local ports (<1024)** bind requires root on macOS → `ssh -L` will fail; let the
  submit hard-fail surface the OS error (no special-casing).
- **Port-range validation** 1–65535, client-side, submit disabled until both valid; backend re-validates.
- **Target-host field visibility** — subtle/optional inline field vs. an "advanced" disclosure; lean
  to a small always-visible field pre-filled `127.0.0.1` for implementation simplicity.
