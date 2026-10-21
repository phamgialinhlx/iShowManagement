# rmux-web — the same rmux, in a browser

rmux-web serves rmux's own UI and its own command implementations over HTTP and a
WebSocket, so a phone or an iPad gets the workbench without a desktop app. It is not
a second product: **one command table drives both the desktop and the web**, and the
UI is the same bundle with a different bridge underneath.

## Where it runs and who can reach it

- **On the dev host** (first deployment: yitec-pcloud), not on the operator's Mac. The
  Mac is frequently closed; the host is not, and its sessions are already local there.
  This is the reasoning that put the Redstone bridge on the host.
- **Bound to `127.0.0.1` only.** It is reached through the host's existing cloudflared
  tunnel, behind **Cloudflare Access** (SSO).
- **The server verifies the Access JWT itself** (`Cf-Access-Jwt-Assertion`, checked
  against the team's certs, audience pinned) on every request *and* every WebSocket
  upgrade. Access in front is not enough on its own: anything on the host or the LAN
  that reaches the port would otherwise be a shell with no login. Until the JWT check
  exists, the prototype requires a per-run token and is reachable only through an SSH
  tunnel.
- **WebSocket upgrades check `Origin`.** A page on another site must not be able to
  open a terminal in the operator's browser session.

This is shell access to a dev machine from a web page. Every rule above exists because
removing it turns "rmux on my phone" into "a shell for whoever finds the URL".

## The shape

```
crates/rmux-app      tauri-free: Ctx (stores), Host trait, Sink trait, Registry,
                     and the command implementations moved out of src-tauri
src-tauri            desktop: Host impl (AppHandle, keychain, resources),
                     an invoke handler that dispatches the Registry first,
                     then the desktop-only `generate_handler!` commands
crates/rmux-web      web: Host impl (Linux paths, file-backed secrets or none),
                     axum — static UI, POST /api/invoke/{cmd}, WS /api/ws
ui/src/lib/bridge.ts the only file that knows which shell the UI is in:
                     invoke / Channel / listen, Tauri or web
```

### The four seams

- **`Registry`** — command name → handler taking typed, `camelCase`-deserialised args
  (the same key convention Tauri uses, so the UI's call sites do not change). Each
  module registers its own commands; there is no central list to forget to edit.
- **`Host`** — what a command needs from the shell around it: resource dir (bundled
  agents), data and cache dirs, `emit(event, json)`, and secrets (`None` where there
  is no keychain). Desktop implements it over `AppHandle`; web over the filesystem.
- **`Sink`** — a stream back to the UI: `send_bytes` (terminal output, raw) and
  `send_json`. Desktop resolves a channel id to a Tauri `Channel` on the invoking
  webview; web resolves it to a frame on the page's WebSocket, multiplexed by id.
- **`bridge.ts`** — `call`, `Channel`, `listen`. Nothing else in the UI imports
  `@tauri-apps/api` directly.

### What keeps the web version current

- A command added to a module's `register` is live in both shells with no other edit.
- **Every command is classified.** The registry carries a `web: bool`; desktop-only
  commands (windows, glass, face camera, keychain-only flows) stay in
  `generate_handler!`. A test fails when a `#[tauri::command]` exists that is neither
  registered nor on the explicit desktop-only list, so a new command cannot silently
  be missing from the web.
- rmux-web is cross-compiled with the agents (zig, linux-musl) and provisioned to a
  host the same way the agent is — so connecting from a newer desktop upgrades it.

## Invariants carried over unchanged

Everything in `CLAUDE.md` still holds inside the moved code: `shell_quote` on remote
lines, NUL-separated listings, length-framed reads, copy-over-original saves,
noclobber uploads, secrets never in argv, `Tty::None` for one-shots. Moving a command
is a **move**, not a rewrite: behaviour must be byte-for-byte what it was.

## Phases

1. Prototype: rmux-app + registry + Tauri bridge; terminals, files, Claude, pi, git
   moved; rmux-web serving them on yitec-pcloud behind a token, via SSH tunnel.
2. Classification test; the remaining portable commands moved.
3. Cloudflare Access hostname + JWT verification + Origin check.
4. Phone/iPad layout: one pane, drawer rail, a key bar (Esc, Tab, Ctrl, arrows).
5. Provisioned/upgraded from the desktop; workspace stored on the host and shared.

## Wire protocol (web)

One WebSocket per page carries every stream; commands are plain HTTP.

- `GET /api/ws` → upgrade. The server's first frame is text `{"hello":{"conn":"<id>"}}`.
  The page opens it lazily (first `listen`, or first invoke carrying a `Channel`),
  keeps it for its lifetime and reconnects with capped exponential backoff
  (0.5s doubling to 8s, reset by a `hello`). A new socket is a new `conn`, and
  channels bound to the old one are gone. `bridge.ts` does **not** resurrect them:
  it calls every `onReconnect(cb)` hook with the new conn id, and the pane
  reattaches, exactly as a desktop pane does after a reload. A `Channel` object may
  be sent again after a reconnect; it is re-bound to the conn of the invoke that
  carries it.
- `POST /api/invoke/<cmd>` with the args object as JSON and header `X-Rmux-Conn: <id>`
  (sent whenever the page has a conn; an invoke that carries a channel waits for
  `hello` first, up to 15s, and fails with a message rather than hanging).
  `200` → the JSON result; an empty body is read as `null` (a command returning
  `()`). Non-2xx → `{"message": "...", "requiresSignin"?: bool}`, which `bridge.ts`
  throws as-is so `api.ts` turns it into the same `ApiError` the desktop path throws.
  A non-JSON error body is reported as `<cmd>: HTTP <status> — <first 200 chars>`.
- **Channels.** The page allocates a numeric id (u32) per `Channel` and passes it in
  the args as the string `"__CHANNEL__:<id>"` — the same string Tauri's own
  `Channel.toJSON()` produces, so a Rust handler sees one format from both shells.
  The server binds it to `X-Rmux-Conn`.
  - Binary frame: 4-byte big-endian channel id, then the raw bytes → `onmessage(ArrayBuffer)`.
  - Text frame `{"channel": <id>, "data": <json>}` → `onmessage(json)`.
  - Text frame `{"channel": <id>, "end": true}` → the page forgets the channel (the
    equivalent of Tauri's channel end; optional, but without it a page that opens
    and closes many panes keeps every dead channel's handler alive until reload).
- **Events.** Text frame `{"event": "<name>", "payload": <json>}` → every `listen(name)`,
  whose handler receives `{event, id, payload}` like Tauri's `Event<T>`.
- Unparseable or unknown frames are logged and dropped, never thrown: one bad
  frame must not take the socket and every terminal on it down. The decoder is
  `ui/src/lib/bridge-frames.ts`, pinned by
  `node --experimental-strip-types ui/bridge-frames-check.ts`.
- **Files.** `GET /api/file?path=<url-encoded absolute path>&token=<token>` serves a
  file the UI needs as a URL — today, a background picture (`convertFileSrc`, which
  is the Tauri asset protocol on the desktop). It must be scoped exactly as the
  desktop asset protocol is (the app's backgrounds directory), never an arbitrary
  path: an unscoped file endpoint is a read of the whole host.
- **Auth.** Until Access JWT verification lands, every request carries the token.
  The page reads it once from `#token=` (a fragment is never sent to a server, so
  it cannot reach an access log on the way in), keeps it in `sessionStorage`, and
  strips it from the address bar with `history.replaceState`. HTTP requests send
  `Authorization: Bearer <token>`. A browser **cannot set headers on a WebSocket
  upgrade** or on an `<img>` load, so `/api/ws` and `/api/file` take it as a
  `?token=` query parameter instead; the server accepts either form on every
  route. The query form can land in proxy logs, which is the reason it is limited
  to those two routes and the reason the token is per-run.
- **Which shell.** `bridge.ts` treats a page as rmux-web when it has a token, or when
  the server marks the page with `<meta name="rmux-shell" content="web">` (for when
  the JWT replaces the token). Otherwise a non-Tauri page is a plain browser tab
  (design work, the check harnesses) and stays inert rather than calling `/api`.
- **Guard.** `node ui/bridge-imports-check.mjs` fails if anything under `ui/src`
  other than `lib/bridge.ts` imports `@tauri-apps/api`.
