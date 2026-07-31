# Claude status: 4-state detection via the sessions file

**Date:** 2026-07-31
**Goal:** Replace the fragile hook-latch + `pane_current_command == "claude"` scan with a reliable 4-state model — **WORKING / WAITING / UNREAD / READ** — driven by Claude Code's own per-session status file.

---

## Why

Today `build_inventory` (`core/src/notify.rs:457`) decides "is this Claude, and what's it doing" from two weak signals:
1. `pane_current_command == "claude"` — **already broken on CLI 2.1.220**: tmux reports the process *title* (`2.1.220`), not `claude` (confirmed 2026-07-31).
2. the latest line in `~/.ism/notify.jsonl` — a *latch* that sticks on `working` if the closing event never lands (crash, SSH drop, hook timeout), and produces stale false-positives for dead panes.

Neither verifies anything against Claude's actual state.

## The signal (confirmed firsthand, CLI 2.1.220)

Claude writes `~/.claude/sessions/<pid>.json` and updates it live:

```json
{"pid":18987,"sessionId":"29f4f72f-…","cwd":"/Users/linh/work/iShowManagement",
 "version":"2.1.220","kind":"interactive","status":"waiting",
 "waitingFor":"input needed","statusUpdatedAt":1785474593641}
```

`status` values observed live: `busy` (generating), `shell` (running a Bash tool / shelled out), `idle` (at prompt), `waiting` (HITL, blocked on user; `waitingFor` gives detail). No hooks, no scraping, cross-platform (file present on macOS).

## State model

Two axes: **Claude's objective state** (from the host) and **the user's attention** (app-side).

| State | Source | Rule |
|---|---|---|
| **WORKING** | host | `status ∈ {busy, shell}` |
| **WAITING** | host | `status == "waiting"` (key on status alone; `waitingFor` is tooltip only) |
| **UNREAD** | host + app | `status == idle` **and** `statusUpdatedAt` > user's last view of that session |
| **READ** | app | `status == idle` **and** viewed since it went idle |

Precedence: `WORKING > WAITING > UNREAD > READ`. WAITING is a host-state, not attention — viewing a blocked session does **not** clear it; only answering (status flips off `waiting`) does.

---

## Backend changes — `core/src/notify.rs`

### 1. One round-trip, three sections (`claude_inventory`, ~line 423)

Replace the `tail notify.jsonl … ===PANES=== … list-panes` command with:

```sh
for f in "$HOME"/.claude/sessions/*.json; do [ -f "$f" ] && cat "$f" && echo; done 2>/dev/null
echo '===PS==='
ps -eo pid,ppid,tty,comm 2>/dev/null
echo '===PANES==='
tmux list-panes -a -F \
  '#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_id}\t#{pane_tty}\t#{pane_pid}\t#{pane_current_path}' \
  2>/dev/null
```

### 2. Rewrite `build_inventory(sessions, ps, panes)` (pure, testable)

```
alive:  pid -> {ppid, tty_norm, comm}         # from ps
by_tty:  tty_norm -> pane                       # from list-panes, strip /dev/ from pane_tty
by_ppid: pane_pid -> pane

for each sessions/<pid>.json:
    if pid not in alive:            continue     # stale file, process gone
    if alive[pid].comm != "claude": continue     # recycled-pid guard
    pane = by_tty[alive[pid].tty_norm]
        or (walk pid -> ppid until a pane_pid matches) -> by_ppid
    if pane is None: continue                     # non-tmux Claude — not in tree (v1)
    status = busy|shell -> "working"
             waiting     -> "waiting"
             else        -> "idle"
    emit { paneId, window, windowName, pane, status, waitingFor,
           statusUpdatedAt, sessionId, project: basename(cwd) }
    group by pane.session
```

TTY normalization: strip leading `/dev/`. Confirmed match: `ps` `ttys007` ↔ tmux `/dev/ttys007`; Linux `pts/3` ↔ `/dev/pts/3`.

### 3. Response shape (per instance)

```json
{ "paneId":"%0", "window":0, "windowName":"…", "pane":0,
  "status":"working|waiting|idle", "waitingFor":"input needed",
  "statusUpdatedAt":1785474593641, "sessionId":"…", "project":"iShowManagement" }
```

Drop the hook-derived fields (`kind`, `notificationType`, `message`, `summary`). `contextTokens` is dropped for v1 (see caveats).

### 4. Keep untouched

`install` / `uninstall` / `status` / `events` / **`notify_ws`** and `banner_text` stay — the hook still powers **instant push banners** (6s polling is too slow for a snappy "finished" toast). Only tree-state detection moves to the sessions file. The `set_watching` heartbeat stays as-is.

---

## Frontend changes

### `web/src/lib/api.ts` (`ClaudeInstance`, line 118)

```ts
export interface ClaudeInstance {
  paneId?: string; window?: number; windowName?: string; pane?: number
  status: 'working' | 'waiting' | 'idle'   // objective, from host
  waitingFor?: string
  statusUpdatedAt?: number
  sessionId?: string
  project?: string
}
```

### `web/src/lib/HostTmuxTree.svelte`

- Keep `lastViewed: Record<string, number>` (per session name; persist to `localStorage`). Set `lastViewed[session] = Date.now()` while that session's tab is the **active** one (reuse the existing `activeName` prop).
- Derive the display state:
  ```ts
  function display(inst, session): 'working'|'waiting'|'unread'|'read' {
    if (inst.status === 'working') return 'working'
    if (inst.status === 'waiting') return 'waiting'
    return (inst.statusUpdatedAt ?? 0) > (lastViewed[session] ?? 0) ? 'unread' : 'read'
  }
  ```
- `badgeLabel` / dot classes (`notify.rs` today: working/needs/done): map `working→working`, `waiting→needs-you`, `unread→unread`, `read→(muted)`. This is a straight evolution of the existing three dots — `done` splits into `unread`/`read`.

---

## Fallback ladder (phase 2, optional)

For hosts on CLI < ~2.1.119 (no sessions file): if a pane's `pane_current_command == "claude"` **or** it has a claude process on its tty but no sessions file, `tmux capture-pane -p -t <pane>` and grep:
- `esc to interrupt` / `ctrl+c to interrupt` (case-insensitive) → WORKING
- `Do you want` / `❯ 1.` / `esc to cancel` → WAITING
- else → idle (with ~1.5s stability debounce)

Skip entirely if every managed host is ≥ 2.1.119.

---

## Phased steps (each independently verifiable)

1. **Pure core rewrite.** Rewrite `build_inventory` + add parsers; keep the endpoint shape adapter. → *verify:* unit tests over captured fixtures (busy/shell/idle/waiting sessions JSON + a real `ps` + `list-panes` dump from session `hi`) assert correct state + pane mapping, incl. the `pane_current_command=2.1.220` and stale-file cases.
2. **Wire the new SSH command** into `claude_inventory`. → *verify:* `curl /api/servers/local/tmux/claude` (or a live host) returns the 4 statuses matching the watcher log.
3. **Frontend types + display derivation + read/unread.** → *verify:* open a session in the app, drive a second Claude through busy→waiting→idle, confirm dots show working→needs-you→unread, and focusing the tab flips unread→read.
4. **(Optional) capture-pane fallback** behind an `is-sessions-file-present` check.

## Caveats / risks

- **Undocumented** file — could change across CLI versions. Mitigation: it's Claude's own machine-readable state (far more stable than scraping UI text); pin the fallback ladder.
- **Version-gated** (needs ≥ ~2.1.119). Fallback covers older hosts.
- **`contextTokens` lost** — the tree's context-fullness indicator came from the hook's transcript read. Derivable later from `sessionId` → `~/.claude/projects/<slug>/<sessionId>.jsonl`, but out of scope for v1.
- **Non-tmux Claude** (plain SSH shell) has a sessions file but no pane → not shown in the tmux-grouped tree (unchanged from today; still gets banners via the hook).
- **Read/unread granularity** is per-session in v1 (viewing a session marks all its Claudes read). Per-pane is a later refinement.
