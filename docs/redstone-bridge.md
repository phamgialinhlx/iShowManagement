# Driving rmux's Claude and pi sessions from Redstone

**Audience:** the Redstone backend/frontend developer building this. You do not
need the rmux source to build against this document — every message, field and
endpoint is specified below, with examples.

**Status:** the **rmux side is built and unit-tested**, and validated against a
real host using a stand-in WebSocket server (see [§5.2](#52-trying-it-without-redstone)) —
the agent dials out, authenticates by header token, keeps the socket alive, and
answers every method in [§3](#3-the-protocol) for both Claude and pi.

**The Redstone side is not built yet.** Measured against the current deployment
(`cowork.chatredstone.com`, 2026-08-31): `GET /api/v1/rmux/config`, `/hosts` and
`/bridge` all return **404** — there is no endpoint for the agent to dial, and no
Redstone repo references `rmux`. So **all of [§2](#2-what-redstone-must-build)
remains to build**: the bridge WebSocket endpoint ([§2.1](#21-the-bridge-endpoint)),
the host registry and config ([§2.2](#22-host-registry-and-tokens)), and the agent
tools ([§2.4](#24-the-agent-tools)). [§2.3](#23-sign-in-your-web-login-not-a-token)
(sign-in) reuses the existing web login and needs only the four confirmations
listed there.

---

## 1. What this is

Redstone's agent can already spawn and drive its own chats (`spawn_session`,
`send_to_session`, `list_my_sessions`, `read_session` in
`backend/app/agents/tools/dashboard.py`). This adds a surface it can drive:
**the coding sessions and terminals running on the user's actual servers.**

```
Redstone agent UI                      the user's dev box
┌──────────────────┐                   ┌─────────────────────────────┐
│ "check the       │                   │  rmux-agent daemon          │
│  billing rewrite │   wss (outbound)  │   ├── claude-a1b2  (busy)   │
│  and run the     │ ◄──────────────── │   ├── pi-9f0c      (idle)   │
│  tests"          │                   │   └── term-3       (shell)  │
└──────────────────┘                   │  rmux-agent bridge ─────────┘
                                       └─────────────────────────────┘
```

Concretely, the agent gets to:

- **See every Claude and pi conversation on a host** — running or not, including
  ones started months ago from a laptop that is now closed.
- **Read a conversation back** as structured messages.
- **Send a message** into a running session, and interrupt one.
- **Spawn a new session** — Claude or pi — in a folder.
- **Read, drive, and stream a terminal live**, like a normal terminal in your UI.

### 1.1 The bridge runs on the server, not in the desktop app

This was nearly built the other way — rmux holds the WebSocket and forwards over
the ssh it already has. It is wrong for one decisive reason: **rmux is frequently
closed.** The whole point of `rmux-agent` is that work continues without it. A
bridge in the desktop app would let Redstone drive a server only while the user
is sitting in front of the machine that could have driven it by hand.

Two consequences, both good:

- A transcript read is a **local file read** on the host. Real transcripts have
  been measured at **228 MB**; pulling that over ssh per request is not viable.
- The connection is **outbound**, so a host behind NAT, on hotel wifi, or in a
  VPC with no ingress needs no firewall change, no public address, no certificate.

The cost is a credential on each enrolled host, which is why it is a **per-host
token** — see [§4](#4-security).

### 1.2 It reads transcripts, never the screen

Conversations come from each agent's own `.jsonl` files, which are structured and
authoritative. Redstone is never shown a scraped terminal buffer for a
conversation. (The *live terminal* view in §3.2 is a separate, explicit feature.)

---

## 2. What Redstone must build

§2.1 and §2.2 are the feature — the bridge WebSocket endpoint and the host
registry. §2.3 is sign-in — **nothing new to build, just confirm four things.**
§2.4 is what makes it reachable by the agent. **None of §2 is deployed yet** (the
current deployment 404s on all of it), so §2.1, §2.2 and §2.4 are all still to
build.

### 2.1 The bridge endpoint

```
GET /api/v1/rmux/bridge          (WebSocket upgrade)
Authorization: Bearer <host token>
```

Accept the upgrade, authenticate the **host token** (not a user token — see
§2.2), and hold the socket open. The bridge sends its greeting immediately,
unprompted:

```json
{"protocol":1,"agentVersion":"0.2.21","host":{"hostname":"build-box","user":"dev.user","os":"linux","home":"/home/dev.user"}}
```

Then it waits. Redstone sends requests; the bridge answers.

- **Reject an unknown token at the HTTP layer**, before the upgrade. Measured
  against the dev deployment: it answers an unknown token with **HTTP 403 before
  the upgrade**, not the 1008 close it documents. Both are handled on our side
  and both back off, but they are different client paths — so whichever one you
  did not mean to ship is the one that is untested.
- **Send WebSocket pings.** The bridge answers them and does not ping you. Idle
  connections through a load balancer are dropped at ~60s; without pings a host
  silently disappears about a minute after connecting. (Verified working: one
  connection, zero reconnects over two minutes through a public tunnel.)
- **Expect reconnections constantly.** Hosts reboot; deploys drop every socket at
  once. The bridge redials with backoff. Key on the host id, not the connection.
- **One host may connect more than once.** A rebuilt agent runs beside its
  predecessor until the old one's sessions end. Treat the newest connection for a
  host id as current.

### 2.2 Host registry and tokens

```
POST   /api/v1/rmux/hosts        → mint a token for one machine
DELETE /api/v1/rmux/hosts/{id}   → revoke it
GET    /api/v1/rmux/hosts        → list, for the UI
GET    /api/v1/rmux/config       → what this deployment supports
```

**`POST /api/v1/rmux/hosts`** — authenticated as the *user*, with their access
token. rmux calls it when the operator enrols a machine.

```jsonc
// request
{ "label": "build-box", "agentVersion": "0.2.21", "protocol": 1 }

// response
{
  "hostId": "h_7fc2…",
  "token":  "rbt_…",                                  // the per-host bearer token
  "endpoint": "wss://redstone.example/api/v1/rmux/bridge"
}
```

- **Return the `endpoint` explicitly.** A deployment may terminate WebSockets on
  another host or path, and guessing the scheme breaks the one install that does.
- **The token belongs to the host, not the user.** Scope it to the verbs in §3
  against that one machine. See §4.
- Return the token **once**. rmux writes it to the host and never reads it back.

**`GET /api/v1/rmux/config`** is asked *before* rmux shows any Redstone control,
so an older deployment simply has none. A `404` is a fine answer and is handled.

```json
{"bridge":true,"deviceFlow":false,"orgName":"…","protocols":[1]}
```

### 2.3 Sign-in: your web login, not a token

**Nothing new to build here — this is how it works today, and it needs only your
existing web login.** The device grant we first asked for is no longer required;
it stays a nice-to-have.

rmux is a public client — it holds no `client_secret` (a secret compiled into a
desktop app is published to everyone who downloads it) and cannot host a
redirect. So it does what your desktop spec
(`docs/desktop/redstone-desktop-spec.md` §4) already prescribes:

1. The operator types their Redstone address.
2. rmux opens **your login page** in a window — your form, your SSO, your 2FA. No
   password is ever typed into rmux.
3. rmux reads the **`rs_token`** cookie your web app sets on that origin.
4. It proves the token with `GET /api/v1/rmux/hosts` before storing it, then
   mints host tokens itself.

**Four things to confirm** — three are almost certainly already true:

| What | We need |
|---|---|
| **Cookie** | named `rs_token`, on the same origin as `/api/v1/*`. `HttpOnly` is fine (we read the native cookie store, not `document.cookie`). If it lives on a *different* origin from the API, tell us — we read cookies for the address the operator typed. |
| **The token as Bearer** | `rs_token` must be accepted as `Authorization: Bearer` on `/api/v1/rmux/*`. If that endpoint wants an OAuth2 access token instead, sign-in **hangs silently** — the cookie is there and the check never passes. That is the most confusing possible failure; confirm explicitly. |
| **Webview login** | your login page must render in an embedded webview (WKWebView / WebView2). ⚠️ **Google blocks OAuth in embedded webviews** (`disallowed_useragent`). If any deployment signs in with "Continue with Google", this path fails and the device grant becomes necessary after all. Tell us which SSO methods your deployments actually use. |
| **`endpoint`** | return the websocket `endpoint` explicitly from `POST /rmux/hosts` (§2.2). |

> The device grant (RFC 8628) is still welcome as a cleaner future flow — it
> avoids rendering a login in a webview at all, and it is the answer if the
> Google-webview case is real. rmux's client half is written and reports "not
> supported" until you ship it. **Any secret-free flow works** (device, or
> authorization-code + PKCE with a loopback redirect); what cannot work is one
> needing a `client_secret`.

### 2.4 The agent tools

Siblings of the ones in `backend/app/agents/tools/dashboard.py`, each a thin
wrapper over one bridge request:

| Tool | Bridge method |
|---|---|
| `rmux_list_sessions(host_id?)` | `listSessions` |
| `rmux_list_conversations(host_id, agent?, folder?, limit?)` | `listConversations` |
| `rmux_read_session(host_id, conversation_id, agent?)` | `readConversation` |
| `rmux_send_to_session(host_id, session, message)` | `send` |
| `rmux_spawn_session(host_id, folder, prompt, agent?, name?)` | `spawn` |

Plus the terminal verbs (§3.2) if you want the agent — or a human in Redstone's
UI — to drive a shell. Three things to get right in the tool descriptions,
because the model reads only those:

- **`session` and `conversationId` are different keys.** `session` (`claude-a1b2`)
  is what you send *to*; `conversationId` (a UUID) is what you *read*.
  `listSessions` returns both together.
- **Sending is fire-and-report**, exactly like `send_to_session`. It returns when
  the keystrokes land, not when the agent replies.
- **`waiting` means the agent stopped and asked the operator something.** Surface
  that to a human; do not answer it blindly.

---

## 3. The protocol

One JSON object per WebSocket **text** message. Three kinds, demultiplexed on the
presence of `id`:

| | shape |
|---|---|
| request (Redstone → bridge) | `{"id": 7, "method": "...", ...}` |
| response (bridge → Redstone) | `{"id": 7, "result": "...", ...}` |
| event (bridge → Redstone) | `{"event": "...", ...}` — no `id` |

A request gets one response with the same `id`. Events arrive unprompted. **Match
on `id` first**: an event that parsed as a response would resolve a random pending
request with unrelated data.

### 3.1 Requests

#### listSessions

```jsonc
→ {"id":1,"method":"listSessions"}
← {"id":1,"result":"sessions","sessions":[
     {"name":"claude-a1b2","kind":"claude","folder":"/home/dev.user/api",
      "title":"billing rewrite","conversationId":"2f9c8e10-…",
      "status":"busy","pid":4021,"ageSeconds":900,"attached":false},
     {"name":"pi-9f0c","kind":"pi","folder":"/home/dev.user/api","status":"unknown"},
     {"name":"term-3","kind":"shell","status":"unknown","ageSeconds":40,"attached":true}
   ]}
```

`kind` is `"claude"`, `"pi"` or `"shell"` — show the right icon without parsing a
name. `conversationId` links a live session to its transcript. Unions **every**
daemon on the host.

#### spawn

```jsonc
→ {"id":7,"method":"spawn","agent":"pi","folder":"~/api","prompt":"refactor the parser","name":"parser"}
← {"id":7,"result":"spawned","session":{"name":"pi-1a2b3c","kind":"pi"}}
```

- `agent` is `"claude"` (default) or `"pi"`. Omit it for Claude.
- The folder **must already exist** (`~` is expanded host-side). The prompt is a
  launch argument, shell-quoted.
- There is **no field for `--dangerously-skip-permissions`**. A spawned session
  always has its permission checks on.
- `modelProfile` is accepted and **refused** — profiles are applied by rmux.

#### send · interrupt · close

```jsonc
→ {"id":4,"method":"send","session":"claude-a1b2","message":"run the tests"}    // → ok
→ {"id":5,"method":"interrupt","session":"claude-a1b2"}                          // Escape
→ {"id":6,"method":"close","session":"claude-a1b2"}                             // ends it for good
```

`send` types the message and submits it; it returns when the keystrokes land, not
when the agent replies. These work on any session — Claude or pi — and need no
`agent` field. `{"result":"error","code":"notFound"}` is the routine outcome for
a session that has ended; say "that conversation has finished", not "failed".

#### listConversations

Every conversation on the host, running or not, newest first.

```jsonc
→ {"id":2,"method":"listConversations","agent":"pi","limit":50,"folder":"/home/dev.user/api"}
← {"id":2,"result":"conversations","conversations":[
     {"id":"pisess-…","folder":"/home/dev.user/api","summary":"refactor the parser",
      "modified":1782903431000,"running":false}
   ]}
```

`agent` defaults to `"claude"`. Always pass `limit` — a host in use for a year
holds thousands.

#### readConversation

```jsonc
→ {"id":3,"method":"readConversation","agent":"pi","conversation":"pisess-…","maxBytes":262144}
← {"id":3,"result":"conversation","truncated":false,"messages":[
     {"role":"user","text":"refactor the parser","at":"2026-08-18T09:59:47.768Z"},
     {"role":"assistant","text":"Done — split it into three passes.","at":"…"}
   ]}
```

> The field is **`conversation`, not `id`** — the envelope flattens the request,
> so a field called `id` would collide with the envelope's `id` and the frame
> would not parse. No request field may be called `id` or `method`.

- `maxBytes` defaults to 256 KiB, capped at 8 MiB. Transcripts reach hundreds of
  MB — always read the tail. `truncated: true` means the front was cut off.
- `role` is `user` · `assistant` · `tool` · `system`. **`system` is not the user
  talking** — plumbing and reminders are demoted for you; hide them by default.
- `at` is ISO-8601, verbatim from the record.

#### hostInfo

```jsonc
→ {"id":8,"method":"hostInfo"}
← {"id":8,"result":"host","host":{"hostname":"build-box","user":"dev.user","os":"linux","home":"/home/dev.user"}}
```

`user` is **the ceiling on everything this connection can do** — the bridge has
no privilege beyond that account's.

### 3.2 Terminals — read, drive, and stream live

**These verbs run commands.** A terminal has no permission prompt, so anything
sent to one runs at once. They are bounded to terminals the operator already
opened — there is no verb that opens a shell — but this is real command
execution; see §4. Terminals appear in `listSessions` with `kind: "shell"`.

**Request/response**, for running a command and reading the result:

```jsonc
→ {"id":10,"method":"readTerminal","session":"term-3","maxBytes":65536}
← {"id":10,"result":"terminal","output":"…recent output, escapes stripped…","truncated":false}

→ {"id":11,"method":"sendTerminal","session":"term-3","input":"ls -la","submit":true}
← {"id":11,"result":"ok"}
```

- `readTerminal` cleans the output best-effort so an agent gets words, not escape
  noise. The raw stream is on the live channel below.
- `sendTerminal` with `submit:true` appends a carriage return — that is what runs
  it. The output does not come back here; read it or watch the stream.
- Both refuse a missing session (`notFound`); neither creates one.

**Live stream**, for showing a terminal in Redstone's UI like a normal terminal:

```jsonc
→ {"id":12,"method":"attachTerminal","session":"term-3","cols":80,"rows":24}
← {"id":12,"result":"terminalAttached","session":"term-3"}

← {"event":"terminalOutput","session":"term-3","data":"<base64 raw bytes>"}   // backlog, then live

→ {"id":13,"method":"terminalInput","session":"term-3","data":"<base64 keystrokes>"}
→ {"id":14,"method":"resizeTerminal","session":"term-3","cols":120,"rows":40}
→ {"id":15,"method":"detachTerminal","session":"term-3"}       // keeps running
← {"event":"terminalExited","session":"term-3","code":0}       // if the shell ends
```

- **`terminalOutput` data is base64 RAW bytes** — backlog first, then live. Feed
  it straight into a terminal emulator (xterm.js); the escape sequences are the
  terminal's own — do not strip them or the cursor, colours and clears break.
- **`terminalInput` data is base64 keystrokes** — arrows, control codes and UTF-8.
- **Detach is not close.** The session keeps running; last attach wins on size.
- `terminalInput` before attaching is `notFound` — attach first.

### 3.3 Coding agents: Claude and pi

rmux runs two coding agents and Redstone drives both the same way. `spawn`,
`listConversations` and `readConversation` take the optional `agent` field —
`"claude"` (default) or `"pi"`. `send`, `interrupt` and the terminal verbs are
keystroke-level and need no agent field.

**One honest gap for pi:** pi writes no live status file, so a running pi
session's `status` is `"unknown"` and its `conversationId` is absent — it is not
auto-linked to its transcript by id. It is still fully drivable as a terminal,
and its conversations are fully listable and readable. (Validated against real pi
v0.84.2, including a real task run and reading its actual on-disk transcripts.)

### 3.4 Errors

```json
{"id":4,"result":"error","code":"notFound","message":"no session called claude-a1b2 on this host; it may have ended"}
```

| `code` | meaning | what to do |
|---|---|---|
| `notFound` | No such session or conversation | Re-list. Usually a stale list, not a fault |
| `refused` | Understood and declined | Show the message; it explains itself |
| `unsupported` | This agent build has no such method | Name the version that has it |
| `failed` | Something on the host went wrong | Show the message |

**Branch on `code`, never on `message` text.**

### 3.5 Events

```jsonc
{"event":"sessionStatus","session":"claude-a1b2","status":"waiting","at":1782903431000}
{"event":"sessionOpened","session":{}}
{"event":"sessionClosed","session":"claude-a1b2"}
{"event":"terminalOutput","session":"term-3","data":"<base64>"}     // §3.2
{"event":"terminalExited","session":"term-3","code":0}             // §3.2
{"event":"goingAway","reason":"host shutting down"}
```

`sessionStatus` is from the agent's own status file — only changes are sent, so
you never poll. `at` is a millisecond epoch on the **host's** clock. `goingAway`
means stand down rather than reconnect-loop.

### 3.6 Compatibility

- `protocol` in the greeting is this document's version, currently `1`.
- **An unknown method is answered with `unsupported`, not a dropped connection.**
  A newer Redstone against an older agent is the ordinary case.
- Unknown *fields* are ignored both directions. Adding an optional field is not a
  breaking change; adding a required one is.

---

## 4. Security

The token is a credential sitting on a development server. Everything follows
from that.

**Mint one token per host, never the user's own session token.** A dev box is a
machine other people have accounts on and which is rebuilt without ceremony. A
per-host token is revocable on its own; its blast radius is the verbs in §3
against one machine.

**The boundary moved, on purpose.** The first version had no terminal access, on
the rule "there is no `exec`, ever". The operator deliberately widened it:
terminal I/O to a session they opened is now allowed (§3.2). It is real command
execution with no permission prompt, bounded to hosts they enrolled *and* have a
session on. What did **not** move:

- **No verb creates a shell from nothing.** There is no "open a terminal" and no
  bare `exec`. The only session a remote caller can start is a Claude or pi one,
  under its own permission prompts. Every terminal verb carries a `session` that
  must already exist. `rmux-bridge` has a test that fails if a verb both executes
  and lacks a session, or spawns a terminal.
- **The blast radius stays "hosts the operator enrolled and has a session on."**

**A `prompt`, `message` or terminal input from Redstone is an instruction, by
design** — that is the feature. It is also why enrolment is an explicit, per-host
act by the operator, and why the verb set is closed. If Redstone's agent is
prompt-injected by a page it read, that injection reaches exactly these verbs on
exactly the hosts the user enrolled, and no further.

Already implemented on the rmux side: the token travels in the `Authorization`
header (never a frame, URL, or argv); `~/.rmux/redstone.json` is `0600` in a
`0700` dir, mode set before the token is written, and a world-readable file is
refused on read; unenrolling deletes the file and kills the bridge; a
conversation id is validated before it is joined onto a path; the bridge runs as
the operator's own account and has no privilege beyond it.

---

## 5. What rmux ships

| | |
|---|---|
| `rmux-agent bridge` | The client. Dials out, reconnects with backoff, answers §3 |
| `crates/rmux-bridge` | The wire contract, with tests |
| Enrolment + web-login sign-in | Writes the token to a host over stdin, starts the bridge |
| Terminals | read / send / attach-live, on any session the operator opened |
| Claude **and** pi | spawn / list / read, through the same verbs |
| `GET /rmux/config` probe | rmux controls stay hidden on a deployment without the bridge |

The agent grew from ~1.4 MB to ~2.9 MB (the TLS + WebSocket stack), uploaded to a
host once per build fingerprint. Not built: any Redstone-side UI, and the device
grant (optional, §2.3).

### 5.1 Verified against real hosts

- **Bridge, against a dev deployment** that hosted the endpoint at the time,
  through a public tunnel: `wss` dial, token accepted, one connection with zero
  reconnects over two minutes (keepalive works), unknown token → HTTP 403 with
  correct backoff. *(That endpoint is not on the current deployment — it 404s
  today; see Status. This run proves the rmux client, not that Redstone hosts it.)*
- **Terminals, on a real host:** listed, read history, sent a command and read the
  result, attached live, typed into the stream and saw the echo, detached.
- **pi, on a real host:** pi v0.84.2, a real task run (via an `openai-codex`
  login); `listConversations` and `readConversation` verified through the real
  bridge against pi's **actual on-disk transcripts** — which caught and fixed a
  format difference the source alone would have hidden.

Still unproven end to end: any request actually **sent by Redstone** — every
exchange so far has had a stand-in server on one side. That is what §2.4 unblocks.

### 5.2 Trying it without Redstone

A twenty-line WebSocket server is enough to drive a real host:

```python
# pip install websockets
import asyncio, json, websockets

async def handler(ws):
    print("greeting:", json.loads(await ws.recv()))

    async def call(n, **msg):
        await ws.send(json.dumps({"id": n, **msg}))
        return json.loads(await ws.recv())

    print("sessions:", await call(1, method="listSessions"))
    print("pi history:", await call(2, method="listConversations", agent="pi", limit=5))

async def main():
    async with websockets.serve(handler, "127.0.0.1", 8787):
        await asyncio.Future()          # serve forever

asyncio.run(main())
```

Then on the host:

```sh
mkdir -p ~/.rmux && chmod 700 ~/.rmux
umask 077 && cat > ~/.rmux/redstone.json <<'JSON'
{"endpoint":"ws://127.0.0.1:8787/bridge","token":"anything"}
JSON
~/.rmux/bin/rmux-agent-<version>-<fingerprint> bridge
```

`ws://` skips TLS, which is why this is a useful first step. It skips the
`Authorization` check and assumes responses arrive in order (true only with one
request in flight) — a real client matches on `id`.

---

## 6. Questions for the rmux side

Send these to the rmux repository rather than guessing:

- A method you need that is not in §3 — **except** anything that runs a command
  with no session behind it, which is answered in §4.
- Whether an event should carry more than it does. Events are cheap; a round trip
  from a Redstone worker to a dev box is not.
- Anything in §3 whose behaviour is not what this document says. The protocol has
  tests; the document is written from them, but they are the authority.
