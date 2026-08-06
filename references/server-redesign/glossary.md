# Server redesign — glossary (ubiquitous language)

The canonical vocabulary for rmux's hierarchy redesign: **Server → Project →
Session (kind)**, replacing the flat `Session[]` model. One term per entry;
implementation lives in the ADRs and plan, not here.

Status legend: ✅ resolved · 🔶 proposed/under grilling · ❓ open

---

## Server ✅

A **Server** is a single SSH connection identity — the `(alias, user, port)` the
backend already models as `TargetId` (`Local | Ssh(SshHostId)`). The **local
machine is a Server too**. Two projects are "on the same server" iff they share
this identity.

A Server *owns* everything the backend already keys per-host:

- the one `ControlMaster` connection (opening more sessions on it costs no handshake),
- host **metrics** (CPU/memory — was the per-session HOST tab),
- the discovered **process list**,
- the `-L` port forwards and the `-D` SOCKS proxy.

A Server is **not** a user-named group of multiple hosts, and it is **not**
identified by a port or a process — ports and processes are *contents* of a
Server, not its identity (a host has many of each).

Identity is the **ssh alias, never the resolved hostname** (CLAUDE.md invariant:
we never resolve `~/.ssh/config`). So the *same machine* reached under two
aliases — or under an alias vs. a bare IP — is **two Servers**. That is correct:
each may authenticate differently.

Not the auth/Cowork server — that is unrelated. Here "Server" always means a
connection target.

---

## Project ✅

A **Project** is an absolute **folder on a Server**. Its identity *is* that path;
two sessions in the same folder on the same Server are the same Project. The
folder lives here now — pulled *off* the individual session — so every session
in a Project shares one root.

- **Label** = folder basename, renameable for display only (identity stays the path).
- A Project **owns** the file tree + editor and project search. These were
  per-session (buffers keyed by `sessionId`); they become **per-project** (keyed
  by project) — one file tree shared by all the project's sessions.
- A Project **contains** a flat set of Sessions of either kind (see below).

Matches today's derived `(host, folder)` grouping exactly, so migration is
mechanical.

**Membership is by the folder a session was created under, not its live cwd.** A
terminal that `cd`s elsewhere, or a Claude that roams, still belongs to its
Project. Resuming an old Claude conversation whose recorded folder is a
*different* path lands it in (creates, if absent) the Project for **that** path.
An empty Project (made via `[+ project]` with nothing in it yet) is a real,
persisted node — it survives restart.

## Session ✅

A **Session** is a named PTY held by the agent, living under a Project. It has a
**kind**: `terminal` or `claude`. The two kinds are **full peers** — a Project
holds one flat list, any count of either, **zero Claude allowed** (a
terminals-only project is normal). Neither kind is "the default"; opening a
Project shows whichever Session you select.

- **Shared** across kinds: `id`, `name`, `status`, `error`.
- **Terminal session**: carries essentially a title. Reattach name `term-…`.
- **Claude session**: carries the Claude config — `resume`, `fullscreen`,
  `skipPermissions`, `modelProfile`, `claudeAccount`, `contextWindow`,
  `jiraProject`. Reattach name `claude-…`.

This matches the agent, which already treats the reattach name as opaque and
only *infers* shell-vs-Claude from the recorded command.

Icons (from the user): **`◧`/`+` for terminal** (the `+` action adds a terminal),
the **lobehub Claude mark for claude** (a dedicated action adds a Claude session).

## Session status ✅ (v1 scope)

The **attention system** (`working` turning-ring, `waiting`, `finished` accent
mark, and the header "needs you" count) is **Claude-only in v1**. Its detection
is inherently Claude-shaped — the elapsed-timer pattern and permission prompts.

A **terminal session is status-neutral**: it shows only its kind icon, never a
status, and never adds to "needs you." No agent changes required to ship the
hierarchy.

**Fast-follow (not v1):** a terminal **busy ring** — the agent reports whether a
foreground command is running (fg process group ≠ the shell's pid); no `waiting`,
no `finished`, not counted. Deferred because it is a new agent capability.

## Pane ✅

A **Pane** is one tile in the grid — a "window" in the workbench. The grid is a
general **pane manager** (1 / 2 / 4 / … tiles), and a Pane holds a **content
source**, not necessarily a session. Pane kinds:

- **Session pane** — a Claude TUI or a shell (attached to a Session).
- **Host pane** — a Server's metrics / process list / port forwards (was the
  per-session HOST tab).
- **Files pane** — a Project's file tree + editor (was the per-session FILES tab).

Every rail node has an **"open as a pane"** affordance. Opening a node puts its
content into the grid — the whole stage in single mode, or **replacing one tile**
in grid mode (this is what the user means by "show the content in a window").
A Pane is a *view slot*; closing a Pane does **not** kill the Session behind it —
the Session persists in the tree and can be reopened into a Pane.

Uniform rule: **one interaction to learn** — "open this node into a pane" — for
sessions, host, and files alike.

## Claude sub-views ✅

**Transcript** and **Jira** are **not** their own panes. They are a small
**sub-tab strip inside a Claude session's pane** (`Claude | Transcript | Jira`),
because a transcript with no conversation beside it is rarely useful — they ride
next to the conversation they belong to. Jira's tab appears only when the Claude
session carries a `jiraProject`.

**Settings** stays app-global (its own window, unchanged).
