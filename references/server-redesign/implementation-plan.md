# Implementation plan — Server → Project → Session hierarchy + pane manager

Derived from [ADR-001](./ADR-001-server-project-session-hierarchy.md) and
[ADR-002](./ADR-002-pane-manager.md). **UI-heavy, backend-light**: the model the
backend already has (per-host connection, opaque named PTYs, per-host
metrics/ports) is being surfaced — so v1 needs **no new Rust/IPC**. Ordered so the
app renders and reattaches at every step, never shipping a broken intermediate.

## Phase 0 — Data model + migration (`ui/src/lib/sessions.ts`)

The foundation. Nothing visual yet; the app must still render exactly as before at
the end of this phase (rail keeps deriving groups until Phase 1 replaces it).

> **Progress (pure layer DONE + tested):**
> - `ui/src/lib/workspace-model.ts` — v3 types, `serverId`/`projectId`,
>   `reattachName`, `migrateV2toV3`, `resolveWorkspace` (v3-or-migrate-v2),
>   `EMPTY_V3`, `makeServer`/`makeProject`. Pure (only `import type`).
> - `ui/src/lib/workspace-reducers.ts` — `addServer/addProject/addSession/
>   removeSession/assignPane/closePane`, immutable, `EMPTY_CORE`.
> - `ui/src/lib/workspace.ts` — the Zustand store (thin IO over the reducers):
>   localStorage load/persist via `resolveWorkspace` (writes `rmux.workspace.v3`,
>   leaves the v2 blob as a downgrade fallback), agent kills on remove
>   (`reattachName` + resolved target), selectors (`targetOf`/`projectOf`/…),
>   entity + pane + status + config actions, runtime handle map. `tsc` clean.
>   **Nothing imports it yet**, so the running app is untouched.
> - Tests: `ui/workspace-migration-check.{ts,html}` (28), `ui/workspace-reducers-check.{ts,html}`
>   (17). All 45 green in Node (`--experimental-strip-types`) + browser harness; `tsc` clean.
>
> **Remaining:** file/editor **buffers re-keyed to project** land in Phase 2 with
> the files pane (not needed to render the rail). Phase 1 moves consumers onto
> `useWorkspace`. Live-host reattach is proven at the end of Phase 1 (needs the
> store wired + a build).

- Introduce the v3 types beside the old ones: `Server`, `Project`, and a
  kind-tagged `Session` (see ADR-001 sketch). `serverId` = hash of the target;
  `projectId` = hash of `(serverId, folder)` — **derivable**, so dedup is a pure
  function, not id-minting.
- Restructure store state: `servers: Server[]`, `projects: Project[]`,
  `sessions: Session[]` (flat, each with `projectId` + `kind`). Re-key
  `buffers`/`openOrder`/`activeBuffer` from `sessionId` → `projectId` (files are
  per-project now).
- Bump `STORAGE_KEY` `rmux.workspace.v2` → **`v3`** and write `migrateV2toV3()`:
  - each distinct `target` → a `Server` (dedup by `serverId`),
  - each `(target, folder)` → a `Project` (dedup by `projectId`),
  - each old fused `Session` → a **claude** `Session`, **id preserved** (carries
    resume/fullscreen/skipPermissions/modelProfile/claudeAccount/contextWindow/
    jiraProject),
  - each old `TerminalTab` → a **terminal** `Session` under the same Project, **id
    preserved** (`term-<id>`),
  - old `gridSlots` (cell→sessionId) → `paneSlots` cell→`{kind:'session', id}`.
  - `openPaths`/`activePath` re-key session→project.
  - **Reattach names must survive verbatim** — assert in a unit test that a
    migrated claude session's reattach name equals `claude-<oldId>`.
- Keep `load()` tolerant: unknown/absent → empty v3 (fresh install), v2 present →
  migrate once, v3 present → load. A **test** pins a realistic v2 blob → v3.

> **Phase 1 build order (parallel: new components against `useWorkspace`, old app
> untouched, `tsc` green each step, swap `Workbench` last):**
> 1. `WorkspaceRail.tsx` — the tree rail (server→project→session), create/open/kill. ✅ DONE (tsc-clean; not yet wired). TODO later: collapsed-rail status dots; exact lobehub Claude path.
> 2. Pane manager + `SessionPane`/`HostPane`/`FilesPane`. ✅ DONE (`WorkspaceDeck.tsx`, tsc-clean; not
>    yet wired). `HostPanel` refactored to take `target` (both callers updated) — shrinks cutover.
>    TODO: FilesPane is a placeholder (real tree+editor = Phase 2); focus-mode warm-set optimisation;
>    Claude transcript/Jira sub-tabs. `SessionPane`'s claude branch renders `ClaudePanel` as-is; its
>    internal store swap happens in the cutover (step 6).
> 3. Leaf-component findings (from reading them):
>    - `TerminalView` — **already prop-driven, reuse as-is** (`target/cwd/session/ptyId/onOpened/onExit`).
>    - `HostPanel` — **prop-driven** (takes `session`/`target`); feed it `serverOf(id).target`.
>    - `ClaudePanel` — launch config already props; only internal `useSessions` use is a
>      mechanical swap to `useWorkspace` (same names: `setStatus`/`adoptClaudeTitle`/
>      `configureSession`; `setClaudeSession`→`setLive`, `clearClaudeSession`→`clearLive`;
>      `claudeSessions[id]`→`live[id]`; `sessions.find(..).contextWindow` read). It also hard-codes
>      `sessionName: claude-${sessionId}` — that already equals `reattachName`, so it stays correct.
> 4. Connect-server / new-project flows. ✅ DONE (`WorkspaceNewSession.tsx`, two modes, tsc-clean;
>    not yet wired). Session creation stays on the rail; resume/skip/profile-at-create is a follow-up.
> 5+6. **CUTOVER — must be ATOMIC (one fire), end `tsc`-green** (deleting `sessions.ts` cascades):
>    - **Delete:** `SessionView.tsx`, `SessionRail.tsx`, `NewSession.tsx`, `session-groups.ts`, `sessions.ts`.
>    - **Move out of `sessions.ts` first** → new `lib/buffers.ts`: `Buffer`, `BufferKey`, `bufferKey`,
>      `isDirty` (CodeEditor + Workbench-dirty import from here; buffer *store* is Phase 2).
>    - **Flip to `useWorkspace` (store rewrite):**
>      - `ClaudePanel` — `setClaudeSession`→`setLive`, `clearClaudeSession`→`clearLive`,
>        `claudeSessions[id]`→`live[id]`; `setStatus`/`adoptClaudeTitle`/`configureSession` same names;
>        `sessions.find(..).contextWindow` read via `useWorkspace`.
>      - `lib/status-watch.ts` — poll claude-kind sessions, publish via `setStatus`.
>      - `lib/notify.ts` — subscribe to `runtime` status transitions.
>      - `lib/control.ts` — mirror `sessions` (map to `{id,name,host,folder}` via `serverOf`/`projectOf`).
>      - `WidgetRail` — take `target` (host sample) instead of `Session`.
>      - `SessionSettings` — `useWorkspace` config actions; or defer behind the session row.
>      - `screens/Workbench.tsx` — render `WorkspaceRail`+`WorkspaceDeck`+`WorkspaceNewSessionLayer`;
>        grid controls + footer from `useWorkspace`; `dirtyCount`=0 until Phase 2.
>    - **Type-only decouple (deferred components, keep compiling):** `JiraPanel`, `TranscriptView`
>      import `type Session` — switch to explicit props / `SessionV3` (they're not in the new deck yet).
> 5+6. **CUTOVER ✅ DONE.** Created `lib/buffers.ts`; flipped `ClaudePanel` + `status-watch`/`notify`/
>    `control` + `WidgetRail`(→`Active` ctx)/`SessionSettings` to `useWorkspace`; decoupled
>    `CodeEditor`/`JiraPanel`/`TranscriptView` (props/`buffers`); rewrote `Workbench` to
>    `WorkspaceRail`+`WorkspaceDeck`+`WorkspaceNewSessionLayer`; deleted `SessionView`/`SessionRail`/
>    `NewSession`/`session-groups`/`sessions.ts` + `groups-check`; repointed `rail-selection-check`.
>    **Verified: `tsc` exit 0, `vite build` exit 0, 45/45 logic tests pass.**
> 7. `pnpm tauri build` ✅ built+signed+installed once (Aug 6 00:37). **Runtime still UNVERIFIED** —
>    compiles+bundles ≠ works; needs the app opened + a live-host reattach.
> **Phase 2 (parity) — files ✅ DONE:** per-project buffer store in `useWorkspace`
>    (`openFile`/`closeBuffer`/`activateBuffer`/`edit`/`save`/`restoreFiles`, keyed by projectId,
>    target via project→server; persist derives openPaths/activePath from live buffers, preserves
>    untouched projects); new `FilesPane.tsx` (recovered tree/search/resizer/tabs/preview from git,
>    keyed by projectId) wired into `WorkspaceDeck`. `tsc`/`vite`/45 tests green.
> **Parity ✅ DONE:** Claude **transcript/Jira sub-tabs** — new `ClaudeSessionPane.tsx` keeps the
>    Claude TUI mounted (display-toggled) with `TranscriptView`/`JiraPanel` mounted on demand;
>    wired as the claude branch of `SessionPane`. `tsc`/`vite`/45 tests green. Final `pnpm tauri build`
>    running (bomq5fd6x) → then sign+install.
> **Lower-priority follow-ups (not blocking parity):** focus-mode warm set, resume/skip/profile at
>    Claude-create (rail `✦` currently starts fresh; launch screen handles skip), collapsed-rail status
>    dots, exact lobehub Claude path. **Runtime still needs the app opened + a live-host reattach test.**

## Phase 1 — The rail becomes a stored tree (`SessionRail.tsx`, new `session-tree.ts`)

- Replace the derived `groupSessions()` render with a **Server → Project →
  Session** tree read from the store. `session-groups.ts` is retired (its
  `(host, folder)` join key logic moves into `projectId` hashing in Phase 0).
- Render nodes per ADR-002's reference layout: server rows with `[host]`,
  collapsible; project rows with `[+]`(terminal) / `[✦]`(Claude) and `[+ project]`;
  session rows with kind icon (`◧` terminal, lobehub `✦` claude) and — for claude
  only — the existing status dot/ring.
- Vendor the **lobehub Claude icon** as an inline SVG (design rule 3: no emoji,
  inline SVG, Lucide-style). Source: https://lobehub.com/icons/claude — trace to a
  single-path SVG, strokes/caps per SIGNAL ROOM.
- Wire create actions to reused flows:
  - `[+ connect]` → `NewSession`'s host step (ssh-config aliases + manual) → creates
    a `Server` (no session yet).
  - `[+ project]` → `NewSession`'s folder step scoped to that server → creates a
    `Project` (no session yet). **Empty projects/servers are valid, persisted nodes.**
  - `[+]` → `addSession(projectId, kind:'terminal')` (name `term-<newId>`).
  - `[✦]` → `addSession(projectId, kind:'claude', …)` (name `claude-<newId>`), with
    the resume / skip-permissions / model-profile choices from `NewSession`'s claude
    step.
- **Kill vs close** stays honest: the session row's kill action sends
  `terminal_close`/`claude_end_session`; it is the deliberate second choice, visually
  distinct from closing a pane.

## Phase 2 — Pane manager (`SessionView.tsx`)

- `gridSlots: (string|null)[]` → `paneSlots: (PaneRef|null)[]`,
  `PaneRef = {kind:'session',id} | {kind:'host',serverId} | {kind:'files',projectId}`.
- Generalise focused-cell placement: clicking any rail node fills the **focused
  tile** with the matching `PaneRef` (whole stage in single mode). Reuses the
  `focusedCell` + `assignSlot` machinery the prior session already reshaped.
- Render a tile by `PaneRef.kind`:
  - `session` → existing xterm host (Claude pane *or* Terminal pane by the session's
    kind), **unchanged** (WebGL, refit-on-appearance, ⌘F focus, resize-debounce,
    click-to-refocus all carry over).
  - `host` → `HostPanel` re-parented from `session` prop to `serverId`.
  - `files` → `FilesView` re-parented from `session` prop to `projectId`.
- **Close pane** = clear the slot (unmount → pollers stop, per the instruments
  rule). Does **not** kill the session. In v1, opening an already-tiled session
  **focuses the existing tile** rather than duplicating (ADR-002 risk).

## Phase 3 — Retire the per-session tab strip (`SessionView.tsx`, `ClaudePanel.tsx`)

- Delete the `View` union and the `claude|transcript|files|terminal|host|jira|settings`
  tab strip. `files`/`host` are now pane kinds (Phase 2); `terminal` is now sessions
  (Phase 1); `settings` is already the app window.
- **Transcript + Jira** become a sub-tab strip **inside the Claude pane**
  (`Claude | Transcript | Jira`); Jira only when `session.jiraProject` is set. The
  transcript widget and Jira view move under `ClaudePanel`.
- `status-watch.ts` is **unchanged** — still publishes Claude status for
  off-screen sessions (now: claude-kind sessions not currently tiled). Terminals
  publish nothing (decision 5).

## Phase 4 — Verify

- `pnpm exec tsc --noEmit` clean; any new `*-check.ts` added to `tsconfig.json`
  `include`. `cargo test/clippy --workspace` clean (should be untouched, but the
  workspace bar stands).
- **Migration**: load a real v2 `localStorage` blob → correct tree; **live
  reattach** proven on a real host (a running `claude-<id>` from before the upgrade
  reattaches, not re-spawns). This is the whole point of "preserve ids".
- **Peers**: a terminals-only Project renders and works (zero Claude). `[+]` and
  `[✦]` create the right kind; kill removes the right agent session.
- **Panes**: open host / files / session into a focused tile in 1- and 4-up grid;
  close a pane and confirm the session **survives** (`rmux-agent list` still shows
  it) while its poller stopped.
- **Attention system**: claude status/finished-mark/needs-you count all still work;
  terminals show no status and never inflate the count.
- **`pnpm tauri build`** (never `cargo build --release`), verify the *binary* is
  newer than `dist/index.html`, sign, install — per CLAUDE.md's "verify the
  artefact" rule.

## Explicitly deferred (not v1)

- **Terminal busy ring** — agent reports per-PTY foreground-pgrp-running; new
  `SessionSummary` field + status-watch plumbing (ADR-001 decision 5).
- **Same session in two panes** (mirrored xterm views) — v1 focuses the existing
  tile instead.
- **Jira at Project level** — parked on the claude session for now.
- **Drag-to-place** panes — v1 uses click-fills-focused-tile only.
