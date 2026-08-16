# Driving rmux Claude sessions from Redstone

**Audience:** the Redstone backend/frontend developer building this. You do not
need the rmux source to build against this document — every message, field and
endpoint is specified below, with examples.

**Status:** the rmux half is **built**. The agent ships a `bridge` subcommand that
dials Redstone, and rmux can enrol a host and start it. Nothing on the Redstone
side exists yet; [§2](#2-what-redstone-must-build) is the complete list of what
does.

**Sign-in is no longer a gate.** rmux can enrol a host from a **token pasted**
out of Redstone's own UI, so the whole feature works today on a deployment that
has not built the device flow. Minting over HTTP is a convenience, not the
mechanism: a host only ever needs an endpoint and a token, and whether Redstone
handed those to rmux or to a person makes no difference downstream. The device
grant ([§2.3](#23-device-sign-in)) remains worth having, as an upgrade.

---

## 1. What this is

Redstone's agent can already spawn and drive **its own** chats
(`spawn_session`, `send_to_session`, `list_my_sessions`, `read_session` in
`backend/app/agents/tools/dashboard.py`). This adds a fifth surface it can drive:
**Claude Code sessions running on the user's actual servers.**

```
Redstone agent UI                      the user's dev box
┌──────────────────┐                   ┌─────────────────────────────┐
│ "check on the    │                   │  rmux-agent daemon          │
│  billing rewrite │   wss (outbound)  │   ├── claude-a1b2  (busy)   │
│  and tell it to  │ ◄──────────────── │   ├── claude-9f0c  (waiting)│
│  run the tests"  │                   │   └── term-3       (shell)  │
└──────────────────┘                   │  rmux-agent bridge ─────────┘
                                       └─────────────────────────────┘
```

What the agent gets, concretely:

- **See every Claude conversation on a host** — running or not, including ones
  started months ago from a laptop that is now closed. This is the one thing it
  cannot get any other way.
- **Read a conversation back** as structured messages.
- **Send a message** into a running session, and interrupt one.
- **Spawn a new session** in a folder, with an opening prompt.

### 1.1 The bridge runs on the server, not in the desktop app

This was nearly built the other way — rmux holds the WebSocket and forwards over
the ssh connections it already has, which is less code and puts no credential on
any server. It is wrong for one decisive reason: **rmux is frequently closed.**
The entire point of `rmux-agent` is that work continues without it. A bridge in
the desktop app would let Redstone drive a server exactly while the user is
sitting in front of the machine that could have driven it by hand, and go blind
the moment they shut the lid.

Two consequences fall out, both good:

- A transcript read is a **local file read** on the host. Real transcripts have
  been measured at **228 MB**; pulling that over ssh per request is not viable.
- The connection is **outbound**, so a host behind NAT, on hotel wifi, or in a
  VPC with no ingress needs no firewall change, no public address and no
  certificate.

The cost is a credential on each enrolled host, which is why it is a **per-host
token** — see [§4](#4-security).

### 1.2 It reads transcripts, never the screen

Conversations come from Claude's own `.jsonl` files, which are structured and
authoritative. Redstone is never shown the terminal buffer. rmux's standing rule
is that it may *observe* Claude's TUI and must not *reimplement* it; a remote
client scraping rendered pixels would be the largest possible version of that
mistake.

---

## 2. What Redstone must build

Four things. §2.1 and §2.2 are the feature and are **done on the dev
deployment**; §2.3 is now optional; §2.4 is what makes it reachable by the
agent, and is the only thing still between this and a working demo.

### 2.1 The bridge endpoint

```
GET /api/v1/rmux/bridge          (WebSocket upgrade)
Authorization: Bearer <host token>
```

Accept the upgrade, authenticate the **host token** (not a user token — see
§2.2), and hold the socket open. The bridge sends its greeting immediately,
unprompted:

```json
{"protocol":1,"agentVersion":"0.2.20","host":{"hostname":"build-box","user":"dev.user","os":"linux","home":"/home/dev.user"}}
```

Then it waits. Redstone sends requests; the bridge answers. Full protocol in
[§3](#3-the-protocol).

Requirements:

- **Reject a token you do not recognise at the HTTP layer**, before the upgrade.
  A socket that accepts anyone is a socket anyone can drive a dev box through.

  Measured against the dev deployment: an unknown token gets **HTTP 403 before
  the upgrade**, not the 1008 close described to us. Both are handled and both
  back off — 403 fails the dial, 1008 is a policy close — but they are different
  code paths on the client, so it is worth knowing which one a deployment
  actually does. Observed backoff on 403: 2s, 4s, 8s, 16s, 32s, capping at 60s.
  Five attempts in the first forty seconds, and no hot loop.
- **Send WebSocket pings.** The bridge answers them and does not ping you. Idle
  connections through a load balancer are dropped at 60s by most defaults, and
  without pings every host silently disappears a minute after connecting.
- **Expect reconnections constantly.** Hosts reboot, deploys drop every socket at
  once. The bridge redials with backoff from 1s to 60s. Key on the host id, not
  the connection.
- **One host may connect more than once.** A rebuilt agent runs beside its
  predecessor until the old one's sessions end. Treat the newest connection for a
  host id as current; do not assume uniqueness.

### 2.2 Host registry and tokens

```
POST   /api/v1/rmux/hosts        → mint a token for one machine
DELETE /api/v1/rmux/hosts/{id}   → revoke it
GET    /api/v1/rmux/hosts        → list, for the UI
GET    /api/v1/rmux/config       → what this deployment supports
```

**`POST /api/v1/rmux/hosts`** — authenticated as the *user*, with their access
token. rmux calls this when the operator enrols a machine.

```jsonc
// request
{ "label": "build-box", "agentVersion": "0.2.20", "protocol": 1 }

// response
{
  "hostId": "h_7fc2…",
  "token":  "rbt_…",                                  // the per-host bearer token
  "endpoint": "wss://redstone.example/api/v1/rmux/bridge"
}
```

- **Return the `endpoint` rather than letting rmux derive it.** A deployment may
  terminate WebSockets on another host or path entirely, and guessing the scheme
  is how an integration breaks on the one installation that does something else.
- **The token belongs to the host, not the user.** Scope it to the verbs in §3
  against that one machine. See [§4](#4-security).
- Return the token **once**. rmux writes it to the host and never reads it back.

**`GET /api/v1/rmux/config`** is asked *before* rmux shows any Redstone control,
so an older deployment simply has none. A `404` is a fine answer and is handled.

```json
{"bridge":true,"deviceFlow":true,"orgName":"…","protocols":[1]}
```

### 2.3 Device sign-in

**No longer a blocker — a convenience.** rmux enrols from a pasted token today
(§5), so this buys the operator not having to carry one by hand. Worth doing,
not worth waiting for.

rmux is a public client: it holds no `client_secret`,
because a secret compiled into a desktop app is published to everyone who
downloads it. It also cannot host a redirect URI.

The standard answer is the **Device Authorization Grant, RFC 8628** — the "go to
this URL and type this code" flow. rmux already implements the client half
against this contract:

```
POST /api/v1/oauth2/device/authorize
  client_id=rmux&scope=openid profile email rmux.hosts

→ { "device_code": "...", "user_code": "WDJB-MJHT",
    "verification_uri": "https://redstone.example/device",
    "verification_uri_complete": "https://redstone.example/device?code=WDJB-MJHT",
    "expires_in": 900, "interval": 5 }
```

```
POST /api/v1/oauth2/token
  grant_type=urn:ietf:params:oauth:grant-type:device_code
  &device_code=...&client_id=rmux

→ 400 { "error": "authorization_pending" }        while they are still in the browser
→ 400 { "error": "slow_down" }                    to widen the poll interval
→ 200 { "access_token": "...", "refresh_token": "...", "id_token": "..." }
```

Plus a `/device` page in the web app where a signed-in user types the code and
approves. Redstone's existing `oauth_provider` already mints exactly these tokens
for the password grant — this is a new grant type over the same machinery, not a
new token system.

> **Any equivalent secret-free flow works.** If you would rather ship
> authorization-code + PKCE with a loopback redirect, or the poll-based shape
> Cowork uses for Jira (`POST /auth/jira/start` → open in the real browser → poll,
> draining on read), say so and rmux's client changes in about thirty lines. What
> cannot work is a flow requiring a `client_secret`.

### 2.4 The agent tools

Four tools, siblings of the ones in `backend/app/agents/tools/dashboard.py`. Each
is a thin wrapper over one bridge request.

| Tool | Bridge method | Notes |
|---|---|---|
| `rmux_list_sessions(host_id?)` | `listSessions` | What is running, and what each one is doing |
| `rmux_read_session(host_id, conversation_id, max_bytes?)` | `readConversation` | The conversation, as messages |
| `rmux_send_to_session(host_id, session, message)` | `send` | Types it and submits it |
| `rmux_spawn_session(host_id, folder, prompt, name?)` | `spawn` | Starts a new Claude |

Plus `rmux_list_conversations(host_id, folder?, limit?)` → `listConversations`
for history the rail never had.

Three things to get right in the tool descriptions, because the model reads only
those:

- **`session` and `conversation_id` are different keys.** `session` (`claude-a1b2`)
  is what you send *to*; `conversationId` (a UUID) is what you *read*. A live
  session carries both — `listSessions` returns them together. Say so, or the
  model will pass one where the other belongs.
- **Sending is fire-and-report**, exactly like `send_to_session`. It returns as
  soon as the keystrokes land. The reply appears in that session; poll
  `readConversation`, or watch for a `sessionStatus` event going `busy` → `idle`.
- **Status is worth acting on.** `waiting` means Claude has asked the user
  something and has stopped. That is the state most worth surfacing to a human,
  and the agent should not try to answer it blindly.

---

## 3. The protocol

One JSON object per WebSocket **text** message. Three kinds, demultiplexed on the
presence of `id`:

| | shape |
|---|---|
| request (Redstone → bridge) | `{"id": 7, "method": "...", ...}` |
| response (bridge → Redstone) | `{"id": 7, "result": "...", ...}` |
| event (bridge → Redstone) | `{"event": "...", ...}` — no `id` |

A request gets exactly one response carrying the same `id`. Events arrive
unprompted. **Match on `id` first**: an event that parsed as a response would
resolve a random pending request with unrelated data.

### 3.1 Requests

#### `listSessions`

```jsonc
→ {"id":1,"method":"listSessions"}
← {"id":1,"result":"sessions","sessions":[
     {"name":"claude-a1b2","kind":"claude","folder":"/home/dev.user/api",
      "title":"billing rewrite","conversationId":"2f9c8e10-…",
      "status":"busy","pid":4021,"ageSeconds":900,"attached":false},
     {"name":"term-3","kind":"shell","status":"unknown","ageSeconds":40,"attached":true}
   ]}
```

| field | meaning |
|---|---|
| `name` | The key `send`, `interrupt` and `close` take |
| `kind` | `claude` or `shell` |
| `folder` | Working directory, when known |
| `title` | The operator's own name for it, host-side, shared across their machines |
| `conversationId` | **Links a live session to its transcript.** Absent means Claude has not reported yet |
| `status` | `busy` · `waiting` · `idle` · `shell` · `unknown` |
| `ageSeconds` | Since creation. Age is what identifies an abandoned session |
| `attached` | Whether anyone is looking at it right now |

Unions **every** daemon on the host, not just the newest build's.

#### `listConversations`

Every Claude conversation on the host, running or not. Newest first.

```jsonc
→ {"id":2,"method":"listConversations","limit":50,"folder":"/home/dev.user/api"}
← {"id":2,"result":"conversations","conversations":[
     {"id":"2f9c8e10-…","folder":"/home/dev.user/api",
      "summary":"rewrite the billing reconciliation job","modified":1782903431000,
      "running":true}
   ]}
```

`folder` filters on a path boundary, so `/srv/api` does not also return
`/srv/api-staging`. **Always pass `limit`** — a host in use for a year holds
thousands.

#### `readConversation`

```jsonc
→ {"id":3,"method":"readConversation","conversation":"2f9c8e10-…","maxBytes":262144}
← {"id":3,"result":"conversation","truncated":true,"messages":[
     {"role":"user","text":"run the tests","at":"2026-08-16T09:58:11.000Z"},
     {"role":"tool","tool":"Bash","text":"…"},
     {"role":"assistant","text":"Three failures, all in …"}
   ]}
```

> **The field is `conversation`, not `id`.** The envelope is flattened into the
> same object, so a field called `id` here collides with the envelope's `id` and
> the frame does not parse at all. It was written that way first and a round-trip
> test caught it — worth knowing if you add a method: no request field may be
> called `id` or `method`.

- `maxBytes` defaults to 256 KiB and is capped at 8 MiB. **Transcripts reach
  hundreds of megabytes** — always read the tail.
- **`truncated: true` means the front was cut off.** Rendering that as a complete
  conversation shows one that appears to begin mid-sentence.
- `role` is `user` · `assistant` · `tool` · `system`. **`system` is not the user
  talking** — slash-command plumbing, caveat banners and system reminders are
  recorded by Claude as *user* messages and dominate the tail of a long session.
  They are demoted for you; hide them by default.
- `at` is **ISO-8601**, verbatim from Claude's record. It is the only timestamp in
  this protocol that is not a millisecond epoch, because it is a string in the
  file and converting it would mean an offset parser that can be quietly wrong.

#### `send`

```jsonc
→ {"id":4,"method":"send","session":"claude-a1b2","message":"run the tests"}
← {"id":4,"result":"ok"}
```

Types the message and submits it. Returns as soon as the keystrokes land, not
when Claude replies.

`{"result":"error","code":"notFound"}` is the **routine** outcome, not an
exceptional one: you are working from a list fetched some seconds ago and the
session may have ended. Say "that conversation has finished", not "failed".

#### `interrupt` · `close`

```jsonc
→ {"id":5,"method":"interrupt","session":"claude-a1b2"}   // Escape
→ {"id":6,"method":"close","session":"claude-a1b2"}       // ends it for good
```

#### `spawn`

```jsonc
→ {"id":7,"method":"spawn","folder":"~/api","prompt":"fix the failing test",
   "name":"test fix"}
← {"id":7,"result":"spawned","session":{"name":"claude-1a2b3c","kind":"claude",…}}
```

- The folder **must already exist**. Creating it would let a typo in a tool call
  produce directory trees on someone's server. `~` is expanded host-side.
- The prompt is passed as an argument to `claude`, shell-quoted — not typed in
  afterwards, which would mean guessing when the TUI is ready.
- There is **no field for `--dangerously-skip-permissions`**, deliberately. That
  is a judgement about one piece of work on one machine, made in person. A
  Redstone-spawned session always has permission checks on.
- `modelProfile` is accepted and **refused**: a profile decides where the user's
  credential is sent, and the bridge has no profile store. Start such a session
  from rmux.

#### `hostInfo`

```jsonc
→ {"id":8,"method":"hostInfo"}
← {"id":8,"result":"host","host":{"hostname":"build-box","user":"dev.user","os":"linux","home":"/home/dev.user"}}
```

`user` is **the ceiling on everything this connection can do** — the bridge has no
privilege beyond that account's. Worth showing in the UI.

### 3.2 Errors

```json
{"id":4,"result":"error","code":"notFound","message":"no session called claude-a1b2 on this host; it may have ended"}
```

| `code` | meaning | what to do |
|---|---|---|
| `notFound` | No such session or conversation | Re-list. Usually a stale list, not a fault |
| `refused` | Understood and declined | Show the message; it explains itself |
| `unsupported` | This agent build has no such method | Name the version that has it |
| `failed` | Something on the host went wrong | Show the message |

**Branch on `code`, never on `message` text.** The messages get reworded.

### 3.3 Events

```jsonc
{"event":"sessionStatus","session":"claude-a1b2","status":"waiting","at":1782903431000}
{"event":"sessionOpened","session":{…}}
{"event":"sessionClosed","session":"claude-a1b2"}
{"event":"goingAway","reason":"host shutting down"}
```

- `sessionStatus` is sourced from Claude's own status file — a fact it reports
  about itself, not one inferred from its pixels. **Only changes are sent**, so
  you never need to poll.
- `at` is a millisecond epoch **on the host's clock**. Use it to order one host's
  events, never to compute an age against Redstone's clock.
- `goingAway` means stand down rather than reconnect-loop.

### 3.4 Compatibility

- `protocol` in the greeting is this document's version, currently `1`.
- **An unknown method is answered with `unsupported`, not a dropped connection.**
  A host may run an agent from weeks ago because nothing has restarted its
  daemon — this is normal, not exceptional.
- Unknown *fields* are ignored in both directions. Adding an optional field is not
  a breaking change; adding a required one is.

---

## 4. Security

The token is a credential sitting on a development server. Everything below
follows from that.

**Mint one token per host, never the user's own session token.** A dev box is a
machine other people frequently have accounts on and which is rebuilt without
ceremony. A token that could act as the user across Redstone would make every
enrolled host a copy of their identity. A per-host token is revocable on its own,
and its blast radius is the closed verb set in §3 against one machine.

**There is no `exec`, and there must never be one.** This is the most important
sentence in the document. Every method in §3 is something the operator could have
done from rmux's own UI. Add a method that runs a command and the token stops
meaning "drive my Claude sessions" and starts meaning "shell on every machine I
have ever ssh'd into" — a different decision, which nobody made.

The pressure to add one will be real, because an agent that can run `ls` is more
capable than one that cannot. The answer is that **Claude already runs commands**,
inside a session, under its own permission prompts, where the operator can watch
and interrupt. Routing shell access through a session keeps that machinery in the
path. A bare `exec` removes it. `rmux-bridge`'s protocol carries a test that fails
if anyone adds a method whose name contains `exec`, `shell`, `command` or `run`.

**A `prompt` or `message` from Redstone is an instruction, by design** — that is
the feature. But it is also the reason enrolment is an explicit, per-host act by
the operator rather than a default, and the reason the verb set is closed. If
Redstone's agent can be prompt-injected by a web page it read, that injection
reaches exactly these verbs on exactly the hosts the user enrolled, and no
further.

On the rmux side, already implemented:

- The token travels in the `Authorization` header — never in a frame (it would
  land in every log that records a body), never in a URL (proxy logs), never in
  argv (`ps` shows one user's command line to every account on the host).
- `~/.rmux/redstone.json` is `0600` in a `0700` directory, and the mode is set
  **before** the token is written. A world-readable file is refused on read rather
  than used, on the basis that a credential that has been exposed should be
  treated as disclosed.
- Unenrolling **deletes** the file and kills the bridge. A revoked host still
  holding its token is one restart away from re-enrolling.
- A conversation id is validated before it is joined onto a path, so
  `../../.ssh/id_ed25519` cannot be read through `readConversation`.
- The bridge runs as the operator's own account and has no privilege beyond it.

---

## 5. What rmux ships

| | |
|---|---|
| `rmux-agent bridge` | The client. Dials out, reconnects with backoff, answers §3 |
| `crates/rmux-bridge` | The wire contract, with tests |
| Enrolment | Writes the token to a host over stdin and starts the bridge |
| `GET /api/v1/rmux/config` probe | So rmux controls stay **hidden** on a deployment without the bridge |
| Enrol from a pasted token | `redstone_enrol_with_token` — no sign-in needed |
| Device-flow client | Written against §2.3, reports "not supported" until it exists |

The agent grew from 1.4 MB to 2.8 MB, which is the TLS and WebSocket stack. It is
uploaded to a host once per build fingerprint.

**Not built:** any Redstone-side UI, and no rmux UI for enrolment — the command
exists, nothing renders it yet.

### 5.2 Verified against the dev deployment

Run on 2026-08-17 against `/api/v1/rmux/bridge` through the public tunnel, with
the shipped 0.2.20 agent binary:

| | |
|---|---|
| `wss://` dial, TLS, token accepted | connected first attempt |
| Keepalive | **one connection, zero reconnects over 2 minutes** — uvicorn's protocol-level pings are answered by the read path with no application traffic |
| Unknown token | HTTP 403 before upgrade; backoff 2→4→8→16→32s, capping at 60s |
| Config probe | `{"bridge":true,"deviceFlow":false,"orgName":"","protocols":[1]}` |

Still unproven end to end: `spawn`, and any request actually **sent by Redstone**
— every request so far has come from a stand-in server on one side or the other.

### 5.1 Trying it without Redstone

The protocol is plain JSON over a WebSocket, so a twenty-line server is enough to
drive a real host end to end:

```python
# pip install websockets
import asyncio, json, websockets

async def handler(ws):
    print("greeting:", json.loads(await ws.recv()))

    async def call(n, **msg):
        await ws.send(json.dumps({"id": n, **msg}))
        return json.loads(await ws.recv())

    print("sessions:", await call(1, method="listSessions"))
    print("history: ", await call(2, method="listConversations", limit=5))

async def main():
    async with websockets.serve(handler, "127.0.0.1", 8787):
        await asyncio.Future()          # serve forever

asyncio.run(main())
```

This toy skips the `Authorization` check entirely, which is why it is a toy. It
also assumes responses arrive in order, which is true only because it has one
request in flight at a time — a real client matches on `id`.

Then on the host, by hand:

```sh
mkdir -p ~/.rmux && chmod 700 ~/.rmux
umask 077 && cat > ~/.rmux/redstone.json <<'JSON'
{"endpoint":"ws://127.0.0.1:8787/bridge","token":"anything"}
JSON
~/.rmux/bin/rmux-agent-<version>-<fingerprint> bridge
```

`ws://` works and skips TLS, which is why this is a useful first step. Use
`wss://` for anything real.

---

## 6. Questions for the rmux side

Send these to the rmux repository rather than guessing:

- A method you need that is not in §3 — **except** anything that runs a command,
  which is answered in §4.
- Whether an event should carry more than it does. Events are cheap; a round trip
  from a Redstone worker to a dev box is not.
- Anything in §3 whose behaviour is not what this document says. The protocol has
  tests; the document is written from them, but they are the authority.
