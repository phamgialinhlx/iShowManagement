# Context — zmux

Glossary of the ubiquitous language. Terms only; no implementation detail.

## Workspace
The single top-level surface of the app inside one OS window. Holds a tree of
**Panes** and a set of **Docks**. Replaces the old **fixed grid** (a 4×4 array of
tiles each bound to one session). Modelled on Zed's workspace.

## Pane
A rectangular region of the Workspace that displays one **Tab** at a time and
carries the tab strip for its group. Panes are arranged in a splittable tree
(split horizontally/vertically) and can be resized by dragging their borders.
The old model had immovable tiles; a Pane can be split, closed, and have Tabs
dragged into and out of it.

## Tab
One unit of content shown in a Pane — a terminal, a Claude session, an editor
buffer, a transcript, a file tree. Tabs can be dragged between Panes. A Tab is
the new home of what the old model called a "session pane". Every Tab kind is a
peer; none is structurally privileged.

A **Session** is surfaced as a single, **atomic** Tab — the remote process, not a
group. The old **companion shell** and **WorkspaceDeck** were workarounds for the
fixed grid and are **retired**: "a shell beside Claude" is now just splitting a
Pane and opening a terminal Tab. Convenience is preserved by an *opinionated
default arrangement* on "new Claude session" (open the Claude Tab, split a shell
beside it), but the pieces remain independent, freely-rearrangeable Tabs.

## Dock
An edge-anchored region (left / right / bottom) that slides in and out and holds
supporting UI — e.g. the session rail, file tree, or widgets. Distinct from a
Pane: docks frame the work rather than holding the primary content.

## Session
A running unit of remote work owned by `zmuxd` on a target host — a shell or
a Claude run that survives the app closing. A Session is surfaced in the Workspace
as one or more Tabs, but it *is* the remote process, not the UI showing it.
(Unchanged in meaning from today; only its on-screen container changes.)

## Target
A place work runs — a local machine or an SSH host — resolved to a locally
spawnable argv by `zmux-transport`. The "local and remote are one code path"
invariant is a property of the Target, not of the UI, and survives the redesign.
