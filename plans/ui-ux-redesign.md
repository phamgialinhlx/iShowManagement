# iShowManagement — UI/UX Redesign Plan

Outcome of the `/grill-with-docs` session on "redesign & improve the desktop app UI/UX
(change the BE if needed)". The desktop app is a thin Tauri wrapper that loads the Svelte
SPA from `web/dist` (`desktop/src/main.rs`), so **all redesign work is in `web/`**; `desktop/`
is untouched except possibly window chrome.

Visual spec (open in a browser): **`references/ui-ux/index.html`**.
Committed screens: `dark-minimal.html` (terminal), `minimal-docker.html` (table),
`minimal-home.html` (home). Skin tokens: `references/ui-ux/_minimal.css`.

---

## Locked decisions

| # | Decision | Choice |
|---|----------|--------|
| A | Session model | **Lazy explicit connect; persist until explicit disconnect.** No auto-connect. A host is "live" iff it has ≥1 open session. |
| B | Navigation | **Host-centric.** Sidebar is the primary axis; each host owns a persistent workspace; switching hosts is instant + lossless. |
| C | In-pane layout | **One segmented bar, no labels.** Left of a hairline divider: management panels `Overview · Docker · Ports · Processes · Files` (pick one). Right: open sessions `shell · tmux · logs:<cid> · Browser↗` (closeable, persistent) + `＋` to spawn a shell. |
| — | Browser | **Session tab that launches/controls external Chrome** over the host's `ssh -D` SOCKS proxy. Shows proxy status + open/close. **No embedded webview.** |
| D | Global surfaces | **Bottom status bar + home dashboard.** Status bar shows live totals (`sessions · forwards · proxies`) and is clickable. Home (no host selected) = host cards with live badges + active-tunnels list. Tunnels killable from **both** the global surface and the host's Ports tab. |
| E/F | Sidebar filter, collapsible sidebar, live-first ordering, command palette, shortcuts | **Out of scope for now.** |
| Skin | Visual direction | **Dark minimal.** Flat near-black surfaces, hairline borders, saturated color reserved for **status only** (`--run`/`--warn`/`--danger`), one quiet slate accent (`--accent`) for navigation/"you are here". Fonts `Geist` + `Geist Mono`. |

Parked for later (not now): layout #3 terminal drawer (split-view), embedded browser, command palette.

---

## Frontend work (`web/`) — the bulk

### 1. Session persistence (the core behavioral change)
Today `App.svelte` remounts `<Terminal>` via `{#key connKey}` on every host/tab switch,
which runs `onDestroy` → `socket.close()` → PTY dies. **New model:**
- Introduce a **session registry** in `App.svelte` (Svelte 5 rune state): a list of open
  sessions, each `{ id, hostId, kind: 'shell'|'tmux'|'docker-logs'|'docker-exec', cid?, wsKey }`.
- Render **all** open `<Terminal>` instances simultaneously; show only the active one
  (`display:none` on the rest — keeps xterm + WebSocket alive in the background). Never key-remount
  an existing session.
- The BE already supports N concurrent WS/PTYs (one per socket, ControlMaster-multiplexed), so
  **no BE change is needed for persistence** — it's purely keeping components mounted.
- "Live host" = host has ≥1 session in the registry → drives sidebar dot + count badge.
- Disconnect (top-bar) closes all of that host's sessions + tunnels.

### 2. Component restructure
- **`App.svelte`** — becomes the shell: sidebar + per-host workspace + status bar + home.
  Holds session registry, active host, per-host active-tab.
- **New `Workspace.svelte`** (or inline) — the segmented bar + content switch for the active host.
  Panels = single instance, re-created on switch (cheap). Sessions = persistent instances.
- **New `StatusBar.svelte`** — live totals; click → tunnels popover (jump/kill).
- **New `Home.svelte`** — host cards + active tunnels (replaces the "Select a server" empty state).
- **New `BrowserTab.svelte`** — proxy status panel: calls `openBrowser` (existing), shows
  `socksPort`, "Open browser window" + "Stop proxy" (new endpoint).
- Keep **`Managers.svelte`**, **`Files.svelte`**, **`Terminal.svelte`** logic; re-skin only.

### 3. Re-skin to dark-minimal
- Port `references/ui-ux/_minimal.css` tokens into the app: replace the palette in
  `App.svelte` `<style>` + `app.css`, and each component's colors
  (`Managers.svelte`, `Files.svelte`). Same hairline/flat/status-only-color rules.
- **Terminal theme** (`Terminal.svelte`): update xterm `theme` to the minimal palette
  (`background:#0b0c0e`, `foreground:#c4c6ca`, plus an ANSI set matching `--run/--warn/--accent`);
  `fontFamily` → `'Geist Mono', ui-monospace, Menlo, monospace`.
- **Fonts must be vendored** (Tauri runs offline — no Google Fonts CDN). Add `Geist` + `Geist Mono`
  woff2 to `web/src/assets/fonts/` and `@font-face` them in `app.css`. (CDN links in the mockups are
  fine for `references/` only.)

---

## Backend work (`core/`) — small, additive

Only two gaps; both additive, no changes to existing routes.

1. **`GET /api/tunnels`** — aggregate active forwards + proxies for the status bar / home.
   Reads existing `AppState.forwards` + `AppState.proxies` (already tracked). Returns e.g.
   `{ forwards: [{alias, remotePort, localPort}], proxies: [{alias, port}] }`.
2. **Stop a proxy** — currently proxies are created (`browser.rs` `ensure_proxy`) but there's no
   stop route. Add `DELETE /api/servers/{id}/proxy` → drop the `ProxyEntry` (Drop kills the
   `BgSsh`). Mirrors the existing `unforwardPort` (`DELETE …/ports/{port}/forward`).

`api.ts` additions: `getTunnels()`, `stopProxy(id)`. `openBrowser` already returns `socksPort`.

Security posture unchanged (loopback bind, origin guard, `SAFE_NAME`, secrets encrypted).

---

## Risks / notes
- **Many live PTYs**: persistence means several `ssh`/PTY processes can run at once. Acceptable —
  the user explicitly controls what's open; ControlMaster limits real connections. Disconnect frees them.
- **xterm background instances**: hidden-but-mounted xterms keep buffers in memory; fine for the
  handful a user keeps open. `fit()` must run when a session is re-shown (it was `display:none`).
- **Font licensing**: Geist is OFL (OK to bundle). Verify before vendoring.
- Desktop repackage after: `cd web && npm run build` → `cargo tauri build`.

---

## Iteration: tmux navigation + persisted recency (post-implementation)

- **Tmux is a sidebar dropdown, not a workspace panel.** Each live+active host row in the
  left sidebar reveals a nested disclosure tree (`web/src/lib/HostTmuxTree.svelte`) listing
  that host's tmux sessions (status glyph = open-here / on-host / attached-elsewhere, name,
  window count, close-to-detach). Clicking a leaf attaches it and opens the session as a
  normal top **session tab** (like a shell). The earlier `TmuxNav.svelte` master–detail
  workspace panel and the `Tmux` segment tab were removed. Backend `GET …/tmux` is unchanged.
- **Recency now persists.** Replaced the in-memory `recency: string[]` with a per-host
  `last_accessed` (unix secs) in `store.rs` (`state.json`), exposed as `lastAccessed` on
  `ServerDto`/`Server`, stamped via `POST /api/servers/{id}/touch` on `selectHost` (optimistic
  local update + fire-and-forget persist). Sidebar orders live/idle lists by `lastAccessed`
  desc (nulls keep ssh-config order via a stable sort). Survives restart/reinstall because
  `dirs::data_dir()` (macOS `~/Library/Application Support`) is not cleared on bundle replace.

## Iteration: Claude Code notifications (native banner when Claude finishes / needs you)

Goal: the user runs Claude Code inside remote tmux; they want a macOS banner when
Claude **completes a turn** or **asks for input/permission**.

- **Detection = Claude Code hooks.** `Stop` (once per turn = "done") and
  `Notification` with matchers `permission_prompt` / `idle_prompt` (= "needs you").
  Payload carries `cwd`, `message`, `last_assistant_message` → richer banners.
- **Transport = file + poll over SSH** (hooks can't reach the tmux pane / don't
  inherit `$TMUX`, so in-band was out). A hook runs `~/.claude/ism-notify.sh` that
  appends `<kind>\t<compact-json>` to `~/.ism/notify.jsonl`. The app polls the tail
  of that file (byte-cursor) over the ControlMaster it already holds, **only for
  hosts with ≥1 open session** (per the user's "only sessions I've opened" choice).
  Poll rides the existing 4s tunnel-refresh interval; init skips history.
- **Consent-gated install.** Nothing touches a host until the user clicks Enable in
  a non-modal card (`web/src/lib/ClaudeNotifySetup.svelte`) shown when a live remote
  host lacks the hook. Install **merges** into existing `~/.claude/settings.json`
  (JSON merge done app-side in Rust, idempotent, preserves the user's own hooks);
  reversible via the top-bar `🔔 Notifications` toggle → uninstall (strips only our
  entries, prunes empties, removes the script).
- **Banner + badge.** Banner fired by core via `osascript` (`POST /api/notify`);
  webview also increments a sidebar badge on the host row when it isn't focused.
- **BE additions:** `core/src/notify.rs` (`GET/POST/DELETE /api/servers/{id}/claude-notify`,
  `GET …/claude-notify/events?cursor=N`) + `POST /api/notify` (`api::notify`, osascript).
  `api.ts`: `getNotifyStatus/installNotify/uninstallNotify/getNotifyEvents/notify`.
- **Known limitation:** file-based, so it works even outside tmux; but with
  "only opened sessions" the poll only runs while the host has a session open in the
  app. Host-wide watching (notify for a Claude you haven't opened) is a later upgrade.
- **Status:** built + self-verified (31 core tests incl. merge idempotency/parse;
  shell script + cursor logic validated locally; osascript banner fires; svelte-check
  clean; web builds). Live end-to-end (install on a real host → Claude fires → banner)
  is user-driven via the consent card (won't silently edit a remote's Claude config).

## Status: IMPLEMENTED ✅ (branch `ui-ux-redesign`)
All build-order steps done. Web builds (`npm run build`, fonts vendored via `@fontsource/geist-*`),
`svelte-check` clean, 25 core tests pass, `cargo tauri build` produced `.app` + `.dmg`, embedded
server verified serving. New files: `web/src/lib/{StatusBar,Home,BrowserPanel}.svelte`. BE additions:
`GET /api/tunnels`, `DELETE /api/servers/{id}/proxy`. Not yet merged to `main`; not committed.

## Suggested build order
1. BE: `GET /api/tunnels` + `DELETE …/proxy` (+ `api.ts`). Small, unblocks status bar/home.
2. Session registry + persistent Terminal rendering (behavioral core). Verify: open shell on prod,
   switch host, come back — session still alive.
3. Segmented workspace bar (panels ┊ sessions + `＋`, Browser tab).
4. Status bar + home dashboard + tunnels popover.
5. Dark-minimal re-skin (tokens, components, xterm theme, vendored fonts).
6. `npm run build` + `cargo tauri build`; smoke-test the packaged app.
