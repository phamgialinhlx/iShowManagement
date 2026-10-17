# Redstone bridge — catching up from PR #240 to rmux v0.2.25

*For the Redstone team, after the server loss. Written 2026‑09‑22.*

Your integration work survives as **[PR #240](https://github.com/mosy-ai/redstone-agent/pull/240)** — nine commits, Aug 16–31, verified 28/28 end‑to‑end plus the Coding tab 6/6 in a real browser. That PR already implements essentially the whole contract rmux ships against. **You are not rebuilding from zero.** This document does two things:

1. Confirms **what PR #240 already gives you**, so you can trust it as the baseline.
2. Names the **exact deltas** rmux added *after* your PR (v0.2.22 → v0.2.25) — there are only **two** wire‑level changes, plus one thing to re‑verify.

The canonical, always‑current sources on the rmux side are:

- **`docs/redstone-bridge.md`** — the full spec (sign‑in, protocol, terminals, pi, security). Read it as the reference; this catch‑up only carries the *diff* on top of it.
- **`crates/rmux-bridge/src/protocol.rs`** — the wire types themselves, with a test per invariant. When prose and code disagree, the code wins.

> One caveat about the spec doc: `docs/redstone-bridge.md` was written at agent **0.2.21** and has **not** yet been updated with the two deltas below. So for `drivable` and `unsupportedByHost`, *this* document is the authority until the spec catches up. Everything else in the spec is current.

---

## 0. TL;DR — what to add

| # | Change | Kind | Effort |
|---|--------|------|--------|
| 1 | Read the new **`drivable`** boolean on each `Session`; gate the composer / send UI on it | New optional field (non‑breaking) | Small |
| 2 | Handle the new **`unsupportedByHost`** error code, distinct from `notFound` | New error code | Small |
| 3 | Re‑verify you send **`maxBytes`** camelCase on `readConversation`/`readTerminal` | No change if you follow the spec | Trivial |

Nothing you already built changes shape. Both deltas exist for the *same* real situation: **a busy host accumulates one rmux‑agent daemon per app upgrade**, each still serving its own live sessions, and a session owned by a daemon older than **agent 0.2.20** cannot accept the drive frames. Before these two additions, Redstone couldn't tell such a session apart from one that had ended, so it offered a composer that silently failed.

---

## 1. What PR #240 already delivers (your baseline — trust it)

Everything in this list is done and verified in your PR. Do **not** rebuild it; just make sure it is deployed.

**Transport & enrolment**
- `GET /api/v1/rmux/bridge` — the outbound WebSocket the host dials. Authenticated **before** the upgrade (unknown token → **HTTP 403 before upgrade**, which is what rmux measured through Cloudflare — not a 1008 close).
- `rmux_hosts` table — one row per machine, SHA‑256 of the token only (shown once), revocable per host.
- Registry keyed by host id, request/response correlation matched on **`id` before `event`**.
- Superseded connection left **open** (a rebuilt agent runs beside its predecessor until the old one's sessions end); deregistration checks identity so an old socket closing can't evict the live one.

**The verbs** — `listSessions`, `hostInfo`, `spawn`, `send`, `interrupt`, `close`, `listConversations`, `readConversation`, and the terminal set `readTerminal` / `sendTerminal` / `attachTerminal` / `terminalInput` / `resizeTerminal` / `detachTerminal`.

**Both coding agents** — the `agent` field (`"claude"` | `"pi"`) on `spawn` / `listConversations` / `readConversation`; `Kind`/`Agent` enums; the five agent tools mounted on both runtimes (deepagents + Claude SDK), only for users with a host enrolled.

**The hard‑won details you already fixed**
- Keystrokes don't wait for a response; ordering held by the send lock.
- Attach is refcounted per viewer.
- `goingAway` marks the connection **draining** immediately (refuse new calls in <1s rather than burning 30s timeouts).
- `orgName` in `/rmux/config` comes from the deployment default (the probe runs before sign‑in; never 500 it).
- Keepalive is **protocol‑level** ping, pinned on the command line (`--ws-ping-interval 20`), not an application‑level `{"method":"ping"}`.
- `refused` and `unsupported` are **409** (not 503 — 503 tells proxies to retry the unretryable); only `failed` is retryable.
- Reconnect backoff 1s / 2s / 4s / 8s; the agent redials constantly and that is normal.
- Reserved‑name guard on the send‑only path, matching the call path.
- SSE host set is live (a host enrolled after a tab opened emits without reload).

**The Coding tab** — sidebar entry once a machine is connected, live terminal via xterm.js (font read from the *computed* value, not `var(--font-mono)`; bearer via a raw reader because `EventSource` can't set headers; transparent xterm background so the glass panel shows through).

If any of the above is missing after you re‑deploy, it's a **redeploy gap, not new work** — recover it from the PR.

---

## 2. Delta 1 — the `drivable` flag on `Session` *(rmux v0.2.25)*

### On the wire

`listSessions` (and the `sessionOpened` event) now carry one extra boolean per session:

```jsonc
← {"id":1,"result":"sessions","sessions":[
     {"name":"claude-a1b2","kind":"claude","folder":"/home/dev.user/api",
      "conversationId":"2f9c…","status":"busy","pid":4021,"ageSeconds":900,
      "attached":false,"drivable":true},          // ← NEW
     {"name":"claude-77e1","kind":"claude","status":"idle","ageSeconds":90000,
      "attached":false,"drivable":false}          // ← alive, but owned by a pre-0.2.20 daemon
   ]}
```

- **Type:** `boolean`. **Default when absent: `true`.** An older bridge that never sends the field must be treated as drivable — reading a missing field as `false` would hide the composer on *every* session. (Pinned by `drivable_defaults_true_for_an_older_bridge_that_omits_it` in `protocol.rs`.)
- **`drivable: false`** means: the session is **alive and listed**, but the daemon that owns it is a build older than agent **0.2.20** and cannot accept the drive frames. rmux itself can still reach it (by reattaching locally); Redstone cannot write to it.

### What to build

- **Gate the composer / send button on `drivable`.** For a session where `drivable === false`, don't show an input; show a short note instead, e.g. *"Running under an older agent on this host — read‑only here. Interrupt/close and reading still work; open it from rmux to interact."*
- **Reading is unaffected.** `readConversation` and `listConversations` work regardless of `drivable` — a non‑drivable session's transcript is still fully readable.
- Because the flag is a *non‑breaking optional field* (spec §3.6: unknown fields are ignored both ways), a Redstone that ignores it won't crash — it will just show a composer that fails on send. That failure is now the typed error in Delta 2.

### Why this is now rare (context, no action needed)

rmux v0.2.25 also taught the **newest** agent to *adopt and drive sessions owned by older daemons* wherever the protocol allows (commit `bridge: drive old-daemon sessions too`). So in practice `drivable: false` is limited to sessions from daemons genuinely older than 0.2.20. Most sessions on an up‑to‑date host will be `drivable: true`.

---

## 3. Delta 2 — the `unsupportedByHost` error code *(rmux v0.2.25)*

### On the wire

A new value in the `code` enum on error responses:

```jsonc
← {"id":4,"result":"error","code":"unsupportedByHost",
   "message":"session is alive but its daemon (agent 0.2.18) predates the drive frames"}
```

Returned when a **drive** verb (`send`, `interrupt`, `sendTerminal`, `readTerminal`, `attachTerminal`, `terminalInput`) targets a session whose owning daemon is older than 0.2.20 — i.e. the same sessions that are marked `drivable: false`.

### What to build

Extend your error table (spec §3.4) with one row. **Branch on `code`, never message text.**

| `code` | meaning | what the UI should say |
|---|---|---|
| `notFound` | The session is **gone** | "That session has finished." Re‑list. |
| **`unsupportedByHost`** | The session is **alive**, but on a host running an old agent | "Can't drive this session — the host is running an older rmux agent. Reading still works; interact from rmux, or update the agent." **Do not say it finished.** |
| `refused` | Understood and declined (bad folder, unknown model profile) | Show the message. |
| `unsupported` | This *agent build* has no such **method** | Name the version that added it. |
| `failed` | Something on the host went wrong | Show the message. |

The critical distinction is **`unsupportedByHost` ≠ `notFound`**: one means "still running, just not drivable from here", the other means "gone". Telling a user their live session *finished* is the failure this code exists to prevent. (Pinned by `unsupported_by_host_is_its_own_code` in `protocol.rs`.)

> Don't confuse `unsupportedByHost` (this session's daemon is old) with the pre‑existing `unsupported` (this agent build lacks the whole method). Different causes, different messages.

---

## 4. Delta 3 — re‑verify `maxBytes` is camelCase *(rmux v0.2.22, no change if you follow the spec)*

rmux had a bug where `readConversation` / `readTerminal` expected `max_bytes` (snake_case) and silently ignored the spec's `maxBytes`, falling back to the default tail. **That was fixed on the rmux side** — the agent now reads `maxBytes` correctly. No change is required of you *if* you already send camelCase per spec §3.1:

```jsonc
→ {"id":3,"method":"readConversation","agent":"pi","conversation":"pisess-…","maxBytes":262144}
```

Just confirm your client sends `maxBytes`, not `max_bytes`. (Defaults: 256 KiB, capped 8 MiB. `truncated: true` means the front was cut off — render the notice.)

---

## 5. Re‑standing‑up checklist (since the server was lost)

If the deployment/DB is gone, this is the order to bring it back — all of it is already in PR #240:

- [ ] `rmux_hosts` table (host id, token SHA‑256, user/provider scoping, revocation).
- [ ] `GET /api/v1/rmux/bridge` — auth **before** upgrade; unknown token → **403 pre‑upgrade**.
- [ ] Process‑local registry keyed by host id; `id`‑before‑`event` correlation; superseded socket left open with identity‑checked deregistration.
- [ ] Keepalive **pinned** on the ASGI command line (`--ws-ping-interval 20`) — hosts silently vanish at ~60s otherwise, and this only worked before because uvicorn's *default* is 20s.
- [ ] Reconnect backoff 1/2/4/8; `goingAway` → mark draining and stand down.
- [ ] `/rmux/config` returns the deployment's `orgName`; never 500 (it's a pre‑sign‑in probe).
- [ ] Error codes: `refused`/`unsupported`/**`unsupportedByHost`** → 409; `failed` → retryable; `notFound` distinct.
- [ ] The five agent tools on both runtimes, mounted only for users with a host enrolled.
- [ ] Coding tab: composer gated on **`drivable`** (Delta 1); error handling for **`unsupportedByHost`** (Delta 2).

**Sign‑in reminder** (spec §2.3, confirmed against the live deployment in your own commit `197e825`, then corrected in `895cc86`): the SSO buttons are `disabled` / "Coming soon" — **password login works in a webview today**, so the device grant is what to have *before* Google SSO ships, not a blocker now. `rs_token` must travel in the **`Authorization: Bearer` header** — sent as a cookie it returns 401 and hangs silently.

---

## 6. Testing without rmux

You can exercise the whole bridge with the standalone Python stand‑in in **spec §5.2** (`pip install websockets`) — it dials your `/api/v1/rmux/bridge`, greets, and answers frames, so you can validate enrolment, correlation, the two new deltas, backoff and `goingAway` end‑to‑end before a real host is involved. To exercise Delta 1/Delta 2 specifically, have the stand‑in emit a session with `"drivable": false` and reply to any drive verb against it with `{"result":"error","code":"unsupportedByHost","message":"…"}`.

---

## 7. Summary

- **PR #240 is your baseline and it's ~complete.** Redeploy it; recover anything missing *from it*, not from scratch.
- **Two wire additions** since then: the `drivable` boolean on `Session`, and the `unsupportedByHost` error code. Both serve the "one daemon per upgrade, old ones can't be driven" reality.
- **One re‑verify:** send `maxBytes` (camelCase).
- Track the current contract at `docs/redstone-bridge.md` + `crates/rmux-bridge/src/protocol.rs` on the rmux side; ask us to update the spec doc with these two deltas so it stops being the one stale part.
