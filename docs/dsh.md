# DeepSeek Harness (`dsh`) in rmux

DeepSeek Harness is an agent harness whose interface is a **Web UI** (`dsh web`);
it ships no terminal UI. rmux runs it as a session kind of its own — **DeepSeek
Harness** in a project's `+` row (the `DS` button) or its right-click menu — and
shows that Web UI inside the pane.

## What happens when you open one

1. **The server starts on the target, under `rmux-agent`**, as session
   `dsh-<id>`, so it keeps running when rmux is closed, the lid shuts or the
   network drops — like a shell or a Claude run. It is launched through the
   login shell (`-l -i`), because `node`/`npx` usually come from a version
   manager whose PATH exists only there.
   - `dsh` on the PATH is used if present; otherwise
     `npx -y @deepseek-ai/dsh` (the first run downloads it — the pane says so).
   - The port is chosen **on the target** by node (`listen(0)` on loopback), and
     the server is started with `web --no-open --host 127.0.0.1 --port <P>`. It
     never listens on a LAN address.
2. **rmux reads the address from the server's own output.** `dsh web` prints
   one line, `dsh web: http://127.0.0.1:<P>/?token=<launch token>`. The token is
   a per-process credential that dsh exchanges for a cookie; rmux keeps it in
   memory on the desktop side and never passes it to the page. After an rmux
   restart, the agent's scrollback replay carries the line back.
3. **A loopback tunnel** (`ssh -N -L 127.0.0.1:<local>:127.0.0.1:<P>`, its own
   connection, `ExitOnForwardFailure`) brings it to this machine. A local
   target needs none.
4. **The Web UI is a native webview laid over the pane** and follows it on
   resize, tab switch and split drags; it hides when the pane is hidden.

While it starts, the pane shows the server console (the `>_` icon toggles it
later). If Node is missing or too old, the console and a banner say so with the
install hint — the pane is never blank.

Header controls: console, **open in browser** (the same loopback URL in your
real browser), **restart** (kills the server on the host and starts a fresh
one), session settings.

Closing the session (✕) kills the server on the host and closes the tunnel.
Detaching closes the tunnel and view only; the server keeps running.
Disconnecting a server closes its dsh tunnels and views with everything else.

## Requirements

- **Node.js 24.9 or newer** on the target's login PATH (dsh's `engines.node`
  is `>=24.9.0`). `nvm install 24` is the usual fix.
- macOS or Linux target (the agent does not run on Windows hosts).
- The **rmux desktop app**. rmux-web shows a "not available yet" message: the
  tunnel ends on the dev host's loopback, which a remote browser cannot reach.

## Credentials

dsh reads its DeepSeek key from the environment: `DEEPSEEK_API_KEY` by default
(`apiKeyEnv` in `@deepseek-ai/dsh-llm-deepseek-api-key`), plus
`DEEPSEEK_BASE_URL` for another endpoint. It also reads
`$DSH_HOME/.credentials.yaml`, the working directory's `.env` and
`$DSH_HOME/.env` (`$DSH_HOME` defaults to `~/.dsh`).

rmux delivers keys the same way it delivers Claude's: a model profile's
variables reach the host with `rmux-agent setenv` (over stdin, never argv) into
the agent daemon's environment, and the dsh server — spawned by that daemon —
inherits them. Nothing about the key passes through the dsh pane. A key typed
into dsh's own Settings is stored by dsh on the host, not by rmux.

## Status in the rail

`unknown` while starting; then, every 3 s while the pane is mounted, one
`session/list` call through the tunnel (the RPC dsh's own sidebar uses):

| dsh reports | rail |
|---|---|
| any session `running` | working |
| an open question (`userQuestions.active`) | waiting |
| otherwise | idle |
| server exited / Node missing | error, with the reason |

Not mapped: dsh's tool-approval prompts have no flag in `session/list` that we
found, so an approval wait reads as `working`. The status is only refreshed
while the pane is mounted.

## Limits worth knowing

- **The view is native, so it draws above the page.** An rmux menu or dialog
  opened over a dsh pane appears behind it. Switch the pane to the console or
  settings view first, or move the menu.
- dsh's own default workspace is `~/Documents/deepseek-harness/default-workspace`
  (observed with 0.2); pick the project folder in dsh's workspace picker.
- dsh is in preview; its startup line and RPC shapes are what the parser and
  status probe read. Both fail soft (no endpoint → the console stays up; an
  unreadable status → no status, never a guessed idle).
