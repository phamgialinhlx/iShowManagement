# Agents driving agents: the `rmux` CLI

**Audience:** operators who want an agent to delegate work to other agents, and
developers working on rmux. The desktop half (showing spawned sessions beside
their parent) reads the `children.json` contract in [§4](#4-childrenjson).

An agent running inside an rmux session (Claude, pi, or a shell) can start
another agent or a terminal on the same host, prompt it, wait for it, read what
it produced, and close it. This is the model [herdr](https://github.com/herdrdev/herdr)
popularised, built on what rmux already has: every session lives under
`rmux-agent` on the host, so the CLI is a thin client of the same daemon the
desktop app attaches to.

```bash
child=$(rmux spawn --agent claude --name tests | jq -r .session)
rmux prompt "$child" "Run the test suite and summarise the failures." --wait --timeout 900
rmux read "$child" --source transcript --lines 3
rmux close "$child"
```

---

## 1. Inside a session

The daemon gives every session it spawns — sessions the app opens, sessions the
Redstone bridge spawns, and the CLI's own children — this environment:

| Variable | Value |
|---|---|
| `RMUX_ENV` | `1` — "you are inside rmux". The skill refuses to act without it. |
| `RMUX_SESSION` | this session's daemon name, e.g. `claude-session-mwc9x1k0-3f9a2b` |
| `RMUX_BIN` | absolute path of the agent build that owns this session |
| `PATH` | `~/.rmux/bin` prepended |

`~/.rmux/bin/rmux` is a shell **shim** the daemon keeps current (rewritten only
when its content differs, atomically). It execs `$RMUX_BIN`, or — if that build
has since been pruned — the newest `~/.rmux/bin/rmux-agent-*`, skipping
half-uploaded `*.partial*` files. It is a script rather than a symlink because a
symlink to a pruned build is `rmux: not found` with nothing to say why.

`RMUX_BIN` matters because several agent builds coexist on a host (each daemon
socket is fingerprinted, and an upgrade never kills live work), and a session
lives in the daemon of the build that created it. The CLI talks to *its own
build's* daemon, so it must be that build.

**How `PATH` survives a login shell.** Every session is a login shell, and on
Debian/Ubuntu `/etc/profile` *assigns* `PATH`, discarding an inherited prepend.
So for a session launched with a command line (every Claude and pi), the daemon
also prefixes `PATH='<home>/.rmux/bin':"$PATH"` onto that line as an assignment —
it runs after the profiles, and an assignment prefix still lets the shell `exec`
the single command, so the session keeps its pid (status is correlated by pid).
The home is resolved to an absolute path and shell-quoted; never `$HOME` inside
quotes. Lines starting with a reserved word or `(`/`{` are left untouched. The
*recorded* command (what `list` and the app see) is the line as sent.

A plain interactive shell gets only the env prepend. Where a profile resets
`PATH`, `"$RMUX_BIN"` still works; the skill says so.

Everything is `rmux-agent <command>` too; `rmux` is just the shim.

---

## 2. Commands

Success: one JSON value on **stdout** (except `read`, which prints text unless
`--json`). Failure: `{"error":{"code","message",…}}` on **stderr**, exit **1**.
A malformed command line is code `usage`, exit **2**. Session arguments accept an
alias (`--name`, or a rename in the app) as well as the daemon name.

Timeouts are in **seconds**. `spawn` defaults to 30; `wait`, `prompt --wait` and
`wait-output` default to 600. `--timeout 0` means no limit.

### `whoami`

```json
{"session":"claude-session-mwc9x1k0-3f9a2b","host":"devbox","cwd":"/srv/api"}
```

`session` is `null` outside rmux. `cwd` is `$PWD` when it names the current
directory (so a symlinked path stays as the caller sees it).

### `list --json`

```json
{"sessions":[{"session":"claude-session-mwc9x1k0-3f9a2b","alias":"tests","kind":"claude",
  "status":"working","cwd":"/srv/api","parent":"term-mwc9a0aa-1","attached":false,
  "pid":41022,"ageSeconds":312,"conversation":"0b9e…"}]}
```

`kind` is `claude`, `pi` or `terminal` (from the name prefix, the rule the bridge
uses). `status` is herdr's vocabulary, mapped from Claude's own session file or
the `~/.rmux/status` hook: `busy`/`shell` → `working`, `waiting` → `blocked`,
`idle` → `idle`, anything else (and every terminal) → `unknown`. `parent` comes
from `children.json`. Plain `list` without `--json` is unchanged — it is the
tab-separated format the desktop app parses.

### `spawn --agent claude|pi|codex|shell`

```
rmux spawn --agent claude|pi|codex|shell [--cwd DIR] [--name LABEL] [--prompt TEXT|-]
           [--skip-permissions] [--profile NAME] [--timeout S]
```

```json
{"session":"claude-session-mwc9x1k0-3f9a2b","kind":"claude","cwd":"/srv/api",
 "name":"tests","parent":"claude-session-mwc9a0aa-1","profile":"4","chain":null,"status":"idle"}
```

- `--cwd` defaults to the **caller's** directory and must already exist.
- Names follow the desktop app's `reattachName` scheme so it can reattach:
  `claude-session-<base36 ms>-<base36 random>`, `pi-pisession-…`, `term-…`.
- Returns only once the new session can take input: for Claude and pi, when its
  status is published (or, for a Claude too old to publish, its prompt footer is
  on screen); for a shell, once its output has been quiet for 400 ms. A Claude
  that opens on a dialog (usually folder trust) returns `agent_not_ready` at once
  with the `session` in the error; it is kept, so it can be read and answered. An
  agent that exits during startup returns `spawn_failed` with its last lines.
- The child is recorded in `children.json` **before** the readiness wait, so a
  child that never becomes ready is still the caller's to close.
- `--name` labels it (also as an alias for the rail).
- `--prompt` up to 16 KiB is a launch argument (shell-quoted). Larger, or `-`
  from stdin, is typed into the composer once the agent is ready, so it never
  meets `ARG_MAX`. Note a launch-argument prompt is visible in `ps`; use
  `prompt -` for anything sensitive. For a shell, the prompt is typed and run.
- **The child inherits its parent's credential set.** A child of the parent's
  own agent is bound to the parent's set — and to the parent's fallback chain,
  with its *active* member and the members already spent, so it never starts on
  an account the parent used up. A child of a *different* agent (a Claude
  parent spawning Codex) is bound to that agent's **default** set (Settings ›
  Models › default per agent, delivered to the host by the app); if there is
  none the spawn is **refused** (`unsupported`) naming why, rather than running
  on the host's own login. A parent bound to "no profile" passes "no profile" on;
  a parent with no binding at all (a plain shell, an older client) gives none.
  The binding is recorded in `children.json` (`profile`, `chain`) and in the
  JSON above.
- `--profile ID` binds the child to a set the app has delivered to this host by
  **profile id** (or `default` / `none`) instead. A set that is not loaded is
  refused, never substituted.
- `--skip-permissions` — see [§5](#5-security-model). For `--agent codex` it
  means `--dangerously-bypass-approvals-and-sandbox`, under the same rule: only
  from a parent started with either flag.
- **Codex** (`codex-codexsession-…`) publishes no status, so `spawn` returns
  `unknown` once its output settles (or `agent_not_ready` on its folder-trust
  question), and `wait` / `prompt --wait` / `read --source transcript` answer
  `unsupported` rather than time out or guess — use `wait-output` and
  `read --source screen`. The desktop app reads Codex's rendered screen itself.

### `prompt SESSION TEXT|-`

```
rmux prompt SESSION TEXT|- [--wait [--until done|blocked|idle]] [--timeout S]
```

```json
{"session":"claude-session-…","submitted":true,"status":"idle"}
```

Refuses with `agent_blocked`, **before writing anything**, when the target is
showing a dialog. Claude and pi get the shared two-write submission
(`bridge::send_message` → `rmux_claude_core::keys`, the one definition of how a
message is submitted); a shell gets the text and Enter. `-` reads stdin — use it
for long text.

`--wait` (agents only) waits for `--until` (default `done`). It must see some
activity — `working`, `blocked`, or a status change since the prompt — within
5 s, else `agent_prompt_stalled`. A `blocked` while waiting for `done` returns
`agent_blocked` at once rather than at the timeout. A timeout or stall does not
prove the prompt was lost.

### `wait SESSION [--until working|blocked|done|idle] [--timeout S]`

```json
{"session":"claude-session-…","reached":"done","status":"idle"}
```

`done` = idle after work: `working` was seen during the wait, or the status file
changed at or after the session's **last prompt** (remembered in
`~/.rmux/prompted.json`), or the call's start when there was none. So
`prompt` now and `wait --until done` later does not miss a turn that already
finished. Terminals: `unsupported` — use `wait-output`. Timeout: `timeout`,
with the current `status` in the error.

### `read SESSION [--lines N] [--source screen|transcript] [--json]`

Prints text. `screen` (default, last 50 lines) is the daemon's scrollback with
carriage-return overwrites collapsed and escape sequences stripped — best-effort,
since there is no terminal emulator in the agent. `transcript` (Claude/pi, last
10 messages) is the conversation read from the agent's own `.jsonl`, tail-bounded
and system plumbing hidden — the faithful view. `--json`:

```json
{"session":"…","source":"transcript","text":"[assistant]\nAll 214 tests pass.","truncated":true}
```

### `wait-output SESSION (--regex R | --match TEXT) [--timeout S]`

```json
{"session":"term-…","matched":"test result: ok","line":"test result: ok. 214 passed"}
```

Searches the existing buffer immediately (herdr semantics), so output that
arrived before the call still matches. `--regex` is Rust `regex` syntax.

### `keys SESSION KEY...`

```json
{"session":"claude-session-…","sent":2}
```

`enter`, `esc`, `tab`, `shift-tab`, `up`, `down`, `left`, `right`, `home`, `end`,
`pgup`, `pgdn`, `backspace`, `delete`, `space`, `ctrl-<letter>` (`ctrl+c`, `c-c`),
or any single character (`y`, `n`, `2`). Every key is validated before any is
written; each is a separate write so `esc` then `[` is not read as Alt-[. The
table is `rmux_claude_core::keys::named`.

### `interrupt SESSION`

```json
{"session":"claude-session-…","interrupted":true}
```

Esc for an agent (Claude's "esc to interrupt"), Ctrl-C for a terminal.

### `close SESSION [--force]`

```json
{"session":"term-…","closed":true}
```

Refuses with `not_owner` unless SESSION descends from `$RMUX_SESSION` in
`children.json`; outside a session nothing is yours. `--force` overrides — it is
the operator's escape hatch, and the skill tells agents not to use it. The kill
is the daemon's handshake-time `Kill`, resolved across agent builds; `close`
then waits until the pid is gone and reports `close_failed` if it is not.

### `children`

The pruned `children.json` ([§4](#4-childrenjson)), written back if pruning
changed it.

### Error codes

`usage` (exit 2), `not_found`, `agent_blocked`, `agent_not_ready`,
`agent_prompt_stalled`, `spawn_failed`, `timeout`, `not_owner`,
`permission_denied`, `unsupported`, `close_failed`, `failed`. Errors about a
session carry `"session"`, and waits carry the last `"status"`.

---

## 3. The skill

`crates/rmux-claude-core/assets/rmux-skill/SKILL.md`, adapted from herdr's agent
skill (Apache-2.0). It teaches: act only when `RMUX_ENV=1`; use the names the
JSON returns; prefer `prompt --wait`; report a `blocked` agent to the operator
rather than answering its dialog; never close what you did not spawn; spawned
sessions appear in the operator's window; do not stop rmux.

**Installed** by the daemon whenever it creates a session whose command is
`claude` (app, bridge or CLI): `~/.claude/skills/rmux/SKILL.md`, temp-then-rename.
The installed copy carries `<!-- rmux-managed fnv=<hash> -->` (after the YAML
frontmatter, which must stay first). An existing file is replaced only when it
carries that marker **and** its text still hashes to it — i.e. an unmodified copy
from some rmux build. An edited copy, or a skill named `rmux` that rmux did not
write, is never touched. An identical copy is not rewritten.

**pi:** not installed. Nothing in rmux's pi integration (`rmux-claude-core::pi`)
establishes a skills directory convention for pi, so there is no verified place
to put it.

---

## 4. `children.json`

`~/.rmux/children.json`, `0600` inside the `0700` `~/.rmux`, written
temp-then-rename under an `flock` on `~/.rmux/children.lock` (two agents
spawning at once is the normal case):

```json
{"version":1,"children":[
  {"session":"term-mwc9a0aa-1x","parent":null,"kind":"terminal","cwd":"/srv/api","name":"term-mwc9a0aa-1x","createdAt":1794411903881},
  {"session":"claude-session-mwc9x1k0-3f9a2b","parent":"term-mwc9a0aa-1x","kind":"claude","cwd":"/srv/api","name":"tests","createdAt":1794411905869}
]}
```

- `session`, `parent`: daemon session names; `parent` is `null` when spawned
  outside rmux.
- `kind`: `claude` | `pi` | `terminal`.
- `name`: the `--name` label, else the session name.
- `createdAt`: Unix milliseconds.
- **Pruned** of sessions that no longer exist on every write and every
  `children`/`list --json`/`close` read. A pruned entry's children are
  **re-parented** to its parent, so closing a middle session does not cut the
  chain that lets the top agent close the grandchildren.
- A file whose `version` is not `1` reads as empty.

---

## 5. Security model

- **Local only.** The CLI speaks to the daemon over its existing Unix socket
  (`0600`, in `0700` `~/.rmux`). It opens no port and dials nothing. Every caller
  is a process on this host running as this user.
- **It is not the bridge.** The Redstone bridge's verb set stays closed, and its
  test that fails on any method named `exec`/`shell`/`command`/`run` is
  untouched — that rule exists because the bridge is reachable from *off* the
  machine. `prompt` and `keys` to a terminal are acceptable here because the
  caller already has a shell on this host as this user; they grant nothing it
  lacks.
- **Text through stdin, not argv**, for anything long or sensitive: `prompt -`
  and `--prompt -`. Large spawn prompts are typed, never put in argv.
- **`--skip-permissions` cannot escalate.** A child may be started without
  Claude's permission checks only if the caller's own session was started with
  `--dangerously-skip-permissions` (read from the daemon's record of the parent's
  launch line), the flag is explicit (never inherited silently), and outside a
  session it is refused. This is a guardrail against a cooperating agent granting
  itself latitude, not a security boundary — `RMUX_SESSION` is an environment
  variable, and anything running as this user can run `claude
  --dangerously-skip-permissions` directly.
- **Ownership is advisory in the same sense.** `close` refuses strangers so an
  agent cannot tidy away the operator's work by mistake; it is not access
  control between processes of one user.
- Every value interpolated into a shell line goes through `shell_quote`.

---

## 6. Open questions

- **`--profile` takes an id, not a name.** The host holds only the sets the app
  delivered (each session's own, chain members, and the per-agent defaults), keyed
  by profile id; it has no keychain to resolve a name against.
- **Fullscreen Claude children.** CLI-spawned Claude runs inline (rmux's
  `Rendering::default()`), which keeps `read --source screen` legible. The app
  defaults its own new sessions to fullscreen.
- **Status for terminals.** Shells report `unknown`; `wait`/`prompt --wait`
  are refused for them in favour of `wait-output`.
