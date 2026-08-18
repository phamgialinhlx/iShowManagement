# Follow-up: sign-in changed, and what we need from you

**This supersedes §2.3 of the bridge spec.** Everything else in that document
stands.

**Short version:** we dropped the device-grant requirement. rmux now signs in
through **your existing web login**, so there is nothing new for you to build
for authentication. In exchange we need you to confirm four things about the
login page and the session cookie — most of which your own desktop spec already
promises.

The one thing still genuinely outstanding is **§2.4, the agent tools**. Until
those exist, the bridge works but the agent cannot reach it.

---

## 1. What changed on our side

The bridge spec said rmux was blocked on the device authorization grant
(RFC 8628), because a desktop app cannot hold a `client_secret`. That was true
and it is no longer relevant.

rmux now does what `docs/desktop/redstone-desktop-spec.md` §4 already prescribes
for the Electron shell:

1. The operator types their Redstone address — `redstone.example`, nothing else.
2. rmux opens **your login page** in a window. Your normal form, your normal SSO,
   your normal second factor. No password is ever typed into rmux.
3. rmux reads the **`rs_token`** cookie your web app sets on that origin.
4. It proves the token with `GET /api/v1/rmux/hosts` before storing it.
5. From then on rmux mints host tokens itself with `POST /api/v1/rmux/hosts`.

The operator never sees or carries a token. Enrolling the second machine is one
button.

**The login window cannot touch rmux.** It loads a remote origin, and a remote
domain has to be listed in Tauri's `dangerousRemoteDomainIpcAccess` to reach any
command. rmux lists none. The window renders your app and nothing else; we read
one cookie from the native cookie store and never inject script into your page.

The device grant is still implemented client-side and still welcome — it is a
tidier flow and it avoids rendering a login page in a webview at all. It is
simply no longer a gate, so build it when it suits you.

---

## 2. What we need you to confirm

Four things. Three are almost certainly already true; the fourth is the one that
can genuinely break this.

### 2.1 The cookie contract

| | |
|---|---|
| Name | `rs_token` |
| Origin | the same origin as `/api/v1/...` |
| `HttpOnly` | **fine** — we read the native cookie store, not `document.cookie` |
| `Secure` | fine, and preferred |
| `SameSite` | irrelevant to us |

If the name or the origin is not what your desktop spec says, tell us the real
values — it is a one-line change.

The awkward case is a cookie scoped to a *different* origin from the API (for
example an auth subdomain). We read cookies for the address the operator typed,
so a token that only exists on `auth.redstone.example` while the API lives on
`redstone.example` will not be found. If that is your layout, say so.

### 2.2 `rs_token` must work as a Bearer token on `/api/v1/rmux/*`

Your desktop spec says to send it as `Authorization: Bearer <token>` on every
API call, so we assume yes. We verify with:

```
GET /api/v1/rmux/hosts
Authorization: Bearer <rs_token>
```

A `200` is what makes rmux accept the sign-in. If that endpoint rejects web
session tokens and wants an OAuth2 access token instead, sign-in will appear to
hang — the cookie is there and the check never passes. **That would be the most
confusing possible failure**, so it is worth an explicit answer.

### 2.3 The login page must render in an embedded webview

**This is the real risk and the only one we cannot work around.**

rmux opens your login page in a WKWebView (macOS) / WebView2 (Windows). That is
fine for a normal form. It is *not* fine for some third-party SSO:

- **Google blocks OAuth in embedded webviews outright** (`disallowed_useragent`).
  If signing in to Redstone means "Continue with Google", this path cannot work
  and we fall back to the pasted token.
- Microsoft and Okta generally permit it; some tenant policies do not.
- Any "unsupported browser" interstitial of your own will do the same.

Tell us which sign-in methods your deployments actually use. If Google SSO is in
play for real customers, the **device grant becomes worth building after all** —
it is the standard answer precisely because it sends the operator to their real
browser.

### 2.4 `POST /api/v1/rmux/hosts` — the `endpoint` field

Unchanged from the spec, restated because it now matters more: return the
websocket `endpoint` explicitly. rmux will fall back to
`wss://<host>/api/v1/rmux/bridge` if you omit it, but that fallback is a guess,
and a deployment that terminates websockets elsewhere would break on it.

---

## 3. Two corrections from testing against your dev deployment

Both measured with the shipped agent, against
`/api/v1/rmux/bridge` through your public tunnel.

**An unknown token gets HTTP 403 before the upgrade, not the 1008 close you
described.** Both are handled on our side and both back off correctly, but they
are *different client code paths* — 403 fails the dial, 1008 is a policy close
after a successful upgrade. Whichever one you did not mean to ship is the one
that is untested. Our guess is that 1008 is the revoke-while-connected case,
which matches your 12/12 run; worth confirming and worth fixing in your docs
either way.

**Your keepalive works and needs no change.** One connection, zero reconnects,
held over two minutes through the tunnel. Removing your application-level ping
was the right call — we would have answered `unsupported` to every one. Worth
knowing *why* it works, though: the pong has to flush from the read path with no
application traffic in either direction, and a client that queues pongs without
flushing would die at your idle timeout while looking like a network fault. Ours
is now pinned by a test.

`GET /api/v1/rmux/config` returns `orgName: ""`. Cosmetic — we do not depend on
it — but if you intend to show it anywhere, it is empty.

---

## 4. Still outstanding: §2.4, the agent tools

This is the only thing between the current state and a working demo.

The bridge is live and answers `listSessions`, `listConversations`,
`readConversation`, `send`, `interrupt` and `spawn`. Every one has been exercised
against a real host. What does not exist is anything that lets your **agent**
call them — the four tools in §2.4 of the bridge spec, siblings of the ones in
`backend/app/agents/tools/dashboard.py`.

Three things to get right in the tool descriptions, repeated here because the
model reads only those:

- **`session` and `conversationId` are different keys.** `session`
  (`claude-a1b2`) is what you send *to*; `conversationId` (a UUID) is what you
  *read*. `listSessions` returns both together.
- **Sending is fire-and-report**, exactly like `send_to_session`. It returns when
  the keystrokes land, not when Claude replies.
- **`waiting` means Claude has stopped and asked the operator something.** That
  is the state most worth surfacing to a human, and the agent should not answer
  it blindly.

---

## 5. What we are not asking for

- **No device grant, for now.** See §1. Revisit only if §2.3 rules out the
  webview login.
- **No `exec`, ever.** Unchanged and not negotiable — see §4 of the bridge spec.
- **No changes to the wire protocol.** It is stable at version 1 and both sides
  have implemented it.
