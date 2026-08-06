# ADR-001 — A three-level hierarchy: Server → Project → Session (kind)

Status: **Accepted** (grilled 2026-08-05)
Supersedes: the flat `Session[]` model in `ui/src/lib/sessions.ts` (`rmux.workspace.v2`).

## Context

Today rmux's UI is a **flat list of `Session`s** in one Zustand store
(`rmux.workspace.v2`). A `Session` fuses three things that are really separate:

- a **target** (`{host?, user?, port?}`) — which machine,
- a **folder** — the project root,
- exactly **one Claude conversation** (the default full-area view, `claude-<id>`),
  plus a `terminal` tab holding *many* `TerminalTab` shells.

"Project" and "Host" are not entities. A project is just the `folder` string; a
host is just `session.target.host`. The rail *derives* `(host, folder)` groups at
render time (`session-groups.ts`). So the app already **displays** a hierarchy it
does not **model**.

The backend, by contrast, already models it cleanly:

- `TargetId` (`Local | Ssh(SshHostId{alias,user,port})`) → **one `ControlMaster`
  connection per host**; metrics, the process list, and `-L`/`-D` forwards are all
  keyed **per-host** already.
- The agent daemon holds **many named PTYs per host**, and treats the name
  (`term-…` / `claude-…`) as **opaque** — shell-vs-Claude is only *inferred* from
  the recorded command. Terminal and Claude sessions are already peers on the wire.

The operator asked for: a **Server root** grouping all projects on the same
server; **host / processes / ports factored up to the Server layer**; and
**terminals treated as peers of Claude sessions** — the project's `+` adds a
*terminal*, a dedicated Claude action adds a *Claude session*.

The redesign is therefore largely **making the UI honest about the model the
backend already has**, which is why it fights the architecture very little.

## Decisions

1. **Three real entities replace the fused `Session`:** **Server → Project →
   Session(kind)**. See `glossary.md` for the canonical definitions.

2. **A Server *is* a connection identity** — `TargetId` promoted into the UI, with
   the **local machine as a Server**. Identity is the **ssh alias, never the
   resolved hostname** (CLAUDE.md invariant): the same machine under two aliases is
   two Servers. A Server owns the connection, host **metrics**, the **process
   list**, and the `-L`/`-D` **forwards** — all already per-host in the backend.

3. **A Project *is* an absolute folder on a Server.** Identity = the path; label =
   basename (display-only rename). The **folder moves off the session onto the
   Project**, so the project's file tree + editor + search become **per-project**
   (they were per-session, keyed by `sessionId`). Membership is by the folder a
   session was *created under*, not its live cwd.

4. **A Session has a `kind`: `terminal | claude`, and the two are full peers.** One
   flat session list per Project; **any count of either, zero Claude allowed**;
   neither is "the default". Shared fields: `id, name, status, error`. Claude-only
   config (`resume, fullscreen, skipPermissions, modelProfile, claudeAccount,
   contextWindow, jiraProject`) hangs off the claude variant. This mirrors the
   agent's opaque-name model exactly.

5. **The attention system stays Claude-only in v1.** `working/waiting/finished`
   and the header "needs you" count are Claude-shaped (elapsed-timer + permission
   prompts). A **terminal is status-neutral** — kind icon only, no status, not
   counted. A terminal **busy ring** (agent reports foreground-pgrp running) is a
   named fast-follow, deferred because it is a new agent capability.

6. **Migrate v2 → v3 in place, preserving ids.** On first v3 load, translate: each
   distinct `target` → a Server; each `(target, folder)` → a Project; each old
   fused Session → a **Claude session** under its Project **keeping its id** (so
   `claude-<id>` still reattaches to the running conversation); each old
   `TerminalTab` → a **Terminal session** under that Project **keeping `term-<id>`**;
   `gridSlots` (cell→sessionId) → cell→`{kind:'session', ref:id}` (see ADR-002).
   Dropping state like the v1→v2 bump did is **rejected**: the reattach names are
   derived from ids, so a drop would orphan every running Claude and shell — the
   exact "I came back and everything was interrupted" failure CLAUDE.md exists to
   prevent.

## The v3 shape (sketch, not final types)

```
Server   { id, target: {host?,user?,port?} }          // local => target {} 
Project  { id, serverId, folder, label?, renamed? }
Session  { id, projectId, kind, name, renamed?, status, error,
           // claude-only:
           resume?, fullscreen?, skipPermissions?, modelProfile?,
           claudeAccount?, contextWindow?, jiraProject? }
```

`serverId` / `projectId` are derivable keys (target-hash / path-hash) so migration
and dedup are pure functions, not id-minting that could double-create.

## Invariants this must not break

- **The one architectural rule.** Nothing here adds a server hop to the session
  path; Server is a *UI* promotion of the existing `TargetId`. Sessions remain
  direct-SSH named PTYs.
- **One code path for local & remote.** Server(local) is not a special case in
  feature code — it is `TargetId::Local` as today. No `if is_local` leaks in.
- **Reattach names are load-bearing.** `claude-<id>` / `term-<id>` must be
  preserved verbatim by migration and by all create/kill paths, or live work is
  orphaned. Closing a *session* still sends `kill`; closing a *pane* does not
  (ADR-002).
- **Grouping identity == (alias, folder).** The same `(host, folder)` that
  `groupSessions` uses today becomes `(Server, Project)`; the NUL-joined group key
  logic is the migration's join key.

## Consequences

- The Zustand store restructures from `sessions: Session[]` to
  `servers / projects / sessions` (+ pane layout, ADR-002). `session-groups.ts`
  (derived grouping) is largely **deleted** — the grouping becomes stored structure.
- Per-project file state: `buffers`/`openOrder`/`activeBuffer` re-key from
  `sessionId` to `projectId`.
- No new Rust/IPC is *required* for v1 (host/ports/metrics commands already take a
  `TargetRef`; terminal/claude create already take a `session` name). The change is
  overwhelmingly in the UI layer — which is where the model was wrong.

## Risks / open follow-ups

- **Terminal busy ring** (decision 5 fast-follow) needs an agent capability:
  report per-PTY whether `tcgetpgrp(master) != shell pid`. New `SessionSummary`
  field + status-watch plumbing.
- **`jiraProject` placement.** Parked on the claude variant (it gates that
  session's Jira sub-tab). If the operator later wants Jira per-*Project*, revisit.
- **Empty Projects** persist as nodes with no sessions; the rail must render them
  (and offer delete) or `[+ project]` followed by restart looks like data loss.
