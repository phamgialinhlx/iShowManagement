# ADR-002 — The workbench is a pane manager; a session is one kind of pane

Status: **Accepted** (grilled 2026-08-05)
Depends on: ADR-001.

## Context

ADR-001 shatters the fused `Session`. That leaves a spatial question: today one
Session fills the stage with a tab strip —
`claude | transcript | files | terminal | host | jira | settings`. Once `claude`
and `terminal` become *sessions in the tree* and `files`/`host` belong to
*Project*/*Server*, where do the non-session surfaces render?

The operator's answer, in their words: *"a button below the server, it will show
the content in a window … it is the stage — in grid mode, replace one window in
the grid."* "Window" means a **grid tile**, not a native OS window and not a
floating panel.

## Decisions

1. **The grid is a general pane manager.** It tiles (1 / 2 / 4 / …) **Panes**, and
   a Pane holds a **content source** — a session is just one kind. Pane kinds:
   - **Session pane** — a Claude TUI or a shell.
   - **Host pane** — a Server's metrics / process list / port forwards.
   - **Files pane** — a Project's file tree + editor.

2. **Every rail node has one interaction: "open into a pane."** Opening puts the
   content into the grid — the whole stage in single mode, or **replacing the
   focused tile** in grid mode. One gesture to learn, for sessions / host / files
   alike.
   - Click a **session** → open/focus its pane.
   - Click a **project label** → open its **files** pane. (The chevron ▾ just
     expands/collapses; it is *not* the open gesture.)
   - Click a server's **`[host]`** button → open its **host** pane.

3. **Transcript + Jira are not panes.** They are a sub-tab strip **inside a Claude
   session's pane** (`Claude | Transcript | Jira`), because a transcript with no
   conversation beside it is rarely wanted. Jira's tab shows only when the claude
   session carries a `jiraProject`. **Settings** stays app-global (its own window).

4. **A Pane is a view slot, not an entity.** `gridSlots` changes from
   `(string | null)[]` (cell→sessionId) to `(PaneRef | null)[]` where
   `PaneRef = {kind:'session', id} | {kind:'host', serverId} | {kind:'files', projectId}`.
   **Closing a pane removes the tile only.** Killing a session (its shell/Claude) is
   a **separate explicit action on the session row** — tidying your layout must
   never leak a shell.

5. **Pane layout persists.** The v3 store keeps the `PaneRef[]` grid. On load,
   session panes reattach (agent), host/files panes re-derive from their
   Server/Project (both persisted). A layout that evaporated on restart would read
   as broken.

## The rail (reference layout)

```
 SERVERS                               [+ connect]
 ─────────────────────────────────────────────────
 ▾ ◆ prod   alice@prod:22              [ host ]
   ▾ api    /home/me/api               [+] [✦]
       ◧ term-1
       ◧ build
       ✦ claude-abc          ● working
   ▾ web    /home/me/web               [+] [✦]
       ✦ claude-def
     [+ project]
 ▾ ◆ local  this machine               [ host ]
     [+ project]
```

- `[+ connect]` — new **Server** (reuses today's host-picker: ssh-config aliases +
  manual host/user/port). Local always present.
- `[+ project]` — new **Project** on that server (reuses today's folder picker).
  The only way a Project with no sessions is created.
- `[+]` — new **Terminal** session · `[✦]` (lobehub Claude mark) — new **Claude**
  session, both on the Project row.

## Invariants this must not break

- **A widget switched off must not run.** Host/files/session panes are *mounted*
  only while tiled; closing a pane unmounts it (its pollers stop), matching the
  instruments rule. The shared host poller stays gated.
- **Terminal bytes never travel our RPC.** A session pane is still an xterm over a
  local PTY (`ssh -tt` wrapped by `Target::build_command`); the pane manager is
  pure UI placement.
- **Every xterm host loads `WebglAddon`** and re-fits on the appearance channel —
  a session pane is not exempt.
- **⌘F focus rule, click-to-refocus, resize-debounce** — all carry over per pane
  unchanged; the pane is the same xterm host as today, just relocated.

## Consequences

- `SessionView.tsx`'s `View` union and per-session tab strip are **removed**;
  their surfaces become pane kinds (files/host) or claude sub-tabs
  (transcript/jira). `HostPanel` and `FilesView` are re-parented from
  `session`-prop to `server`/`project`-prop.
- The focused-cell + `assignSlot` machinery generalises from sessionId to
  `PaneRef` — the interaction the prior session already reshaped (New-session
  moved to the rail top; "PICK A SESSION FOR CELL" hint removed) fits this cleanly:
  clicking any node fills the focused tile.

## Risks / open follow-ups

- **Same content in two panes.** Allowed by the model (two tiles → same session).
  For a *session* pane this means two xterm views of one PTY — xterm can mirror,
  but decide whether we permit it or focus-existing. Recommend focus-existing in v1.
- **Pane vs session close discoverability.** Two nearby verbs ("close pane" /
  "kill session"). Must be visually distinct per CLAUDE.md's "make the wrong click
  hard" rule — kill is the deliberate second choice.
