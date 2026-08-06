# Session Detach/Close, Server Disconnect/Remove, Attach Running Session — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make detach the default way to close a session (kill moves behind the right-click menu), give servers a Disconnect/Remove right-click menu that actually closes the SSH connection, and let the operator list and attach to sessions already running on a host (including ones another PC started).

**Architecture:** UI-only changes live in the Zustand store (`ui/src/lib/workspace.ts` + pure reducers in `workspace-reducers.ts`) and the rail (`WorkspaceRail.tsx`). Rust gains one new Tauri command (`server_disconnect`) that evicts the cached `Arc<dyn Target>` from the three store maps, and one (`server_sessions`) that runs `agent list` over the existing SSH connection and parses `SessionSummary` rows. The agent daemon is already multi-attacher, so attaching to a running session is the existing `claude_start`/`terminal_open` path.

**Tech Stack:** Rust (Tauri v2, tokio, parking_lot), React 19 + TypeScript + Zustand, `cargo test`/`cargo clippy`, `pnpm exec tsc --noEmit`, Vite SSR checks.

## Global Constraints

- `cargo test --workspace` and `cargo clippy --workspace --all-targets` must be clean.
- `pnpm exec tsc --noEmit` clean; `pnpm exec vite build` succeeds.
- **Detach never kills.** `detachSession` must not send an agent kill and must not drop the session from `sessions`.
- **Kill is deliberate.** The destructive path (`removeSession`, which fires agent kills) is reachable only behind a confirm-in-place.
- **Rule 0 (red = operator must act).** "Remove server" / "Close session" / "Remove project" confirm copy names the target in `rgb(var(--primary))`; the destructive button is `btn btn-primary`. Non-destructive verbs stay monochrome.
- **Rule 3 (no emoji).** Inline SVG only.
- **`TargetRef` is the wire type** for a machine (`host`/`user`/`port`), with `TargetRef::id()` deriving the `TargetId`. Reuse it for the new commands.
- **Windows:** `ensure_agent` refuses on Windows with a reason; `server_sessions` must surface that (return `Err`) rather than hang. `server_disconnect` on a `LocalTarget` is a no-op.
- Never put `$HOME` in a shell_quote'd path; new commands take `TargetRef`, not paths.
- Session ids are **per-PC** today and stay that way this round; adopted sessions reuse the *host* name, they do not remint ids.

---

### Task 1: Rust — `server_disconnect` command

**Files:**
- Create: `src-tauri/src/server.rs`
- Modify: `src-tauri/src/lib.rs` (register the command)
- Test: `src-tauri/tests/server_disconnect.rs` (new integration test, compiled only when a Tauri test harness exists — see below)

**Interfaces:**
- Produces: `pub async fn server_disconnect<R: tauri::Runtime>(store: State<'_, ServerStore>, terminal: State<'_, TerminalStore>, claude: State<'_, ClaudeStore>, agent: State<'_, crate::agent::AgentStore>, target: TargetRef) -> Result<(), String>`
- Consumes: `TerminalStore.targets` (`src-tauri/src/terminal.rs:28-34`), `ClaudeStore.targets` (`src-tauri/src/claude.rs:20-24`), `AgentStore.by_target` (`src-tauri/src/agent.rs:18-24`), `TargetRef::id()` (`src-tauri/src/terminal.rs:49-60`).

- [ ] **Step 1: Write the failing unit test**

The `Arc<dyn Target>` maps are private to their stores, so the test asserts the observable behaviour: after `server_disconnect`, a `resolve_target` for that target must **re-create** the connection (i.e. the target is no longer cached). Use a `ServerStore` with a `resolve` that records cache hits. Add `src-tauri/tests/server_disconnect.rs`:

```rust
//! `server_disconnect` must evict the cached target so the ControlMaster drops
//! and the next use reconnects. Tested against the store maps directly.

use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::HashMap;
use rmux_transport::{Target, TargetId};

// A stub target that counts how many instances exist.
#[derive(Default)]
struct CountingTarget { count: Arc<std::sync::atomic::AtomicUsize> }
impl CountingTarget {
    fn new(count: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self { count }
    }
}
// (implement rmux_transport::Target for CountingTarget — see the existing
//  SshTarget / LocalTarget impls for the required methods; a minimal impl that
//  returns canned values for everything is fine for this test)

// The real eviction logic lives in src-tauri/src/server.rs as a free function
// so it can be tested without booting Tauri:
//   pub fn evict(id: &TargetId, stores: &mut [&mut HashMap<TargetId, Arc<dyn Target>>]) -> bool
// which removes `id` from every map and returns whether anything was removed.
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rmux --test server_disconnect`
Expected: FAIL — `evict` is not defined.

- [ ] **Step 3: Write minimal implementation**

Create `src-tauri/src/server.rs`:

```rust
//! Server-level IPC: disconnecting a server (drop the SSH connection) and
//! listing sessions already running on it.

use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::HashMap;
use rmux_transport::{Target, TargetId};
use tauri::State;

use crate::agent::AgentStore;
use crate::claude::ClaudeStore;
use crate::terminal::{TargetRef, TerminalStore};

/// Remove `id` from every target map. Returns whether any entry was dropped.
/// Dropping the last `Arc<SshTarget>` tears down the ControlMaster (mux.rs),
/// closing the multiplexed SSH connection — sessions on the host keep running.
pub fn evict(
    id: &TargetId,
    terminal: &Mutex<HashMap<TargetId, Arc<dyn Target>>>,
    claude: &Mutex<HashMap<TargetId, Arc<dyn Target>>>,
) -> bool {
    let mut removed = false;
    if terminal.lock().remove(id).is_some() { removed = true; }
    if claude.lock().remove(id).is_some() { removed = true; }
    removed
}

/// Disconnect a server: drop the cached SSH target(s) so the ControlMaster
/// closes. Sessions keep running on the host. A `LocalTarget` is a no-op.
#[tauri::command]
pub async fn server_disconnect(
    terminal: State<'_, TerminalStore>,
    claude: State<'_, ClaudeStore>,
    agent: State<'_, AgentStore>,
    target: TargetRef,
) -> Result<(), String> {
    let id = target.id();
    // AgentStore holds `OnceCell<Installed>`, keyed by TargetId — forget it so a
    // later use re-probes rather than trusting a stale install.
    agent.by_target.lock().remove(&id);
    evict(&id, &terminal.targets, &claude.targets);
    Ok(())
}
```

**Note:** the `AgentStore.by_target` field is `Mutex<HashMap<TargetId, Arc<OnceCell<Installed>>>>`. `targets` fields on `TerminalStore`/`ClaudeStore` are `Mutex<HashMap<TargetId, Arc<dyn Target>>>`. `TargetRef` and its `id()` live in `crate::terminal` (they are `pub(crate)` — if the command needs them `pub`, promote them in the same commit).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rmux --test server_disconnect`
Expected: PASS

- [ ] **Step 5: Register the command and commit**

In `src-tauri/src/lib.rs`, add to the invoke handler (near `claude::claude_end_session` at `:196`):

```rust
server::server_disconnect,
```

```bash
git add src-tauri/src/server.rs src-tauri/tests/server_disconnect.rs src-tauri/src/lib.rs
git commit -m "feat: server_disconnect — drop a server's SSH connection, keep sessions running"
```

---

### Task 2: Rust — `server_sessions` command (list running sessions on a host)

**Files:**
- Modify: `src-tauri/src/server.rs` (add the command + a parser)
- Modify: `src-tauri/src/lib.rs` (register)
- Test: `src-tauri/tests/server_sessions.rs`

**Interfaces:**
- Produces:
  - `pub struct RunningSession { pub name: String, pub pid: Option<u32>, pub age_seconds: u64, pub attached: bool, pub command: Option<String> }` with `#[serde(rename_all = "camelCase")]`.
  - `pub async fn server_sessions<R: tauri::Runtime>(app: tauri::AppHandle<R>, store: State<'_, ClaudeStore>, target: TargetRef) -> Result<Vec<RunningSession>, String>`
  - `fn parse_list(out: &str) -> Vec<RunningSession>` — parses `agent list` stdout into `RunningSession`s.
- Consumes: `claude::resolve` (`src-tauri/src/claude.rs:36-51`), `crate::agent::ensure_agent` (`src-tauri/src/agent.rs:28-50`), `Installed.program` (the agent binary path), `rmux_transport::CommandSpec`.

- [ ] **Step 1: Write the failing parser test**

```rust
//! `server_sessions` runs `agent list` on the host and parses the rows.

#[test]
fn parse_list_rows() {
    // `agent list` prints tab-separated rows: name\tpid\tage\tattached|detached\tcommand.
    let out = "term-abc-1\t1234\t42\tdetached\tsh\n\
               claude-xyz-9\t5678\t9001\tattached\tclaude --resume cafebabe\n";
    let rows = crate::server::parse_list(out);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "term-abc-1");
    assert_eq!(rows[0].pid, Some(1234));
    assert_eq!(rows[0].age_seconds, 42);
    assert_eq!(rows[0].attached, false);
    assert_eq!(rows[1].name, "claude-xyz-9");
    assert_eq!(rows[1].attached, true);
    assert_eq!(rows[1].command.as_deref(), Some("claude --resume cafebabe"));
}

#[test]
fn parse_list_empty() {
    assert!(crate::server::parse_list("").is_empty());
    // A row whose name is absent (blank line / nothing to parse) is skipped.
    assert!(crate::server::parse_list("\t-\t0\tdetached\t\n").is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rmux --test server_sessions`
Expected: FAIL — `parse_list` not defined.

- [ ] **Step 3: Write minimal implementation**

Append to `src-tauri/src/server.rs`:

```rust
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningSession {
    pub name: String,
    pub pid: Option<u32>,
    pub age_seconds: u64,
    pub attached: bool,
    pub command: Option<String>,
}

/// Parse `agent list` output — one **tab-separated** line per session
/// (`name\tpid\tage\tattached|detached\tcommand`), as printed by
/// `crates/rmux-agent/src/main.rs:102-122`. Session names are ours and cannot
/// contain tabs or newlines, so splitting on `\t` is safe.
pub fn parse_list(out: &str) -> Vec<RunningSession> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let name = parts.next()?;
            let pid = parts
                .next()
                .and_then(|p| if p == "-" { None } else { p.parse().ok() });
            let age = parts.next().and_then(|a| a.parse().ok()).unwrap_or(0);
            let attached = parts.next().map(|a| a == "attached").unwrap_or(false);
            let command = parts.next().map(|c| c.to_string()).filter(|c| !c.is_empty());
            Some(RunningSession {
                name: name.to_string(),
                pid,
                age_seconds: age,
                attached,
                command,
            })
        })
        .collect()
}

/// List sessions the agent is running on `target` — including ones another PC
/// started. Mirrors `claude_end_session`: resolve the target, ensure the agent,
/// run `agent list` over the SSH connection, parse the rows.
#[tauri::command]
pub async fn server_sessions<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: State<'_, ClaudeStore>,
    target: TargetRef,
) -> Result<Vec<RunningSession>, String> {
    let resolved = crate::claude::resolve(store.inner(), &target).await?;
    let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;
    let spec = rmux_transport::CommandSpec::new(&installed.program)
        .arg("list")
        .tty(rmux_transport::Tty::None);
    let out = resolved.exec(&spec).await.map_err(|e| e.to_string())?;
    Ok(parse_list(&out))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rmux --test server_sessions`
Expected: PASS

- [ ] **Step 5: Register the command and commit**

In `src-tauri/src/lib.rs`:

```rust
server::server_sessions,
```

```bash
git add src-tauri/src/server.rs src-tauri/tests/server_sessions.rs src-tauri/src/lib.rs
git commit -m "feat: server_sessions — list sessions running on a host (agent list)"
```

---

### Task 3: Store — `detachSession` pure reducer + store action

**Files:**
- Modify: `ui/src/lib/workspace-reducers.ts` (add `detachSessionCore`)
- Modify: `ui/src/lib/workspace.ts` (add `detachSession` action)
- Test: `ui/workspace-reducers-check.ts`

**Interfaces:**
- Consumes: `closePane`/`assignPane` reducer (`workspace-reducers.ts:102-121`), `Core` type.
- Produces:
  - `export function detachSessionCore(ws: Core, id: string): Core` — clears every pane pointing at `id`, leaves sessions/activeSession unchanged.
  - Store action `detachSession: (id: string) => void` — clears live handle + panes, no kill.

- [ ] **Step 1: Write the failing reducer test**

In `ui/workspace-reducers-check.ts`, after the "Removing a server" block, add:

```ts
// ── Detaching a session: panes clear, the session stays, no kill ────────────
{
  let { ws } = seed();
  ws = assignPane(ws, 0, { kind: "session", id: "claude-1" });
  ws = assignPane(ws, 1, { kind: "session", id: "term-1" });
  ws = detachSessionCore(ws, "claude-1");

  check("detach clears the session's panes", ws.panes[0] === null);
  check("detach leaves other panes alone", ws.panes[1]?.kind === "session");
  check("the session stays (detach is not a kill)", ws.sessions.some((s) => s.id === "claude-1"));
  check("activeSession is untouched", ws.activeSession === "term-1");
  check("detaching an unknown id is a no-op", detachSessionCore(ws, "nope") === ws);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (from `ui/`): `pnpm exec vite build --ssr workspace-reducers-check.ts --outDir /tmp/rmux-check-out && node -e "import('/tmp/rmux-check-out/workspace-reducers-check.js').then(m=>m.run(console.log))"` (rename to `.mjs` first).
Expected: FAIL — `detachSessionCore` not defined.

- [ ] **Step 3: Write minimal implementation**

In `ui/src/lib/workspace-reducers.ts`:

```ts
/**
 * Detach a Session — the **structural** half of the store's `detachSession`.
 *
 * Unlike `removeSession`, this does **not** drop the session and never sends a
 * kill: detaching means "stop showing it, leave it running on the server". The
 * panes pointing at it are cleared (the tiles become empty), and the session
 * stays in `sessions` so the rail keeps a row to re-attach from.
 */
export function detachSessionCore(ws: Core, id: string): Core {
  if (!ws.sessions.some((s) => s.id === id)) return ws;
  const panes = ws.panes.map((p) => (p && p.kind === "session" && p.id === id ? null : p));
  return { ...ws, panes };
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: the same SSR command as Step 2. Expected: all checks PASS, including the new detach block.

- [ ] **Step 5: Store action + commit**

In `ui/src/lib/workspace.ts`, import `detachSessionCore` and add the action near `removeSession`:

```ts
detachSession: (id) => {
  const state = get();
  const session = state.sessions.find((s) => s.id === id);
  if (!session) return;
  // Detach = close the view, leave the process running under the agent. No kill.
  set((s) => {
    const ws = rDetachSessionCore(coreOf(s), id);
    const { [id]: _l, ...live } = s.live;
    return { ...ws, live };
  });
  schedulePersist(get);
},
```

```bash
git add ui/src/lib/workspace-reducers.ts ui/src/lib/workspace.ts ui/workspace-reducers-check.ts
git commit -m "feat: detachSession — close a session's view without killing it on the server"
```

---

### Task 4: SessionRow — ✕ detaches, menu has Detach / Close

**Files:**
- Modify: `ui/src/components/WorkspaceRail.tsx` (SessionRow, `:343-483`)

**Interfaces:**
- Consumes: store `detachSession(id)` (Task 3), existing `removeSession`/`renameSession`, `RailMenu`/`RailMenuItem` (already in the file, `:256-336` / `:210-243`), `confirming` state.

- [ ] **Step 1: Wire the ✕ to detach**

In `SessionRow`, the ✕ button (`:441-455`, `onClick` → `setConfirming(true)`) becomes:

```tsx
onClick={(e) => {
  e.stopPropagation();
  detachSession(session.id);
}}
```

and its `title` becomes "Detach — leave running on server". Add `const detachSession = useWorkspace((s) => s.detachSession);` to the store selectors.

- [ ] **Step 2: Add Detach to the session context menu**

The session's right-click `RailMenu` (`:460-472`) becomes:

```tsx
<RailMenuItem label="Rename" onClick={() => { setMenu(null); setEditing(true); }} />
<hr className="hairline my-1" />
<RailMenuItem
  label="Detach"
  onClick={() => {
    setMenu(null);
    detachSession(session.id);
  }}
/>
<RailMenuItem
  label="Close session"
  destructive
  onClick={() => setConfirming(true)}
/>
```

- [ ] **Step 3: Verify with typecheck + build**

Run: `pnpm exec tsc --noEmit` (clean), then `pnpm exec vite build` (succeeds).
Also run the reducer check from Task 3 Step 2 — all checks must still PASS.

- [ ] **Step 4: Commit**

```bash
git add ui/src/components/WorkspaceRail.tsx
git commit -m "feat: session ✕ detaches by default; kill moves to right-click Close session"
```

---

### Task 5: ServerNode — right-click menu with Disconnect / Remove / Attach to running session

**Files:**
- Modify: `ui/src/components/WorkspaceRail.tsx` (ServerNode, `:691-792`)
- Modify: `ui/src/lib/api.ts` (add `serverDisconnect` + `serverSessions` typed wrappers)
- Modify: `ui/src/lib/workspace.ts` (add `adoptServerSession` store action)

**Interfaces:**
- Consumes: `api.serverDisconnect(target)` and `api.serverSessions(target)` (from `ui/src/lib/api.ts`, Task 2's Rust side), `RailMenu`/`RailMenuItem`.
- Produces: `adoptServerSession(serverId: string, name: string, kind: "terminal" | "claude")` store action.

- [ ] **Step 1: Add the typed wrappers to api.ts**

```ts
/** Disconnect a server: close its SSH connection, keep sessions running. */
serverDisconnect: (target: TargetRef) => call<void>("server_disconnect", { target }),
/** Sessions the agent is running on a host — including ones another PC started. */
serverSessions: (target: TargetRef) =>
  call<RunningSession[]>("server_sessions", { target }),
```

Add the `RunningSession` type near `TargetRef`:

```ts
/** A session the agent is running on a host, as `agent list` reports it. */
export type RunningSession = {
  name: string;
  pid: number | null;
  ageSeconds: number;
  attached: boolean;
  command: string | null;
};
```

- [ ] **Step 2: Add the store action `adoptServerSession`**

In `ui/src/lib/workspace.ts`, add near `addSession`:

```ts
/**
 * Adopt a session already running on a host (found via `agent list`) into the
 * rail. The id is derived from the host name so re-attaching hits the running
 * process rather than spawning a duplicate — the daemon is multi-attacher.
 */
adoptServerSession: (serverId, name, kind) => {
  // A claude-<id> name has its id *inside* the name; keep a local id that maps
  // to the host name via reattachName. Reuse the existing session-mint shape but
  // pin the host name: store the discovered name on the session so the attach
  // uses it verbatim.
  const id = name.startsWith("claude-") ? `adopted-${name}` : name;
  const s = get();
  const existing = s.sessions.find((x) => x.id === id);
  if (existing) {
    get().openSession(id);
    return;
  }
  const projectId = // the server's default project, or a "running" bucket:
    //   serverId + "\0running"
  set(addSession(coreOf(get()), {
    id,
    projectId,
    kind,
    name: name.replace(/^claude-/, ""),
    renamed: true, // never let adoptClaudeTitle overwrite the discovered name
  }));
  get().openSession(id);
},
```

**Note on identity:** the existing `mintSessionId` is per-PC and non-deterministic. For adopted sessions we deliberately use the **host name** as the local id so the same host session on two PCs collapses to the same row. Confirm `reattachName` round-trips: for a terminal, `reattachName` returns the id verbatim (the name `term-…`); for a claude `adopted-claude-…`, `reattachName` returns `claude-adopted-claude-…` — **wrong**. If so, store the discovered host name on the session (e.g. `session.hostName = name`) and have the attach path use `hostName ?? reattachName(session)`. See Task 6.

- [ ] **Step 3: Wire the server right-click menu**

In `ServerNode`, add `const [menu, setMenu] = useState<RailMenuTarget | null>(null)` and `onContextMenu` on the server card (`:711-717`). Render:

```tsx
{menu && (
  <RailMenu target={menu} onClose={() => setMenu(null)}>
    <div className="flex flex-col">
      <RailMenuItem label="Disconnect" onClick={() => { setMenu(null); api.serverDisconnect(server.target); }} />
      <RailMenuItem label="Attach to running session…" onClick={() => { setMenu(null); setAttaching(true); }} />
      <hr className="hairline my-1" />
      <RailMenuItem label="Remove server" destructive onClick={() => setConfirming(true)} />
    </div>
  </RailMenu>
)}
```

Add a `confirming` state block mirroring the project/session confirm (names the server, "its sessions will be ended on the server", `btn btn-primary` Remove → `removeServer(server.id)`), and an `attaching` state that opens the picker (Task 6).

- [ ] **Step 4: Typecheck + build + commit**

Run `pnpm exec tsc --noEmit` and `pnpm exec vite build`. Commit:

```bash
git add ui/src/components/WorkspaceRail.tsx ui/src/lib/api.ts ui/src/lib/workspace.ts
git commit -m "feat: server right-click menu — Disconnect, Remove, Attach to running session"
```

---

### Task 6: Attach-to-running picker UI

**Files:**
- Modify: `ui/src/components/WorkspaceRail.tsx` (ServerNode — the picker panel)
- Modify (if needed): `ui/src/lib/workspace-model.ts` (add optional `hostName` to `SessionV3`)

**Interfaces:**
- Consumes: `api.serverSessions(target)` (Task 2/5), `adoptServerSession` (Task 5), `RunningSession`.
- Produces: the picker panel + adoption flow.

- [ ] **Step 1: Build the picker panel**

When `attaching` is true, show a panel (a `RailMenu` styled as a list) under the server row:

```tsx
{attaching && (
  <div className="mx-1.5 mb-1 rounded-none border p-2" style={{ borderColor: "var(--border)" }}>
    <div className="flex items-center justify-between px-1 pb-1">
      <span className="micro">RUNNING SESSIONS ON HOST</span>
      <button type="button" className="chip" onClick={() => setAttaching(false)}>close</button>
    </div>
    {sessions.length === 0 && !loading ? (
      <p className="micro px-1" style={{ color: "var(--text-faint)" }}>none running</p>
    ) : (
      <ul className="flex flex-col gap-px">
        {sessions.map((s) => (
          <li key={s.name}>
            <button
              type="button"
              className="data w-full px-1 py-[4px] text-left text-[11px]"
              onClick={() => { adoptServerSession(server.id, s.name, s.name.startsWith("claude-") ? "claude" : "terminal"); setAttaching(false); }}
            >
              <span className="truncate">{s.name}</span>
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                {s.attached ? "· attached" : "· idle"} · {s.ageSeconds}s
              </span>
            </button>
          </li>
        ))}
      </ul>
    )}
  </div>
)}
```

Load on open (in `ServerNode`, when `attaching` flips true): `api.serverSessions(server.target).then(setSessions)`, with a `loading` flag and `.catch(() => setSessions([]))`.

- [ ] **Step 2: Fix the identity round-trip**

Verify `reattachName` round-trips for an adopted name. If a claude `adopted-claude-…` doesn't reattach to `claude-<id>`, add an optional `hostName?: string` to `SessionV3`, set it in `adoptServerSession`, and have the attach call sites use `session.hostName ?? reattachName(session)`:
- `WorkspaceDeck.tsx:45` (`session={reattachName(session)}`)
- `ClaudePanel.tsx:349` (`sessionName: \`claude-${sessionId}\``)
- `workspace.ts` `removeSession` (`:313`), and any `claudeEndSession`/`terminal_close` call that kills by name.

- [ ] **Step 3: Typecheck + build + commit**

Run `pnpm exec tsc --noEmit` and `pnpm exec vite build`. Commit:

```bash
git add ui/src/components/WorkspaceRail.tsx ui/src/lib/workspace-model.ts ui/src/lib/workspace.ts
git commit -m "feat: picker to attach to a session already running on the host"
```

---

### Task 7: End-to-end verification

**Files:** none (verification only)

- [ ] **Step 1: Rust checks**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
```
Expected: clean.

- [ ] **Step 2: UI checks**

```bash
pnpm exec tsc --noEmit
pnpm exec vite build
# reducer check (rename .js → .mjs first)
```
Expected: all clean; all 35+ reducer checks PASS.

- [ ] **Step 3: Manual smoke (dev)**

Run `pnpm tauri dev`. Against a real host:
- Open a terminal in a project. Click the row's ✕ → row stays dimmed, pane clears. Run `rmux-agent list` on the host (or check via `ps`) → the shell is **still running**.
- Right-click the session row → **Close session** → confirm → the process ends.
- Right-click the server → **Disconnect** → no crash; the ControlMaster socket is gone (check `~/.rmux` / `ssh -O exit` behaviour); a subsequent terminal on that host reconnects.
- Start a session from one PC. On another PC (or a second app instance pointed at the same host), right-click the server → **Attach to running session…** → the session appears; click it → attaches to the running process, scrollback replays.
- Right-click the server → **Remove server** → confirm → all its sessions end on the host and the row drops.

- [ ] **Step 4: Final commit (if verification changed anything) + report**

Report what was verified and what remains (e.g. cross-PC needs two real machines; that is covered by the same-host adoption test).
