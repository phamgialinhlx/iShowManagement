---
name: rmux
description: "Start and coordinate other coding agents (Claude, pi) and shells beside you in the operator's rmux window, through the `rmux` CLI. Use only when the user asks you to delegate, run work in parallel in another agent or terminal, or check on another rmux session — and only when RMUX_ENV=1. Do not use merely because a task could be parallelised."
---

<!-- rmux-managed -->

# rmux

rmux keeps every shell, Claude and pi session on this machine under a small
host daemon, and shows them in the operator's rmux window. The `rmux` CLI lets
you — an agent running inside one of those sessions — start another agent or a
shell beside you, prompt it, wait for it, read what it produced, and close it.

Before running any `rmux` command, check that you are inside an rmux session:

```bash
test "${RMUX_ENV:-}" = 1
```

If that fails, say you are not running inside rmux and stop. Do not try to
drive rmux sessions from outside one.

When the check passes, `rmux` is on your `PATH`. If it is not (some login
profiles reset `PATH`), use `"$RMUX_BIN"` instead — it is always set inside an
rmux session and points at the agent build that owns your session. Your own
session name is `$RMUX_SESSION`; `rmux whoami` prints it with the host and cwd.

## The commands

Every command prints JSON on stdout when it succeeds. A failure prints
`{"error":{"code":"…","message":"…"}}` on stderr and exits 1; a mistake in the
command line itself exits 2. **Read session names from the JSON you get back —
never guess or construct one.**

```bash
rmux whoami                                   # {"session":"claude-…","host":"…","cwd":"…"}
rmux list --json                              # every session: kind, status, cwd, parent, attached
rmux spawn --agent claude --name reviewer     # {"session":"claude-session-…","kind":"claude",…}
rmux spawn --agent shell --cwd "$PWD"         # a plain terminal
rmux prompt SESSION "text" --wait             # submit, then wait for it to finish
printf '%s' "$long_text" | rmux prompt SESSION - --wait   # long prompts via stdin
rmux wait SESSION --until done|blocked|idle|working --timeout 600
rmux read SESSION --lines 80                  # the terminal's recent output, as text
rmux read SESSION --source transcript --lines 10   # last messages of a Claude/pi conversation
rmux wait-output SESSION --regex 'tests? passed' --timeout 300
rmux keys SESSION esc                         # enter esc tab up down ctrl-c y n 1 2 …
rmux interrupt SESSION
rmux close SESSION                            # only sessions you spawned
rmux children                                 # who spawned what
```

Statuses are `working`, `blocked` (a permission dialog or question is on its
screen), `idle`, and `unknown` (a plain terminal, or an agent rmux cannot read).

### Start an agent and hand it work

```bash
child=$(rmux spawn --agent claude --name tests --cwd "$PWD" | jq -r .session)
rmux prompt "$child" "Run the test suite and summarise the failures." --wait --timeout 900
rmux read "$child" --source transcript --lines 3
```

`spawn` returns only once the new agent is ready for input (default 30s,
`--timeout` to change). It defaults to **your** working directory. A spawned
session appears beside you in the operator's rmux window — they can see and
type into everything you start.

### Run a command in a terminal

```bash
term=$(rmux spawn --agent shell | jq -r .session)
rmux prompt "$term" "cargo test 2>&1 | tail -n 20"
rmux wait-output "$term" --regex 'test result' --timeout 600
rmux read "$term" --lines 40
```

`prompt` to a shell types the text and presses Enter. `wait` and
`prompt --wait` need an agent's status, so they are refused for a terminal —
use `wait-output` there.

## Rules

- **Prefer `prompt --wait`.** It refuses with `agent_blocked` if the target has
  a dialog open, and it returns `agent_prompt_stalled` if nothing started within
  a few seconds. A timeout or stall does not prove the prompt was lost — read
  the session before you send it again.
- **Do not answer another agent's permission dialogs.** When a session is
  `blocked`, `read` it and tell the operator what it is asking. Only press keys
  into a dialog when the operator has told you the answer.
- **Never close what you did not create.** `close` refuses with `not_owner`
  unless the session descends from yours in `rmux children`. Do not reach for
  `--force` to get past that; ask the operator.
- **Use the names you were given.** `RMUX_SESSION` for yourself, the `session`
  field of a `spawn` response for a child. Names like `claude-session-…` are
  generated; do not predict them.
- **Do not grant more latitude than you have.** `--skip-permissions` is refused
  unless your own session was started with it, and even then use it only when
  the operator asked for it.
- **Do not stop rmux.** Never kill `rmux-agent` processes or the daemon; closing
  the sessions you started is all the cleanup there is.
- `--profile` (model profiles) is applied by the rmux app, not from a host —
  `spawn` refuses it. A spawned agent uses the host's current provider setup.

---

Adapted from the Herdr agent skill (`skills/herdr/SKILL.md`, Apache-2.0,
https://github.com/herdrdev/herdr) for rmux's CLI.
