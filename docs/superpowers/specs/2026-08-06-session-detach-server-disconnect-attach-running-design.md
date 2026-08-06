# Session detach/close, server Disconnect/Remove, attach a running session

## Context

The session rail (`ui/src/components/WorkspaceRail.tsx`) is a Server → Project →
Session tree. Two usability gaps and one cross-machine capability:

1. **Closing a session kills it.** The rail's ✕ (and its confirm) calls
   `removeSession`, which fires the agent kill and **ends the shell/Claude on the
   server**. Detaching (leave it running, re-attach later) is only implicit — it
   happens if you close the *pane* rather than the row. The operator wants detach
   to be the default close, with the destructive kill behind a deliberate action.

2. **A server can't be disconnected.** `removeServer` kills every session and drops
   the structure, but the Rust side caches `Arc<SshTarget>` forever
   (`TerminalStore.targets`, `ClaudeStore.targets`), so the ControlMaster SSH
   connection never closes while the app runs. There is no "drop the connection but
   keep sessions running" verb, and no way to disconnect a server from the UI.

3. **Sessions already running on a host are invisible to a second PC.** The agent
   daemon is multi-attacher by design (a second `rmux-agent attach --session NAME`
   against the same daemon joins the same PTY/Claude — broadcast + shared PTY
   master, no single-attacher lock). But each PC mints its own session ids
   (`mintSessionId`: `Date.now().toString(36)` + a per-app `seq`), and nothing
   surfaces what the agent is running. So a session started on PC-A cannot be
   attached from PC-B — PC-B neither knows its name nor sees it in the rail.

## Goals

- **Detach = default close.** Closing a session row (✕ or menu "detach") leaves the
  process running under the agent and keeps a dimmed row in the rail; clicking it
  re-attaches with scrollback replay.
- **Kill = deliberate.** The real end (agent kill on the host) lives in the session
  right-click menu as **Close session**, behind a confirm-in-place.
- **Server Disconnect.** A right-click menu on the server card offers **Disconnect**
  (close the SSH connection, keep sessions running, row stays) and **Remove server**
  (existing destructive cascade).
- **Attach a running session.** A way to list what the agent is running on the host
  (`agent list` — name, pid, age, attached) and adopt any of them into the rail,
  including sessions another PC started. Adopted sessions reattach to the running
  process rather than spawning a duplicate.

Non-goals (deferred to a later phase): deterministic cross-PC shared names for
*new* sessions, live mirroring on both PCs, surfacing the `attached` bool in the
rail.

## Design

### 1. Session rows: detach is the default close

The store gains `detachSession(id)`:

- Close every pane pointing at the session (`assignPane` the tiles to `null` — the
  same structural effect `closePane` has, but for all tiles).
- Clear the live runtime handle (`live[id]`, and the runtime `status` can stay).
- **Do not** send the agent kill.
- **Do not** remove the session from `sessions` — the row stays in the rail.

Re-attaching is the existing path: click the row → `openSession(id)` → pane mounts
→ `claude_start` / terminal attach reattaches to the same `reattachName` on the
host and the daemon replays scrollback. Detach/re-attach is therefore exactly how
the app already survives a restart; we are making it the ✕ default.

`removeSession` (the kill) is unchanged in behaviour but moves behind the menu.

**UI (WorkspaceRail `SessionRow`):**

- The row ✕ button now calls `detachSession` (no confirm — it is the safe default).
- The right-click menu becomes **Rename / Detach / Close session**.
  - Rename → existing inline edit (`setEditing(true)`).
  - Detach → `detachSession`.
  - Close session → the existing confirm-in-place ("ENDS THIS SHELL ON THE SERVER")
    → `removeSession` (the kill).
- Detached rows already render dimmed (the `onScreen`-derived surface); a detached
  row is simply one that is not currently in a pane. No new visual state needed.

### 2. Server rows: Disconnect / Remove

**Rust — new command `server_disconnect(target)`:**

The two stores that cache targets must both release the entry so the
`Arc<SshTarget>` drops and `ControlMaster::drop` closes the multiplexed SSH
connection:

- `TerminalStore.targets: Mutex<HashMap<TargetId, Arc<dyn Target>>>`
  (`src-tauri/src/terminal.rs`).
- `ClaudeStore.targets: Mutex<HashMap<TargetId, Arc<dyn Target>>>`
  (`src-tauri/src/claude.rs`).
- `AgentStore.by_target: Mutex<HashMap<TargetId, Arc<OnceCell<Installed>>>>`
  (`src-tauri/src/agent.rs`) — the provisioning cache should also forget the host
  so a later use re-probes rather than trusting a stale install.

The command takes a `TargetRef` (or `TargetId`), removes it from each map, and lets
the `Arc` drop. If the `Target` is `LocalTarget`, the removal is a no-op (no SSH
connection to tear down). It returns `()`.

**UI (WorkspaceRail `ServerNode`):**

- Add `onContextMenu` to the server card → `RailMenu` with:
  - **Disconnect** → `api.disconnectServer(target)` (new Tauri invoke).
  - hairline divider.
  - **Remove server** → confirm-in-place naming the server, "its sessions will be
    ended on the server", `btn btn-primary` Remove → `removeServer(id)`.

`removeServer`'s behaviour is unchanged; it just gains a UI home.

### 3. Attach a running session (already-on-server)

**Rust — list sessions on the host.**

`agent list` already exists (`crates/rmux-agent/src/attach.rs:371-417`) and reports
name, pid, age, attached, command — but it is not surfaced anywhere in the app. Add
a Tauri command `server_sessions(target)` that runs `agent list` over the existing
SSH connection and returns the parsed rows. The agent's `list` already talks to the
same daemon socket and unions every daemon (including other builds' daemons), so it
sees sessions another PC started.

**UI — adopt a running session:**

- The server card's right-click menu gains **Attach to running session…**, and/or a
  small button on the server row.
- Choosing it opens a picker (a `RailMenu`-style panel or a small overlay) listing
  the discovered sessions: name, age, whether attached, the command it runs.
- Choosing a row **adopts** it into the rail as a local session:
  - A new store action `adoptServerSession(serverId, name, kind)` creates (or reuses)
    a local `SessionV3` whose id is derived from the host name, and whose project is
    the server's default (or a "running sessions" bucket under the server if no
    matching project exists).
  - Clicking the adopted row attaches to `name` — `claude_start` / terminal attach
    reattaches to the *running* process on the host (multi-attach). The daemon
    replays scrollback; both PCs now share the same PTY/Claude.

Adopted sessions whose name doesn't map to a local project hang under the server as
a small "running sessions" group, rather than being forced into a project folder.

### Data flow summary

```
Detach:   ✕ → detachSession(id) → clear panes + live, no kill
Close:    menu → confirm → removeSession(id) → agent kill (unchanged)
Disconnect: menu → server_disconnect(target) → drop Arc<SshTarget> from caches
Remove:   menu → confirm → removeServer(id) → cascade kills + drop (unchanged)
Attach:   menu → server_sessions(target) → agent list → pick → adoptServerSession
          → reattach by name → multi-attach to the running process
```

### Error handling

- `server_disconnect` on a `LocalTarget` is a no-op (no SSH connection).
- `server_sessions` when the agent is not installed falls back to empty/error,
  matching `ensure_agent`'s existing refusal on Windows.
- Adopting a session that no longer exists on the host: the reattach fails at
  `open_or_attach` (daemon spawns a *fresh* shell under that name if the old one
  exited) — the row just reattaches to a new process, which is the daemon's
  existing behaviour. No special casing needed.

### Testing

- **Store:** pure reducer-level tests for `detachSession` (panes clear, session
  stays, no kill sent) in `ui/workspace-reducers-check.ts`.
- **Rust:** `server_disconnect` removes the target from both stores (unit test with
  a stub target); `server_sessions` parses `agent list` output (existing parse
  functions can be unit-tested).
- **Manual:** connect a server, open a terminal, ✕ it → row stays dimmed, process
  still running on host (`agent list` confirms); right-click session → Close →
  process ends. Right-click server → Disconnect → ControlMaster socket gone;
  Remove → sessions end. Start a session from one PC, list it from another, adopt
  and attach.
