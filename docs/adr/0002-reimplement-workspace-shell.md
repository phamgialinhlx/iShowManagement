# 2. Reimplement the workspace shell; do not vendor Zed's `workspace`

Date: 2026-08-11
Status: Accepted

## Context

zmux adopts Zed's *presentation* stack by copy (`gpui`, `ui`, `theme`,
`settings`) — see ADR-0001 — and wants Zed's signature window management:
splittable, draggable panes with docks, replacing the old fixed 4×4 grid.

Zed's `workspace` crate is that engine, but measurement shows it is not UI — it
is Zed's application shell fused to Zed's domain model. It depends on `client`,
`db`, `language`, `project`, `remote`, `session`. The coupling is in the core
abstraction: the `Item` trait every tab must implement is defined in terms of
`Project` (`project_path`, `project_entry_ids`, `project_paths`,
`for_each_project_item`, `active_project_path`). `workspace.rs` (17.3k lines)
names `Project` 243 times; `pane.rs` (9.5k lines) 154 times.

The pure layout pieces, by contrast, are clean: `dock.rs` (1.5k lines, zero
`Project` refs) and `pane_group.rs` (the split-tree geometry) are about
rectangles, not projects.

## Decision

Reimplement the workspace shell on gpui + the adopted presentation stack, with a
**zmux-native tab trait that is remote-session-aware** rather than project-aware
— a tab is a running shell, a Claude run, a buffer over `zmux-fs`, or a
transcript. **Lift `pane_group.rs` and `dock.rs` as the reusable layout core**
(close read / partial copy), since their geometry carries little or no domain
coupling.

Do **not** vendor `workspace`, `pane.rs`, or `item.rs` wholesale.

## Consequences

- The split/drag/dock *behavior* the team wants is delivered without importing
  Zed's project model — the exact impedance mismatch ADR-0001 rejects.
- The tab trait models what zmux tabs actually are (sessions), so persistence,
  status, and "which machine needs me?" derive from the session, not from a
  synthesized `ProjectPath`.
- We own the shell. That is a real cost (drag-to-split is fiddly), but it is
  bounded and self-contained, versus surgery on Zed's largest, fastest-changing
  crate that would yield an un-upstreamable fork.
- Workspace/layout persistence cannot come from Zed's `db`-backed
  `persistence.rs` (5.9k lines, rejected with the domain stack); zmux needs its
  own layout+session persistence (open question — likely a file, not sqlite).
