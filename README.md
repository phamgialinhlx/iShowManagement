# iShowManagement

A lean, single-binary desktop app for managing SSH hosts and everything running on
them — an interactive **Console / tmux**, background **managers** (docker, ports,
processes), a **Files** browser, per-host **port forwarding**, and a server-side
**Browser** (SOCKS proxy + Chrome).

It drives the system `ssh` binary through a PTY, so your existing `~/.ssh/config`,
keys, agent, and `ProxyJump` all work as-is — no separate connection setup.

> Inspired by [**tsmanager**](https://github.com/vietrux/tsmanager) (a Node/Electron SSH session
> manager). iShowManagement is a ground-up **Rust port**, built most-basic-feature-first
> as a series of vertical slices. The goal is the same workflow in a single native
> binary: no Electron runtime, no plugin host, no bundled AI client.

## Features

- **Hosts** — read live from `~/.ssh/config`; recently-used float to the top.
- **Console & tmux** — persistent terminal sessions, with a per-host tmux session
  tree so you can attach/detach named sessions from the sidebar.
- **Managers** — overview, docker (with logs/exec/actions), listening ports, and
  processes, each parsed straight from `ssh exec`.
- **Clipboard** — remote copies (tmux yank, nvim `"+y`, OSC 52) land on the
  macOS clipboard; Cmd+V pastes into the remote terminal. Tmux sessions are
  auto-configured for clipboard pass-through on attach. Pasting an **image**
  in a console/tmux terminal uploads it to the host's `/tmp` and types the
  path — so CLIs like Claude Code can read it.
- **Files** — browse, preview, and download (rsync → scp fallback).
- **Port forwarding** — `ssh -L` tunnels managed per host.
- **Browser** — server-side `ssh -D` SOCKS proxy + a Chrome launch (macOS).
- **Claude notifications** — optional per-host hook that pushes notifications over
  a WebSocket.

## Architecture

```
core/      Rust — axum server: REST + WebSocket + static file serving
  ssh.rs         arg building, ControlMaster, ssh -G, password injection
  pty.rs         portable-pty spawn + reader-thread → mpsc bridge
  ws.rs          WebSocket ↔ PTY glue (console/tmux/logs/exec modes)
  discovery.rs   ~/.ssh/config enumeration + `ssh -G` resolution
  secrets.rs     keyring primary, AES-256-GCM file fallback
  managers.rs    overview / docker / ports / processes
  files.rs       list / stat / preview / download
  forward.rs     ssh -L lifecycle
  browser.rs     ssh -D SOCKS + Chrome launch
  security.rs    origin guard, loopback bind, safe names, shell quoting
web/       Svelte + TS + Vite — SPA, built to static assets core serves
desktop/   Tauri wrapper — packages the app into a native .app / .dmg
```

The server source of truth is `~/.ssh/config` (live). The app persists only what
ssh config can't hold (hidden flag, SOCKS index, stored-password flag), keyed by
Host alias.

## Development

Run the core server on its own (`http://127.0.0.1:7070`) with the SPA served from
`web/dist`:

```sh
# frontend
cd web && npm install && npm run build

# server (serves web/dist + REST/WebSocket API)
cargo run -p core
```

During UI work, run the Vite dev server against the core API:

```sh
cd web && npm run dev
```

## Build the desktop app

Requires the Tauri CLI (installed as a dev dependency in `web/`):

```sh
cd web && npm run build            # build the frontend into web/dist
cd desktop && npx tauri build      # compiles core + bundles the app
```

Outputs land in `target/release/bundle/` (`macos/iShowManagement.app` and a `.dmg`).

> The bundle is unsigned (ad-hoc), so on first launch macOS Gatekeeper will block
> it — right-click the app and choose **Open** to allow it.
