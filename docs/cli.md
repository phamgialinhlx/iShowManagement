# The `rmux` command line

`rmux` is the rmux agent binary — the same one that keeps sessions alive on every
host — used tmux-style from any terminal. It lists, attaches, starts and ends
sessions on this machine and on every server you added in the rmux app.

```text
$ rmux ls
local:demo                 terminal  idle     /Users/me/src/api   (detached)  4m
local:claude-session-…     claude    working  /Users/me/src/web   (attached)  2h
dev@10.0.0.107:api   terminal  unknown  -                   (detached)  3d
devbox: unreachable (timed out)
```

## Commands

| Command | What it does |
|---|---|
| `rmux` | Attach the most recently attached session here, or start a shell if there is none. |
| `rmux ls [-t SERVER] [--json]` | Every session here and on every server, asked in parallel (6s per host). An unreachable server is a dim line, not a failure. |
| `rmux attach [-t [SERVER:]SESSION]` | Attach this terminal. With no `-t`: the most recently attached session here, else the only one, else a list to choose from. |
| `rmux new [-s NAME] [-c DIR] [-t SERVER] [--claude\|--pi] [-d]` | Start a shell (or Claude, or pi) and attach, unless `-d`. `NAME` becomes the session's name in the app too. |
| `rmux kill-session -t [SERVER:]SESSION` | End it — the shell and everything running in it. |
| `rmux rename-session [-t [SERVER:]SESSION] NEWNAME` | Rename (the app shows the new name). Without `-t`, inside a session, renames that one. |
| `rmux has-session -t [SERVER:]SESSION` | Exit 0 if it exists, 1 if not. |
| `rmux detach [-s SESSION]` | Detach command-line clients of a session (inside a session: this one). The app's own views are never detached. |
| `rmux -V`, `rmux --help` | Version; every command, grouped. |

Aliases: `list-sessions`, `a`/`attach-session`, `new-session`, `detach-client`.

The agent-orchestration commands (`spawn`, `prompt`, `wait`, `read`, …) are the
same binary — see [orchestration.md](orchestration.md).

### Naming a session

`SESSION` is a name you gave it (`-s`, or a rename in the app), its full id
(`term-…`, `claude-session-…`), or any unique prefix of either. An ambiguous
prefix is an error that lists the matches.

`SERVER` is a server from the app's list — by the name `ls` prints, its host, or
`user@host` — or `local`, or any ssh destination typed verbatim
(`user@host:port`). `-t dev@10.0.0.107:api` is a session `api` on that
server.

### Accounts and fallback

```
rmux fallback-log [--json]                  # every account switch on this host, and which set each session is on
rmux fallback-switch --session NAME --to ID # move a chained session to that account and relaunch it
```

A Claude session started from rmux on a **fallback chain** is moved by the host
to the next account when the current one hits a usage or rate limit — with the
app closed too. The move resumes the same conversation (`claude --resume`) and
types a short "continue" message; it is recorded in `~/.rmux/fallback.json`
(`0600`). It never cycles back on its own: `fallback-switch` is the operator's
"switch back". `rmux new` from inside a session inherits that session's account
exactly as `rmux spawn` does.

## Detaching

Inside a command-line attach, **`Ctrl-B` then `D`** detaches, printing
`[detached (from session NAME)]`; the session keeps running. `Ctrl-B Ctrl-B`
sends one literal `Ctrl-B`. Change it with `--detach-key "C-a d"` or
`RMUX_DETACH_KEY`; `none` turns it off.

The key exists only for attaches made by this command. The rmux app attaches
without one, because `Ctrl-B` is the app's own prefix. If you run `rmux attach`
in a terminal *inside* the app, press the app's `Ctrl-B Ctrl-B` (which sends a
literal `Ctrl-B`) and then `D`.

A server whose agent predates the detach key still attaches; press `Enter` then
`~.` to drop the connection — the session survives it either way.

## How it finds your servers

The app writes `~/.rmux/servers.json` whenever its server list changes:

```json
{"version":1,"servers":[{"id":"…","label":"dev@10.0.0.107","host":"10.0.0.107","user":"dev"}]}
```

`0600`, destinations only — no passwords, no tokens. A server you gave a key
file in the app also carries `"identity": "/path/to/private/key"` — the path,
never the key. `rmux` runs `ssh` itself,
with the host exactly as you entered it (your `~/.ssh/config`, `ProxyJump`,
certificates all apply), the key you chose for it and rmux's own key for that
host (`~/.ssh/rmux_<user>_<host>_ed25519`), each only when the file exists and
in addition to your usual keys, and a connection kept open for a minute under `~/.rmux/cm/` so
the next command is quick. `ls` never prompts for a password; `attach`, `new`
and the rest may.

On the server it runs `~/.rmux/bin/rmux` (or the newest `~/.rmux/bin/rmux-agent-*`)
— the agent the app installed. Listing, attaching, renaming and ending work
against any agent the app ever installed; `new` needs one recent enough to have
this command line, and says so otherwise — open the server once in the app to
upgrade it. Windows servers are reported as unsupported: sessions there do not
persist.

## Getting `rmux` on PATH

- **macOS and Linux:** Settings › COMMAND LINE › INSTALL. It links
  `/usr/local/bin/rmux` when that folder is writable, else `~/.local/bin/rmux`,
  to the agent inside the app — so updating the app updates the command. If
  `~/.local/bin` is not on your PATH, the panel says so. An AppImage copies the
  binary instead (its files vanish when it closes); reinstall after updating.
  UNINSTALL removes only a link it made.
- **On a server**, inside a session `rmux` is already on PATH. To use it from a
  plain ssh login too:

  ```sh
  mkdir -p ~/.local/bin && ln -sf ~/.rmux/bin/rmux ~/.local/bin/rmux
  ```
