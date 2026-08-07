# rmux — agent instructions

A fast, direct-SSH remote dev client. Rust core (Tauri v2) + React/Tailwind/motion UI.
Replaces `redstone-cowork`'s Electron desktop app. Full plan:
`~/.claude/plans/if-you-check-redstone-cowork-dynamic-spark.md`.

## The one architectural rule

**Remote coding never goes through the Cowork server.** Terminals, files, metrics,
browsers and Claude sessions are a direct SSH connection from this client to the target
host. The Cowork server (`../redstone-cowork/apps/api`, NestJS) is reused *only* for auth,
the shared server registry, messaging, token-usage reporting, leaderboard and team targets.

The previous generation relayed sessions through that server, which is where its whole
bug class came from — ghost permission cards, eaten answers, sessions reaped by poller
heartbeat, stale keep-alives after an API redeploy. If a design idea reintroduces a server
hop on the session path, it is wrong.

## Do not copy from redstone-cowork

Read it for the *design language* and the *feature list*. Never port its code — an
earlier attempt to make an offline edition by modifying it in place inherited its
problems. Every file here is written fresh.

## Layout

```
crates/rmux-transport   Target trait: Local | Ssh — THE seam (see below)
crates/rmux-ssh         system `ssh` + ControlMaster; askpass bridge
crates/rmux-agent       remote daemon + thin stdio proxy (not yet built)
askpass/                SSH_ASKPASS helper binary — routes credential prompts to the UI
crates/rmux-term        local PTY + scrollback; terminals outlive their views
crates/rmux-fs          FileSystem trait: LocalFs | TargetFs (POSIX shell over ssh)
crates/rmux-metrics     CPU/memory sampled over the existing connection
crates/rmux-claude      Claude PTY control + screen parsing (not yet built)
crates/rmux-control     the socket other apps drive rmux through (rbrowse and friends)
crates/rmux-cowork      Cowork server client + OS keyring
src-tauri/              thin IPC layer only — no logic
ui/                     React 19 + Tailwind 4 + motion
```

## Invariants

- **Local and remote are one code path.** Everything is written against
  `rmux_transport::Target`. There must never be an `if is_local` branch in feature code;
  the branch belongs in the `Target` impl. This is what the old project got wrong.
- **`Target::build_command` resolves to a *locally* spawnable argv.** For SSH it wraps in
  `ssh -tt host -- <shell line>`. Terminal code therefore always spawns a local PTY and
  never learns SSH exists. Terminal bytes must not travel through our RPC.
- **Never *resolve* `~/.ssh/config` ourselves.** Host aliases go to the `ssh` binary
  verbatim. That is the whole reason we shell out — `Match`, `Include`, `ProxyJump`, certs,
  FIDO keys and 2FA all come free. Windows is the sole exception (no ControlMaster →
  `russh`), and that seam stays inside `rmux-transport`.
  `rmux-ssh::config` reads that file for **enumeration only**, so the picker can list
  names; the `HostName`/`User` it surfaces are display hints and must never become the
  connection target. A test pins this.
- **Anything interpolated into a remote shell line goes through `shell_quote`.** The
  remote login shell re-parses it, so an unquoted path is an injection, not a cosmetic bug.
- **`dragDropEnabled` must be `false` for the webview to see a dropped file.** It defaults to
  `true`, and in that mode the native window swallows the drop and emits a Tauri event
  instead — so HTML5 `dragover`/`drop` handlers never fire and dragging a screenshot onto the
  Claude pane silently does nothing, while Cmd-V works. Turning it off is the right trade
  here: nothing uses Tauri's native drop, and the DOM event gives per-pane targeting for free
  — a window-level event would have to hit-test the drop position against every pane, which
  in a 4x4 grid is sixteen rectangles to get wrong.
- **Tauri plugin commands need explicit ACL grants; app commands do not.** Anything
  `plugin:*` (window dragging, dialogs, fs) must be listed in
  `src-tauri/capabilities/default.json` or it is rejected *silently* — the promise rejects
  with nothing surfaced. `core:default` notably omits `allow-start-dragging`, which is why
  the window could not be moved. `tests/window_drag.rs` guards the ones the chrome needs.
- **The askpass socket is a credential path.** `0700` directory, `0600` socket, and every
  request must carry the per-run token; otherwise any local process could phish a password
  dialog out of rmux. The token is passed explicitly into the `ssh` environment rather than
  inherited, because a security check that depends on two levels of implicit env
  inheritance fails open.
- **Remote filesystem records are NUL-separated.** A Unix filename may contain spaces,
  tabs and newlines — everything but `/` and NUL. Whitespace- or newline-delimited output
  corrupts real filenames, so `rmux-fs` uses NUL for both fields and records.
- **Reads are length-framed and a short read is an error.** Otherwise a connection dropped
  mid-transfer looks like a complete file, and the editor saves that truncation back.
- **Saving copies over the original; it never `mv`s onto it.** `mv` replaces the inode and
  silently discards permissions, ownership and hard links — a `0600` secret would become
  world-readable on save. Tests assert the mode survives on both paths.
- **Create and rename refuse to clobber.** Silent overwrite is data loss the user did not ask for.
- **Upload refuses to clobber with `set -C`, not with a check.** A `[ -e ]` test followed by
  a redirect is a race whose loser truncates a file nobody named; POSIX noclobber makes the
  redirect itself `O_EXCL`, so the refusal is atomic (`LocalFs` uses `create_new` for the same
  reason). The `[ -e ]` test stays only to tell "already there" apart from "the write failed".
  The name is chosen by the *file* rather than typed — a drop lands on whatever folder was
  under the pointer — so a collision is the likely accident, not the exotic one. Uploaded bytes
  go through **stdin**, and both failure paths `cat > /dev/null` first: without that the far
  side exits while we are still writing megabytes into the pipe, and the operator is shown a
  broken pipe instead of the reason. A live test sends invalid UTF-8 with an embedded NUL to a
  real host and compares base64 both ways; removing the guard was verified to turn it red.
- `alacritty_terminal` is pinned with `=` — it offers no stability guarantee across minor
  versions.
- **xterm ships an opaque terminal.** `allowTransparency` plus a transparent theme is not
  enough: xterm's own stylesheet paints `.xterm-viewport` solid `#000` ("required in order
  for the scroll bar to appear fully opaque"), and that element spans the whole terminal
  behind the rows. `signal-room.css` overrides it. Two rounds of changing rmux's *own*
  backdrops changed nothing, because the opaque layer was never ours.
  `ui/xterm-glass-check.html` asserts both halves — that xterm still ships the rule, and
  that the override clears it under both renderers.
- **Closing a session must kill its work.** Its shells and its Claude run under
  `rmux-agent` on the target so they survive quitting the app; dropping the session from the
  list alone leaves them running with nothing able to reach them. `removeSession` sends
  `terminal_close` per tab and `claude_end_session` for `claude-<id>`.
- **`Kill` and `List` are answered during the *handshake*, never after an attach.** `Kill`
  used to require a full attach first, and the kill client closes its connection
  immediately — so the daemon attached, tried to write the scrollback replay to a socket
  that was already gone, errored, and never read the frame. Every closed tab kept its shell
  while the client reported success. Verified broken and then fixed on a real host; a test
  drives `handle` end to end and asserts the *process* died, not just the map entry.
  Attaching to kill was wrong anyway: it created the session when the name was unknown, in
  order to destroy it.
- **An upgraded agent must *adopt* the old build's sessions, not duplicate them.** The
  fingerprinted socket correctly stops a new client inheriting an old daemon — but the other
  half was missing for weeks and it cost real work. A rebuilt agent could not *see* the
  sessions the previous daemon still held, so it created a second Claude under the same name
  while the first kept running, orphaned and unreachable by anything. Measured on a live host:
  one session name existed **three times** across three daemons, the oldest 27 hours old and
  still detached; the operator experienced it as "I came back and all my sessions were
  interrupted", and every agent rebuild did it again.
  `attach` now asks its own daemon first, then every other binary in `~/.rmux/bin`, and
  **`exec`s into the build that owns the name** (`hand_off`) — exec rather than spawn, because
  this process *is* the far end of `ssh -tt` and owns the pty. `RMUX_AGENT_HANDOFF` stops two
  builds bouncing a session between them forever. `kill` resolves the same way or closing a tab
  leaks the shell it claimed to close, and `list` unions every daemon — the orphans worth
  finding are exactly the ones an upgrade left behind, which a single-daemon listing cannot see.
  Proven red-then-green by a test that copies the binary under two installed names, and on a
  real host: the new build attached to an existing session and **never started a daemon of its
  own**.
- **Tests in `persistence.rs` hold a `SERIAL` mutex.** They spawn real daemons and tear them
  down with `pkill`, which is process-wide — run in parallel each one's cleanup lands inside the
  other's run, and they pass alone while failing together.
- **A fingerprinted socket name is longer than `agent-<version>`, and that matters.** The
  handoff test had to move off `temp_dir()` to a short `/tmp` home: macOS's deep
  `/var/folders/…` plus a fingerprint crosses `sun_path`, `bind` fails with an error that never
  mentions length, and the shell simply never starts.
- **A fix that lives in the agent is not deployed until a *new daemon* runs it.** Three
  things all have to happen: `scripts/build-agents.sh` rebuilds the musl binaries,
  `pnpm tauri build` embeds them as resources, and the host must start a daemon from the new
  binary. Skipping any one leaves the old behaviour with nothing looking wrong. When a remote
  fix appears not to work, check `ps -eo pid,lstart,args | grep rmux-agent` on the host and
  compare the *daemon's* binary against the client's before touching the code again.
- **`rmux-agent list` is what makes a leak findable.** Without it the only way to discover an
  abandoned shell is `ps` on the host and correlating by hand, which means nobody does.
  It reports name, pid, age and whether anything is attached — age and attachment are what
  separate "left behind" from "rmux is merely closed". Dead sessions are excluded: their pid
  may already have been reused, and reporting one sends the operator to kill something else.
- **The daemon socket carries the build, not just the version** — and the version-only form
  was a real, expensive bug rather than a rough edge. Every dev build is `0.1.0`, so
  `agent-<version>.sock` meant a rebuilt agent installed correctly under its own
  fingerprinted path, ran as the *client*, and then connected to the previous build's
  daemon. **The daemon is what spawns the shell**, so it kept using its own compiled-in code
  and the fix never ran. Measured on a real host: a client from 03:00 attached to a daemon
  from 09:58 the day before, and `command not found: claude` survived three rebuilds while
  every visible artefact — binary present, fingerprint correct, fix genuinely inside it —
  looked right.
  The socket name is now the running binary's own file name (`ipc::socket_stem`), which
  `provision` already stamps with the content fingerprint. A changed agent starts its own
  daemon and cannot inherit an older one; the old daemon keeps serving its live sessions
  until they end, because upgrading must not kill work in progress.
  **`file_name`, never `file_stem`.** The installed name contains dots, so `file_stem` reads
  `.0-<fingerprint>` as an extension and discards it — collapsing every `0.1.x` build back
  onto one socket. That very mistake shipped into the first version of this fix and was
  caught only by looking at the socket on the host; a test now pins it.

## Windows hosts

**rmux drives Windows through Git for Windows' bash, and that is the whole design.**
Every remote operation here is a POSIX script — `for e in *`, `grep -rInZ`, `cat >` — which is
what lets any `ssh`-reachable host be usable with nothing installed. OpenSSH for Windows hands
the remote command to `DefaultShell`, and unset (the shipping default) that is `cmd.exe`, which
cannot parse a word of it. Measured on a real Windows 11 host: no `sh` on `PATH`, no `bash` on
`PATH`, and `uname -s` — the *first* thing rmux runs — fails, so the connection never completed.

`crates/rmux-ssh/src/winshell.rs` finds the POSIX shell and routes through it. The branch lives
in the `Target` impl, never in feature code: `rmux-fs`, search, terminals and Claude all still
emit ordinary POSIX and know nothing about it.

- **A failed `uname` is the Windows signal, not an error.** That is the detection.
- **The script is base64'd into the command line.** `cmd` re-parses it, so `"`, `|`, `>`, `&`
  and `%` are all live; base64's alphabet contains nothing it reacts to. A test asserts no
  fragment of a hostile script survives into the line, and that `%` never does — `cmd` expands it.
- **The decode goes to a file, never a pipe.** `echo <b64> | base64 -d | bash` gives the inner
  shell the *pipe* as stdin — so saving a file and uploading, which both stream their payload
  over stdin, would have received the script instead of the data. Silent corruption.
- **`SHELL` is corrected to `$BASH`.** Windows sets `SHELL=/c/windows/system32/cmd.exe` and MSYS
  leaves it there, so `CommandSpec::login_shell()` (`$SHELL -l -i`) would have launched **cmd**
  through a POSIX wrapper — every terminal and every Claude session. Verified: `/usr/bin/bash`.
- **The shell is cached per host, not per `SshTarget`.** Several targets exist for one host and
  only one calls `connect`; held per instance, the others assumed POSIX and every command failed
  with `'sh' is not recognized`. That is exactly what the first live run did.
- **Short 8.3 paths** (`C:\PROGRA~1\Git\bin\bash.exe`) so no space needs quoting inside a
  command line `cmd` is already re-parsing.
- **MSYS emulates procfs, so metrics work** — `Platform::has_procfs()` includes Windows. But the
  emulation is partial: `/proc/net/dev`, `/proc/loadavg` and `/proc/uptime` are absent, and
  unguarded the missing one exited non-zero and took the *whole* sample with it, reporting a
  host whose CPU and memory were sitting right there as a failed connection. Every optional
  reading is guarded. `MemAvailable` is also absent, which made an idle 134 GB machine read
  **100% memory used**; it falls back to `MemFree`.
- **The agent does not run on Windows** — it is cross-compiled for linux-musl. `ensure_agent`
  refuses with a reason and callers fall back to a direct login shell, because refusing outright
  would mean no terminal at all on a host where files, search and Claude all work. What is lost
  is persistence. A *model profile* is the exception and fails loudly: it delivers credentials
  through the agent, and running the conversation against Anthropic while the UI named a
  different provider is the silent mis-routing that design exists to prevent.
- `tests/live_windows.rs` drives all of it against a real host — a filename containing `&` and
  `%PATH%`, a binary upload over stdin, search, and the clobber guard.

## Release notes are for the people installing it

Written for a user opening the releases page, not for the person who wrote the
code. The first version of the 0.2.10 notes led with cascade layers, Tauri
sidecars-versus-resources and a notarisation submission id — all true, all the
reason *this* was hard, and none of it a reason for anyone to be pleased. It
was rewritten.

- **Lead with what they can now do, and say where it is.** "Settings ›
  Appearance › TEXT SIZE", "click HOST on any server". A feature nobody can find
  is a feature nobody has; the location is the difference between a changelog
  and an announcement.
- **Say what it replaces and why the old way was worse**, briefly. "Interface
  scale moved everything, so turning it up made the app look stretched" tells
  someone whether the entry is about them. A bare "added text size setting" does
  not.
- **Never make the bug report the headline.** Fixing our own regression is not
  news to a user who never saw it; the *capability* is. Fixes that only restore
  intended behaviour go in a short "Also in this release" list, or nowhere.
- **No internal vocabulary.** No file names, no crate names, no submission ids,
  no `@layer utilities`. That reasoning belongs in the commit message, which is
  where it already is — and commit messages are the right place precisely
  because the two audiences want opposite things.
- **End with what to download and what happens to their data.** Which artefact
  is which platform, whether settings carry over, and anything not built yet.
- **Mark it latest** (`gh release edit <tag> --latest`). A release nobody is
  pointed at is a release nobody installs, and `tauri-action` does not do it.

## Shipping rmux *for* Windows and Linux

The section above is about Windows as a **target**. This one is about rmux **running** there,
which is a different problem with different answers, and conflating the two wastes an afternoon.

**The app cannot be cross-compiled; the agent can.** Tauri links each platform's own webview —
WebKitGTK, WebView2, WKWebView — and those are native system libraries with native toolchains.
`.github/workflows/release.yml` therefore builds the app on the target OS. The *agents* are
built once on macOS with zig and handed to every runner, because they are uploaded to hosts and
have nothing to do with which machine the app runs on. Building them per-runner would give three
fingerprints for one release, and the fingerprint is what decides whether a host starts a new
daemon.

- **A stub is an API mirror, and nothing on this machine compiles it.** `rmux-control`'s stub
  had been wrong since the day it was written — `handle` declared as a hand-rolled
  `-> impl Future` against an `#[async_trait]` trait (an `async fn` in an impl carries a
  lifetime bound to `&self`, so E0195), plus `publish` for `emit` and `socket` for
  `socket_path`. No macOS or Linux build reaches the file, and the Windows build had been dying
  in an earlier crate, so four errors sat there invisibly until a *different* fix let the build
  get far enough to reach the consumer. Change `server.rs`'s public surface and change the stub
  in the same commit — and check it with the Windows target below, because nothing else will.
- **A Unix-socket feature gets a stub on Windows, never a half-port.** `rmux-control::server`
  still is one: `#[cfg(unix)] pub mod server;` beside a `#[path = "server_stub.rs"]` twin whose
  `start` bails with a reason. Nothing in the workbench depends on it.
- **`rmux-ssh::askpass::server` was that stub too, and the stub's premise was false.** It rested
  on "password and 2FA hosts fail fast with a reason instead of hanging". Measured against a real
  password host from a real Windows machine, what actually happens is:

  ```text
  $ ssh -T root@host uname -s      # stdin a pipe, SSH_ASKPASS_REQUIRE=never
  Permission denied, please try again.
  Permission denied, please try again.
  root@host: Permission denied (publickey,password).
  ```

  `ssh` takes a password from a terminal or from an askpass helper, and a command rmux runs has
  neither — so it burns its retries against nothing, on **every** command. And because Windows
  OpenSSH has no `ControlMaster`, every file listing, metrics sample and status poll is its own
  connection and its own authentication: rmux generated failed logins at roughly one every two
  seconds. That is not a degraded host, it is an unusable one — and it is enough to get the
  machine banned by `fail2ban`, which reads afterwards as "the server is down".

  It is now a **named pipe** (`askpass/server_windows.rs`), same wire protocol, same token.
  The stub's real objection is answered rather than ignored: `CreateNamedPipe` with a NULL
  `lpSecurityAttributes` grants, per Microsoft, "read access to members of the Everyone group and
  the anonymous account" — exactly the fail-open a naive port would have shipped. Every instance
  is created with an explicit `D:P(A;;GA;;;<current user SID>)`, and a test asserts the SID is a
  real user rather than Everyone or Administrators. `reject_remote_clients` matters too: a named
  pipe is reachable over SMB by default, and a credential dialog anyone on the LAN can summon is
  not a bridge, it is a hole.
- **On Windows a credential is remembered for the run; everywhere else it is not.** This is the
  same guarantee `ControlMaster` already gives — authenticate once per host per run — implemented
  where there is no `ControlMaster`. Without it, bridging prompts to the UI would only replace "a
  password host cannot connect" with "a password dialog every second", which is not an
  improvement. `src-tauri/src/askpass.rs`'s `Memory` also **coalesces** identical prompts, because
  opening a session connects a terminal, the file tree and metrics at once and three identical
  dialogs reads as a broken app. A repeat inside one second is treated as `ssh` refusing the
  remembered answer — refused passwords are re-prompted immediately, while the fastest poller in
  the app is 1.5s — so a typo costs one more dialog rather than failing every command until
  restart.
- **Every child process is spawned with `CREATE_NO_WINDOW`** (`rmux_transport::NoConsoleWindow`,
  enforced over the source by `src-tauri/tests/no_console_window.rs`). rmux is a GUI process and
  therefore has no console, so Windows gives each console-subsystem child — `ssh.exe` above all —
  a brand new *visible* one. With no multiplexing that is a `cmd` window flashing over the
  operator's work every couple of seconds, forever. The rule is enforced syntactically because it
  is invisible to everyone who could catch it: it does not exist on macOS or Linux, it is not a
  compile error, and no test that runs a command can see it. Terminals are exempt and must stay
  exempt — they go through ConPTY, where the child attaches to a headless pseudoconsole and there
  is no flag to set.
- **`userpath::adopt_login_path` returns early on Windows.** It exists for launchd stripping a
  Finder-launched `.app`'s environment, which has no Windows equivalent, and every assumption in
  it is POSIX: `$SHELL`, `-l -c`, and a `:` separator. Git for Windows users routinely have
  `SHELL` set, so it would run `bash -l -c` , get MSYS paths back, and join them to the real
  `Path` with colons — writing a `Path` no Windows process can parse, after which `ssh` itself is
  unfindable. That reads as "rmux cannot connect to anything", not as a corrupted `Path`.
- **Compile the Windows target on this Mac before spending a CI cycle on it.**

  ```sh
  brew install mingw-w64        # tauri-winres shells out to x86_64-w64-mingw32-windres
  rustup target add x86_64-pc-windows-gnu
  cargo-zigbuild check --workspace --target x86_64-pc-windows-gnu
  ```

  `cfg(windows)` is identical for the gnu and msvc targets, so this exercises every path CI
  does. **Use the gnu target, not msvc**: plain `cargo check --target x86_64-pc-windows-msvc`
  cannot build `ring`'s C (no MSVC headers), which excludes `rmux-claude` — and it never gets to
  `src-tauri` at all, because `tauri-winres` needs a resource compiler. Both exclusions matter:
  the second round of Windows errors was *entirely* inside `src-tauri`. zig supplies the C
  compiler and mingw the resource compiler, and the whole workspace then checks in seconds.

  One CI round trip is twenty minutes and finds one crate's worth of errors. This finds all of
  them at once, which is the difference between two attempts and five.
- **`tauri.conf.json` carries no `version`.** Tauri falls back to `Cargo.toml`, and two copies is
  how `rmux_0.1.0_amd64.deb` came to be published under a release tagged `rmux-v0.1.1`. The
  version an installer's *filename* claims is the one thing a user checks it by.
- **The workflow checks out the tag, not the branch a dispatch ran from.** `workflow_dispatch`
  checks out whatever ref was selected while publishing under the `tag` input, so the two can
  disagree — and did: `rmux-v0.1.1` names a commit two fixes older than the branch built against
  it, so the release's macOS dmg and its would-be Linux bundles came from different source.
- **`max-parallel: 1`, and that is not about runner cost.** `tauri-action` creates the release
  when it cannot find one, so run together both jobs look, both find nothing, both create, and
  the loser takes a 422 and drops its installers on the floor.
- **The job needs `permissions: contents: write`.** The default workflow token here is read-only.
  Without it Linux bundled a `.deb`, an `.rpm` and an AppImage and then died on the first upload
  with "Resource not accessible by integration" — which reads as a packaging failure and is not
  one. The build being green up to the last line is precisely what makes it misleading.
- **CI does not build macOS**, and must not be *made* to. It has no Developer ID certificate, and
  `tauri-action` fails trying to import one rather than falling back to unsigned. macOS is built,
  signed and notarised on the developer's Mac by `scripts/release-mac.sh`; CI opens the release
  as a **draft** so that dmg can be added before anyone publishes it.

## Managing the host

- **A pid crosses the IPC bridge as a `u32`.** This is the one place the operator points at
  something on a machine and says "end that", so the argument must not be able to become a
  shell fragment with a `kill` in front of it. Typing it out of the wire beats quoting it.
  `0` and `1` are refused: `0` signals the whole process group, which would take out the
  operator's own session.
- **A kill reports its exit status, not just its output.** `kill` exits non-zero *and*
  explains itself on stdout, so reading only the text — through `stdout_or_err`, which
  refuses a non-zero status and yields nothing — reported a clean success for every failure.
  On a shared dev box the usual failure is "Operation not permitted", and swallowing it makes
  the row read as a process ignoring TERM rather than one never signalled. A test pins this
  and fails when the check is reverted.
- **`TERM` is the button and `KILL` is a separate, second choice.** `KILL` gives the process
  no chance to flush or clean up, which on a dev host means a corrupted build or a stale lock.

## Pasting an image into a remote Claude

Claude Code reads images off the clipboard itself, which cannot work when it is on a server:
that machine has no clipboard and no route to yours. So the bytes travel — the image is
written to `~/.rmux/pastes` **on the target** (`0700` dir, `0600` file, because a pasted
screenshot is frequently of something private), and its *path* is typed into the prompt.
Claude reads a file you mention, which it can already do, identically local or remote — so
there is no `if is_local` here.

The bytes go through **stdin**, never argv: a screenshot is routinely a megabyte, base64
inflates it by a third, and `ARG_MAX` caps a single argument at 128 KiB, so an argv version
works for an icon and fails on anything real. Verified against a real host — same md5, `600`,
recognised as a PNG. Like a browser report, the path is *typed* into the composer and never
submitted.

## Design system — SIGNAL ROOM

`ui/src/styles/signal-room.css` is the source of truth. Four rules, all load-bearing:

-1. **Text must be legible.** `--text-faint` was `#5c5953` — 2.77:1 against the panel, well
   under the 4.5:1 normal text needs, and most of what wears it is 9px labels. It is now
   `#7e7b74` (4.57:1), still a clear step below `--text-soft` (6.46:1). Measure before
   choosing a grey; prose belongs at `--text-soft`, not `--text-faint`.
0. **Red (`#e63b2e`) only where the operator must act.** Working/done stay monochrome;
   in-progress is amber. Red that means three things means nothing.
1. **Zero border-radius**, enforced by a global `!important`. `.round` is the only escape
   hatch, for genuinely circular instruments.
2. **Blinking is for cursors only.** Liveness = data movement. Meters "breathe" by scaling
   the *bar*, never the printed number — animating a value invents data.
3. **No emoji.** Inline SVG, Lucide-style, 1.5–1.7px strokes, square caps.

**Panels tint; they must not `backdrop-filter`.** This one reads backwards and cost two
wrong diagnoses. `backdrop-filter` filters *the page's own backdrop*, and the desktop behind
this window is not page content — it is a macOS `underWindowBackground` NSVisualEffectView,
behind the webview. The blur therefore sampled an empty transparent page, which WebKit
composites to an opaque dark field that the panel then tinted on top of: a solid app inside a
genuinely translucent window, unreachable by any Appearance setting. Removing it brought the
wallpaper back; restoring it took it away again with the scrim thinned and the tint lowered.
The blur we want already happened, in the compositor, before the webview drew anything. The
cost is real and accepted — there is no per-panel frost, so the whole window shares one
material. There is no Frost setting either, because it drove `--app-blur` and would now be a
dead knob. Stored tints are clamped: a 64% saved against the old opaque blur layer looks
nearly solid without it. Fonts (SFU Futura, IBM Plex Mono) are bundled, never
fetched from a CDN; this app must look right with no network.

**The terminals are part of the appearance system, not exempt from it.** They were the one
surface it did not reach: `TERMINAL_THEME.foreground` was `#e8f4f2`, a cooler and brighter white
than the app's own `--text` (`#e8e6e1`), so the terminal and Claude panes read as whiter than
everything around them *at default settings* — and a chosen text colour changed every surface
except the two an operator looks at all day. `brightWhite` was pure `#ffffff`, which a TUI uses
for bold constantly, making the brightest thing on screen brighter than the design system allows
anywhere else. `terminalTheme()` now derives them from the live tokens, and both hosts push the
new palette into xterm on the appearance channel — xterm does not re-read CSS on its own, so
without that they keep the colours they were constructed with.

**The operator may replace the backdrop, the type scale and the two colours.**
Background is `desktop | color | image`; the picture is copied to
`$APPDATA/backgrounds` by `background_set` and served over the asset protocol,
**never stored as a data URL** — a wallpaper is megabytes and `localStorage` is a
single shared quota that also holds the session list, so overflowing it costs
someone their sessions to gain a picture. A painted background and native glass
are mutually exclusive by physics: glass refracts what is behind the *window*,
which a background inside the page covers. Interface scale is `zoom`, not a font
size — the type is 157 hard-coded pixel values, so a font variable would move
almost nothing and read as a broken control. `--text-soft` and `--text-faint` are
*derived* from a chosen `--text` rather than being three pickers, because the
three-level ramp is load-bearing and three independent colours is three chances
to invert it. `--primary` is a bare `r g b` triplet, never a hex: every use is
`rgb(var(--primary) / <alpha>)`.

**Interface scale is `zoom` on `#root`, uncompensated, and viewport units are banned under
it.** Three separate mistakes were made here and each one looked like a layout bug:

- **On `#root`, never `:root`.** Zooming the document element scales the root box while its
  `height: 100%` still resolves against the *unscaled* viewport, so the app was laid out into
  a fraction of its own window and clipped at every scale but 100%.
- **No `calc(100% / zoom)` compensation.** WebKit implements the *standardised* `zoom`, where
  percentages already resolve inside the zoomed coordinate space — so compensating divides a
  second time and leaves a band of desktop down the right and bottom edges. Measured, because
  Chrome still uses the legacy behaviour and gives the opposite answer:
  `viewport 1712x931 · width:100% → 1695x931 · calc(100%/1.09) → 1555`. `ui/zoom-check.html`
  reproduces it; run it in **Safari**, not Chrome.
- **`h-full`, never `h-screen`.** Viewport units resolve against the real viewport and are not
  scaled by `zoom`, so a `100vh` box renders taller than the window containing it and the
  sheet overflows its own frame. `#root` is exactly the window, so filling the parent is both
  correct and scale-proof.

**The Settings window is never scaled** (`--ui-zoom` is pinned to 1 there). It carries the
control, and a control that resizes itself as it is dragged slides out from under the cursor
and outgrows its own window. The workbench is visible while Settings is open, so the effect is
watched on the thing being configured rather than on the instrument doing the configuring.

**Real glass is native, and the class is looked up rather than linked**
(`src-tauri/src/glass.rs`). The corollary of the paragraph above is that glass cannot come
from the page at all — you cannot refract light you were never handed. macOS 26's
`NSGlassEffectView` is the compositor doing it properly, and it is the only version of this
that can work. Two things must stay true:

- **`AnyClass::get(c"NSGlassEffectView")`, never `NSGlassEffectView::class()`.** `objc2`
  resolves classes with `objc_getClass` and **panics** (`class_not_present`) when one is
  missing — so on macOS 15 the typed path would take the app down the moment glass was
  applied. Measured: the shipped binary has *zero* undefined `OBJC_CLASS` symbols, so this is
  a runtime panic and not, as first assumed, a dyld load failure. Either way the lookup
  returning `None` is the entire version check, and it is what makes this a fallback rather
  than a crash. The generated bindings are still used for the instance methods, which message
  the object and never name the class.
- **The vibrancy is hidden, not removed.** Two materials both sampling behind the window is
  twice the frosting and reads as a solid panel — the exact failure CSS glass had. Hiding
  Tauri's `NSVisualEffectView` means switching glass off restores the shipped material for
  free.

It is **one sheet behind the whole window, not per-panel glass**, and that is settled: glass
is an `NSView` and every panel is HTML in one webview, so per-panel would mean sixteen native
views chasing DOM geometry and wrong for a frame on every resize. Off by default even where
supported — an app that changes its own appearance on an OS upgrade is unsettling.

**With native glass on, the page stops simulating glass at all.** Not "less", none: the
vignette, the tint gradient and the layered opacities all sat *above* the real material, and
a film above an effect can only hide it. `:root[data-native-glass="on"]` reduces the scrim to
the registration grid and repoints `.panel`/`.inset` at **`--glass-overlay`**, a knob that is
deliberately *not* `--panel-tint`: under real glass that background is no longer a material,
it is legibility residue, a different quantity with a different right answer. Settings shows
whichever of the two applies and never both, because the other is a visibly dead slider — the
mistake the Frost setting already made. `.window` and `.menu` are **not** thinned: they stack
over page content, so their opacity was never a glass simulation, and a see-through dialog
over a photograph is worse than a plain one. `applyAppearance` memoises the glass options it
last sent, because the overlay slider calls it on every tick and each call crosses IPC to
mutate AppKit views on the main thread for every open window.

## Code is colour-coded, and that is not a break with rule 0

The Monaco theme was near-monochrome on purpose — the reasoning being that code sits in a
dense instrument panel and a loud syntax theme would out-shout the controls. It was wrong, and
a screenshot of a real Python file made it obvious: `keyword` and `identifier` were **both**
`#e8e6e1`, so the file rendered as one flat grey with a slightly dimmer comment. Syntax colour
is not decoration, it is the parse; unhighlighted code means doing the tokenizer's job by eye
on every line.

Rule 0 survives intact — red still appears nowhere in the editor but genuine errors, unmatched
brackets and the caret. Syntax colour is a *different axis* from the alarm palette, and
conflating them was the error: suppressing every hue to protect one of them left the app unable
to say "this is a string".

- **The six hues are the terminal's hues**, taken from `TERMINAL_THEME` verbatim, so a string
  is the same green in the editor, in a transcript code block and in a shell. Three surfaces
  disagreeing about what green means is three palettes to learn.
- **`monaco.editor.colorize` returns class names, not colours.** It emits
  `<span class="mtk21">`, and the stylesheet that gives `.mtk21` a colour is injected by
  Monaco's theme service **only when an editor is constructed**. A transcript rendered before
  any file was opened therefore produced perfectly tokenized HTML that painted in one inherited
  grey — the exact symptom of no highlighting at all, from code that was working.
  `ensureThemeStyles` builds one throwaway off-screen editor to trigger it; the stylesheet
  outlives its disposal (measured, both halves).
- **`ui/highlight-check.html` counts the colours that actually *paint*** — `getComputedStyle`
  on the rendered spans, never a regex over the markup. Checking the string would have passed
  the entirely-grey output, which is the bug. It measured 0 distinct colours before the fix and
  7 after.
- **Transcript code blocks go through the same tokenizer** (`lib/markdown-code.tsx` →
  `CodeBlock`), so there is one definition of what highlighting is. A second highlighter would
  be a second palette and a second set of language rules, disagreeing within a week. Monaco
  escapes the source as it tokenizes, which is what makes injecting that HTML safe for text
  rmux did not write. An unknown language stays plain — guessing produces confidently wrong
  colours, and a shell transcript tokenized as JavaScript is worse than no colour because it
  *looks* parsed.

## The session rail is the answer to "which machine needs me?"

- **A running session turns; it does not blink.** A static amber dot cannot distinguish work
  still going from work that stopped an hour ago, which is the single most useful thing the
  rail can say. Rule 2 holds — the ring rotates continuously and the dot never disappears.
- **"Finished" is a transition, not a state.** `idle` cannot tell a session that just finished
  ten minutes of work from one that has never run, and those deserve opposite amounts of
  attention. `setStatus` records `finishedAt` on the working→not-working edge; the rail marks
  that session in the accent colour until it is **opened**. Cleared by looking, never by a
  timer: an alert that expires on its own is one the operator misses by being away from the
  desk, which is exactly when a long run finishes. Runtime only — restoring it would greet
  every launch with a rail full of marks that have already been seen.
- The header counts **both** kinds of "needs you". Counting only `waiting` left a rail full of
  accent dots above a header claiming everything was fine.

## Progress — counting the day without inventing it

`ui/src/lib/activity.ts` is the only place rmux keeps a tally of its own, and everything on
the Progress page is derived from it or from notes. The rule it exists to serve is that a
number nobody measured must be **absent, not estimated** — a dashboard that fills a gap with
something plausible cannot be told apart from one that measured it.

- **Only two things are stored, because only two have no other record.** Terminal commands
  (a shell's history belongs to the shell, and reading it back over SSH per refresh is a poll
  of someone else's file) and attention time (nothing else knows which pane you were looking
  at). Prompts, tokens and timings come from Claude's own transcripts, which go back further
  than any counter we could start.
- **Task completions are recorded as they happen.** A note stores only the *current* state of
  its checkboxes, so "tasks finished on Tuesday" is unknowable from the notes alone and any
  chart over time had to either invent it or not exist. Un-ticking decrements, so the count
  stays a description of the board and not a score that only goes up.
- **The hour bucket is written at the moment time is banked, never derived later.** Deriving
  it on read gives whatever hour the dashboard happened to be opened in, which piles the whole
  day into one column — a chart that is confidently, silently wrong. Five seconds of
  misattribution once an hour is the accepted cost of not splitting every tick.
- **Both dimensions are pruned on every write.** `days` was bounded and `hours` was not, at
  first; a store bounded in one dimension reads as bounded right up until the `localStorage`
  quota it shares with the session list runs out. `ui/activity-hours-check.ts` pins this and
  was verified red before the prune existed.
- **Peak hour is `undefined` when nothing is recorded, never `0`.** Hour zero is a real
  answer, so a falsy default tells someone who works late that their busiest hour is the one
  they never worked.
- **Empty hours and empty days are drawn, not skipped.** Closing the gaps turns a week off
  into a continuous run and puts lunch next to midnight.
- **A goal of zero means "not tracking this"**, rather than a second switch beside every
  field. Nobody holds a target of zero tasks, so the value carries its own off state.

## Jira — what rmux may say about someone else's system

`ui/src/lib/jira-focus.ts` stores **selections only** — issue keys. Status, summary and
category are read back from the board every time, never cached, because they are edited by
other people in another system all day; a cached status is a claim rmux is not in a position
to make.

- **Done is Jira's `statusCategory`, never a status name.** "Shipped", "Closed" and "Ready
  for QA" are all real done-column names, and a status *called* "Done" in an unfinished
  category is a real configuration too. Names are per-project and renameable, so a bar built
  on them is right on the board it was written against and quietly wrong everywhere else.
  Both directions are pinned in `ui/jira-focus-check.ts`.
- **Two stores, because they are two different facts.** Which ticket a *session* is on has no
  date and must survive the night — it is a property of the work, like the model profile.
  What you meant to finish *today* must expire, or Tuesday's list shown on Friday makes the
  progress bar meaningless. Folding them together gets one of the two wrong whichever way you
  fold.
- **Transitions are asked for, never assumed.** The buttons are literally what
  `/transitions` returned for that issue at that moment. A hard-coded To Do → In Progress →
  Done produces controls that fail on any board with a customised workflow, which is most of
  them.
- **Finishing a ticket prompts in place, not in a modal.** A dialog appearing over the
  workbench takes the keystrokes of whoever is typing in a terminal. The panel opens inside
  the widget, where the change happened, and waits. It fires on the *observed* transition to
  done — so a ticket somebody else closed in Jira prompts here too, which is the case the
  operator is least likely to notice unaided — and never merely because the app opened on a
  session already pointed at a finished ticket.
- **rmux cannot create an issue, and does not pretend to.** The server exposes
  `POST /agency/missions/:key/transitions` and `/comment` and nothing that creates; adding one
  means a new route in `server/` *and* a deploy. So the picker offers Jira's own create screen
  in the real browser instead of a button that would fail — never show a control that cannot
  work.

## Keyboard shortcuts, in an app made of terminals

`ui/src/lib/shortcuts.ts`. The governing constraint is that rmux is mostly two full-screen
programs reading raw keystrokes, so **anything a shortcut swallows is a key the shell never
sees, and nothing anywhere says why**.

- **A binding without a modifier is refused at the point of binding**, with the reason shown.
  Shift does not count — `Shift+a` is `A`, and taking capitals is the same bug wearing a hat.
- **`keydown` bubbles; it does not capture.** Capture would beat xterm to *every* key, because
  deciding whether a chord is bound means running the match first. Bubbling means xterm acts
  and rmux only intervenes on chords it does not use. `preventDefault` fires only on a match.
- **xterm's textarea is deliberately not a "typing target".** It holds focus the entire time a
  terminal is on screen, so exempting it — the obvious reading of "don't fire while typing" —
  would disable every shortcut in the app's main view. Ordinary `input`/`textarea` outside
  `.xterm` do suppress.
- **Chords are normalised before comparison** (`Mod`, `Ctrl`, `Alt`, `Shift`, key). Without it
  `Alt+Mod+K` and `Mod+Alt+K` are two bindings that fire on one keystroke and the conflict
  detector reports all-clear.
- **`Mod` is ⌘ on macOS and Ctrl elsewhere**, and on macOS Ctrl stays a *separate* modifier —
  counted twice off macOS, no `Mod` binding would ever match.
- **Grid movement clamps, never wraps.** Wrapping "left" at the left edge of a 4×4 lands six
  tiles away, and nothing on screen suggests those are adjacent.
- **Conflicts are marked, not prevented.** Mid-rebind, two actions sharing a chord is a state
  you pass through.
- **Defaults avoid ⌘W/⌘Q/⌘N/⌘M** (macOS binds them in the app menu) and use `Mod+Alt+Arrow`
  rather than `Mod+Arrow`, which is line-start/line-end in every macOS text field.
- **A pane's sub-view stays local to the pane**; the shortcut is a `rmux:set-view` event
  addressed by session id. Putting it in the persisted workspace would restore someone into a
  transcript they closed days ago, and an unaddressed one would switch all sixteen tiles.
- **The defaults are per-platform, because `Mod` unifies the modifier and not its
  consequence.** This was assumed to follow from `Mod` and does not, and it is what made the
  bubble-phase rule above correct on macOS and wrong everywhere else. ⌘ is free in a terminal —
  xterm encodes nothing for it — so "the terminal acts first, we take only what it does not
  use" costs nothing there. Off macOS `Mod` is **Ctrl**, which is exactly what a terminal
  claims, and `preventDefault` on the bubble is far too late: xterm wrote to the pty from its
  own handler frames earlier. A colliding default does not lose, it fires **both**.
  Measured against a real xterm, **eight of the ten macOS defaults** put bytes on the wire when
  `Mod` is Ctrl: `Mod+3` → `\e` (cancels Claude's prompt), `Mod+4` → `\x1c` (the quit
  character), `Mod+T` → `\x14`, `Mod+P` → `\x10` (previous command in every shell), and the
  four `Mod+Alt+Arrow` → `\e[1;7A..D`. Off macOS they are `Mod+Shift+…`, the Windows/Linux
  terminal convention, measured free.
- **Off macOS the grid moves on `H/J/K/L`, because no arrow chord is free.** xterm encodes
  *every* modifier combination of an arrow — `\e[1;3D`, `\e[1;4D`, `\e[1;6D`, `\e[1;7D` were
  all measured taken — so there is nothing to pick. Letters are the only free directional set.
- **`reachesTerminal` marks a rebind that collides (REACHES SHELL); it is a model of xterm's
  encoder and is pinned against the real one.** A model can drift on an xterm upgrade, silently
  and in the direction that matters — a false all-clear costs a key in every terminal forever,
  while a false warning costs one chord. So it is deliberately conservative, and
  `shortcut-terminal-check.ts` asserts it never says "free" about a chord a real xterm takes,
  across 114 bindable chords. Two gaps surfaced exactly that way: **`Ctrl+Shift+Enter` still
  sends `\r` and `Ctrl+Shift+Tab` still sends `\e[Z`**, so Shift is not a blanket escape hatch —
  it lifts a *letter* out of the control-code table and does nothing for a key that has its own
  encoding.
- **The marker is shown off macOS only.** The one chord it would flag there is `Mod+Alt+Arrow`,
  four shipped defaults, whose behaviour turns on xterm's `macOptionIsMeta` and has **not** been
  measured. An unverified warning over defaults that platform ships and tests teaches the
  operator to ignore the marker, which costs more than the marker is worth. Worth measuring on
  a Mac; if it collides there too, the macOS defaults need the same treatment.

## Searching a project

**`grep` runs on the machine that owns the disk** (`crates/rmux-fs/src/search.rs`). Listing
directories and reading each file is one round trip per file, which over SSH is unusable on
any real checkout — a few thousand files is a few thousand connections' worth of latency even
with ControlMaster holding the socket. Only the matches cross the network.

- **Records are NUL-delimited** (`grep -Z`), for the same reason listings are: a filename may
  contain newlines, and splitting the stream on `\n` would report files that do not exist.
  Read to the NUL for the path, *then* to the newline for `line:text`.
- **`-I` and `--exclude-dir`.** Without `-I` a hit inside a `.png` returns raw bytes; without
  the excludes, one `node_modules` outweighs the project it sits in and the first page of any
  search is dependency source, which reads as broken.
- **`-F` unless a regex was asked for**, or a query full of `.` and `*` matches everything.
- **`|| true`.** `grep` exits 1 when nothing matched — an answer, not a failure. Without it
  the most ordinary outcome there is surfaces as an error.
- **Bounded at 500, and the UI *says* it truncated.** A list that stops at a round number
  reads as the complete answer.
- **⌘F is handled by us only when Monaco does not have focus.** Monaco answers ⌘F itself, but
  only when focused — from the tree, or right after clicking a search result, the key did
  nothing, which reads as a missing feature rather than a focus rule.

## A widget that is switched off must not run

Instruments are choosable (rail header › INSTRUMENTS), and *off means off* — the widget is
unmounted, not hidden, so its effects and timers stop with it.

- **The shared host poller is gated too.** `useHostSample` lives in the rail, above the
  instruments, so unmounting HOST and TOP PROCESSES would not have stopped it: it would have
  kept opening an SSH channel every couple of seconds for readings nothing rendered. A setting
  that hides a widget while it keeps working is not a setting.
- **`enabled` is in the effect's dependency list**, not only in its early return. Without it the
  poller never restarts when an instrument is switched back on, and the disable switch becomes a
  one-way door — the widget reappears and stays permanently empty.
- **Absent preference means all of them.** A first run must not open to an empty rail, and the
  stored set is reconciled against `INSTRUMENTS` like the order is, so a widget added later
  appears rather than being silently off for anyone who has ever touched this.

## The interface must never make anyone feel lost

The operator should be able to use rmux on instinct — without reading labels to
work out what a control does, and without ever wondering whether the app has
stopped working. These are not aspirations; each one is a rule with a test.

- **Never leave a blank screen.** A pane that is loading must *say* it is loading,
  what it is loading, and from where. This was violated exactly: the transcript
  rendered `{!entries.length && !loading && …}`, so during the initial read — the
  one case that is genuinely slow, since a real transcript reaches 228MB across
  SSH — it drew nothing at all. Several seconds of empty pane is indistinguishable
  from a broken one, and the operator reported it as broken. If a state can take
  longer than a frame, it needs a visible state of its own.
- **Nothing moves under the operator's hands.** Automatic refreshes, autoscroll and
  re-renders all lose to whatever the person is doing. The transcript polls every
  five seconds; that poll destroyed any selection, so the pane behaved as if text
  could not be selected at all. It now stands down while `hasSelectionWithin` is
  true — and *says* it is standing down, because a view that silently stops
  updating is the same "is this broken?" impression in another costume.
- **Never show a control that cannot work.** Not disabled — absent. A greyed switch
  invites "how do I enable this"; a missing one asks nothing. Liquid Glass is
  hidden on machines without `NSGlassEffectView` and while a painted background
  covers the desktop; the GLASS slider is replaced by OVERLAY rather than left
  dead beside it. The Frost setting was the original sin here, and `--app-blur`
  is still in the tokens as its gravestone.
- **State must be readable without clicking.** Segmented buttons over dropdowns for
  small closed choices, because a `<select>` hides the options *and* the current
  value behind a click. Selection is carried by more than tone — at 9px a
  tone-only difference is not a state anyone can see, so the selected chip also
  wears an underline.
- **"Working" is detected from the elapsed timer, not from a phrase.** Detection rested on
  `esc to interrupt`, which inline rendering frequently does not print — so a session plainly
  working (`Adding due dates and priority… (2m 40s · ↓ 4.0k tokens)`) reported idle and the rail
  said nothing was happening anywhere, which is the rail's whole purpose failing. The stable
  shape is the ellipsis-then-duration; matching that also survives Claude renaming the spinner
  verb, which it does on every line. The digits must be followed by a time unit, or prose ending
  "… (see below)" pins the rail to "working" forever — a signal that is always on carries
  nothing.
- **Status is published for sessions that are *not* on screen** (`lib/status-watch.ts`). It was
  published by `ClaudePanel`, which only exists for a rendered session — so in a 2x2 grid the
  four sessions you could not see never reported anything and sat on "idle" forever. Those are
  precisely the ones the rail exists for; for the ones on screen you can simply look. It polls
  slower than the pane (1.5s vs 400ms) because a dot does not need four updates a second, and it
  leaves a session with no handle alone rather than guessing — reporting "idle" for something
  never opened is inventing a measurement.
- **The rail marks every session on screen, not just the focused one.** In a grid the operator
  is looking at four of eight and nothing said which four, so "is that one visible or do I need
  to go and find it?" could only be answered by looking away and counting panes. Derived with
  the same `gridLayout` the workbench uses — two sources for one fact is two chances to drift.
- **A context menu flips before it clamps.** Right-clicking near the bottom of a file tree put
  half the items below the window edge, unreachable, with nothing indicating they existed —
  Delete was simply gone. Growing *upward* from the cursor keeps the pointer on the menu's edge
  where the hand already is; clamping slides the list under the cursor so it lands on whichever
  item happens to be there, which is how someone deletes a file they meant to rename. The scale
  is *measured* (`rect.height / offsetHeight`) rather than assumed, because `getBoundingClientRect`
  is in viewport pixels while the `top` written is in the zoomed space — see the `zoom` rules.
- **A resize is told to the far side once it settles, not on every observation.**
  `ResizeObserver` fires continuously while a window is dragged, and both xterm hosts used to
  refit *and* send the new dimensions on each callback — so a two-second drag told the far side
  about forty widths. A TUI redraws completely on each one, and because Claude runs **inline**
  rather than on the alternate screen, every one of those redraws stays in the scrollback: the
  same question printed five times at five widths. Reported as "it look weird, have repeated
  render". `lib/terminal-resize.ts` debounces only the *message*; the local `fit` still runs on
  every callback so the grid stays attached to the pointer.
- **Clicking a pane gives the keyboard back to its terminal.** The terminal was focused once, at
  start; using any header control or clicking the padding around the rows moved focus away with
  nothing to restore it. A keystroke then went wherever the browser thought focus was, and for
  **Space** that means scrolling the nearest scrollable ancestor — xterm's own viewport, jumping
  to the bottom. It bites hardest in the case that causes it: a window too small to show a
  multiple-choice question, so you scroll up to read it and then cannot answer. `mousedown`
  rather than `click`, so focus is back *before* the key is pressed, and interactive elements
  are skipped or focusing would take it straight off the button being pressed.
- **A terminal must re-fit on every signal, not only on its own resize.** `ResizeObserver`
  correctly bails on a zero-sized host — a hidden pane measures 0x0, and fitting to that tells
  the far side the window collapsed — but that leaves the pane holding whatever cell grid it
  last computed. An interface-scale change arriving from the *Settings window* does not
  reliably move the element, so the terminal drew into a fraction of its pane and left the
  rest empty. The tell was the operator's: "when I scroll the scale bar it comes back to
  normal" — a window you have to jiggle to look right is the definition of not responsive. Both
  xterm hosts now also refit on `storage` (the appearance channel) and on window `resize`,
  across two animation frames so the zoom lands before a cell is measured.
- **Settings are staged, not live — and the boundary is labelled.** Appearance edits a draft
  and nothing moves until Apply; a slider that re-lays the window out on every tick reads as
  the app changing under the operator's hands, and it makes "I have not applied this yet"
  invisible. The exceptions are the two controls whose entire value is immediate feedback —
  user CSS and the GPU toggle — so they sit *below* the Apply bar under a heading that says
  they apply as you type. An unlabelled mix of staged and live is worse than either.
  **Restart is offered, never required.** Everything propagates across windows already (the
  `storage` listener); a relaunch only buys the terminals a clean re-measure after a scale
  change. The copy states that sessions survive it, because that is the only question anyone
  has before pressing it.
- **Every mutating control reports its outcome inline, beside itself.** Disabled
  plus a progress label in flight, then confirmation or the error. Errors persist
  until the next attempt; successes may fade after ~2.5s. An operation that can
  fail — picking a background picture, killing a process — must never appear to
  have succeeded.
- **Make the wrong click hard and the right one obvious.** The dangerous option is
  never the default and never one keystroke from the safe one:
  `--dangerously-skip-permissions` is a deliberate second choice that explains its
  consequence only once selected, and `KILL` is a separate control from `TERM`.
- **A destructive default is worse than a prompt.** Reset clears the stored
  background file as well as the setting, because leaving a wallpaper on disk that
  nothing in the UI can reach is litter nobody will ever find.

## Claude rendering — the one that caused the most grief

**Claude runs inline, never fullscreen** (`Rendering` in `crates/rmux-claude/src/lib.rs`).
Fullscreen moves to the alternate screen and takes the mouse with SGR tracking, and that one
fact produced three separate bug reports: text could not be selected (a drag goes *to Claude*,
so there is never a selection to copy), scrolling lagged (the wheel is a mouse report, so it
round-trips instead of moving xterm's scrollback), and every mouse move was another round trip.

Measured on a real host: forcing fullscreen emits `?1049h ?1000h ?1002h ?1003h ?1006h`; with
`CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 CLAUDE_CODE_DISABLE_MOUSE=1`, none of them. Those
override an explicit fullscreen request *and* the saved `tui` preference — which matters,
because `/tui fullscreen` persists, so anyone who ran it once carries the problem forever.

- The prefix goes on the **shell line** (`launch_line`), not `CommandSpec::env`: under the
  agent the shell is spawned by the *daemon*, which has its own environment, so env attached
  to the attach command never arrives.
- Every xterm host must load `WebglAddon`. The Claude pane shipped without it, and that was
  the rest of the lag.
- Inline costs only fullscreen's in-TUI mouse scrolling, its flat memory, and `/focus`.
  A native chat UI over `stream-json` was considered and **rejected**: ~50 slash commands are
  interactive-terminal-only (`/compact`, `/context`, `/resume`, `/permissions`, `/plan`, …),
  so it would trade real capability for scrolling. Keeping the real TUI is what guarantees
  parity — it *is* the CLI.

## A decision card needs a cursor, not just numbers

`parse_state` (`crates/rmux-claude/src/screen.rs`) turns Claude's screen into a prompt. It used
to accept any two numbered lines, so **"1. … 2. … 3. …" in ordinary prose became a decision
card** — measured on a live session, a six-point security summary rendered as a six-option card
drawn over the terminal. A test asserted this ("documenting current behaviour, not endorsing
it") for months.

It went unreported for so long only because a *second* bug hid it: the card computed
`position: relative` and was clipped to a 12px sliver, which was reported as a black bar. Fixing
the position exposed the misdetection underneath.

- **The signal is the selection caret, not the frame.** The code's own suggested mitigation —
  require the box-drawing frame — is wrong: Claude's **trust prompt is not framed**, it uses a
  horizontal rule, so a frame check would stop detecting the one dialog that blocks a session
  from ever starting. Both real captures carry `❯` on the highlighted option, because that is
  how the operator knows what Enter will pick. Prose has no cursor.
- **A frame is still accepted as an alternative**, so a repaint landing mid-frame — options
  drawn, caret not yet — does not flicker the card out.
- **Synthetic fixtures must draw a caret.** Three tests wrote `  1. Yes` with no marker, which
  is not a screen Claude ever produces; they encoded the bug into the suite.

## rmux is a backend, and the browser is not part of it

There is **no in-app browser**, and that is settled rather than pending. rmux's window is
one WKWebView; a webview cannot be given a per-session proxy without proxying the app's own
UI, so every page the old Browser tab showed needed a forwarded port typed in first — the
manual step the feature existed to remove. The tab is gone.

What replaces it is `crates/rmux-control`: NDJSON over `~/.rmux/control.sock` (`0600` in a
`0700` directory, plus a per-run token in `~/.rmux/control.json`), so a **separate Chromium
app** can do what a webview cannot. `OpenProxy` runs `ssh -D` and hands back a local SOCKS
port; pointed at with `socks5h`, the far side resolves DNS too, so an internal hostname
resolves on the server. That is the arrangement that actually delivers "no port forwarding".

- **`-D` binds `127.0.0.1:` explicitly.** A bare `-D <port>` binds per the host's
  `GatewayPorts`, and a SOCKS proxy into the operator's infrastructure reachable from the LAN
  is a hole, not a convenience.
- **Sessions live in the webview; Rust holds a mirror**, pushed by `control_sync` on every
  change. Moving ownership into Rust would put an IPC round trip on renaming a tab.
- **A `Report` from a browser is data, never an instruction.** It describes a page rmux did
  not write, so a selection's note is *typed into Claude's composer* (`claude_write`) and
  left for the operator to send — never submitted. Anything else is a prompt-injection path
  straight from any page the operator happens to open.

## Claude credentials

Three kinds, told apart by prefix (`rmux_claude::auth::CredentialKind`) because they look
alike and behave completely differently:

| Prefix | What it is | Where it may go |
|---|---|---|
| `sk-ant-oat…` | subscription token from `setup-token` | `CLAUDE_CODE_OAUTH_TOKEN` on a host |
| `sk-ant-api…` | Console (org-billed) API key | `ANTHROPIC_API_KEY` on a host |
| `sk-ant-admin…` | org admin key | **never leaves this machine** |

- **Never let an admin key reach a host.** It administers the organisation; it does not run
  models. `CredentialKind::detect` checks `admin` *before* `api` because both start
  `sk-ant-a`, and it is stored in a different keychain slot so the code path that ships a
  credential cannot even load it.
- **A subscription token cannot read usage.** Documented: it "can only make model requests".
  Real usage comes from the Console Admin API
  (`/v1/organizations/usage_report/messages`, `rmux-claude/src/usage.rs`), read **on demand**
  — it is a billing API, not something to poll.
- Sharing a *Pro/Max subscription* across people is account sharing; sharing a *Console key*
  is what Console is for. That distinction is why the account manager reports which kind it
  holds.

## Secrets never travel in argv

`ps` shows one user's command line to every account on a host, and `spec_to_shell_line`
renders `CommandSpec::env` **into** that command line. So a credential must not go through
either. The Claude account token reaches a host via `rmux-agent setenv`, which reads it from
**stdin** and hands it to the daemon over the `0600` socket (`Frame::SetEnv`); the daemon keeps
it in memory only and applies it to sessions it spawns. Verified on a real host: the token
reaches the session's environment and appears in **zero** process argv.

## Model profiles — running against Kimi, GLM or a gateway

Claude Code picks its provider out of the environment, so "use GLM here" is a set of
variables. `rmux_claude::profile` is that set, named and saved; `src-tauri/src/model_profile.rs`
stores and applies it.

- **A profile decides where the operator's credential is sent.** `ANTHROPIC_BASE_URL` and
  `ANTHROPIC_AUTH_TOKEN` travel together, so the *endpoint* is printed everywhere a profile is
  shown and is never inferred from its name — a profile called "GLM" can carry any URL at all.
- **Applying a profile must be able to *unset*.** The daemon merged the environment it was
  handed, which is right for adding an account and wrong here: switching from a vendor back to
  Anthropic would leave the old base URL in place, and the session would talk to — and be
  billed by — the previous provider while the UI said otherwise. An **empty value now means
  removal** (`daemon::merge_env`), and every apply sends *every* managed variable, empty for
  the ones it does not set. `apply_to_target` therefore runs even when no profile is selected:
  selecting nothing is an instruction to undo, not an absence. The clear-set spans the keys of
  every *other* stored profile too, or switching leaves behind whatever a different one
  introduced.
- **It reaches a host by `rmux-agent setenv`, never argv** — same path and same reason as the
  account token. Verified on a real host: the variables arrive in the session's environment,
  removal works, and `ps -eo args` shows the token in **zero** processes.
- **The profile set lives in the OS keychain as one JSON document**, not `localStorage` — it
  holds a paid credential, and that quota is shared with the session list. The UI is given a
  *redacted* view (`ProfileView`), and editing re-sends a whole block rather than round-tripping
  the stored token back through the webview. The edit form says the box is empty on purpose.
- **A profile is per-session and kept with it**, like `--dangerously-skip-permissions`: which
  provider a piece of work runs against is a property of that work, and an app-wide default
  would move a conversation to another provider on the next restart with nothing on screen to
  say so. A session naming a deleted profile **refuses to start** rather than falling back to
  Anthropic, because a silent change of provider is the failure worth being loud about.
- **Only `ANTHROPIC_*` and `CLAUDE_CODE_*` keys are carried**, and refused keys are *named*.
  A profile is not a general-purpose environment editor; `PATH` and `LD_PRELOAD` do not belong
  in one. Unknown keys inside those two namespaces are still carried — Claude Code gains
  variables faster than a hard-coded list can.
- **The paste is parsed, not retyped.** People have these as a block of `KEY=value \` lines;
  eight text fields would mean transcribing a base URL by hand, and a typo there points a
  credential at the wrong host without ever looking wrong.

## Reading a conversation back

`crates/rmux-claude/src/transcript.rs` reads Claude's own `.jsonl` rather than scraping the
TUI. Three things it must keep doing:

- **Only ever read the tail.** A real transcript measured **228MB** on a working server; the widget
  polls this on a timer. `tail -c` cuts mid-line, so the first line is dropped — but only
  when the read was actually truncated, or a short file loses its first message.
- **Never bind to Claude's schema.** Fields are picked out one at a time and unknown record
  and block types are skipped. A strict deserialise turns every Claude Code release into
  "the transcript is empty".
- **Slash-command plumbing is not the user talking.** `<command-name>`, `<local-command-*>`,
  caveat banners and system reminders are recorded as user messages and dominate the tail of
  a long session. They are demoted to `system` and hidden by default, not dropped.

## The Cowork server

**The server lives here, under `server/`.** It was copied in from `redstone-cowork`, which is
now **reference only** — an old project we read for design and feature history. Never modify
it, never commit to it, never push to it. Anything the server needs to do is changed in
`server/` and deployed from here.

**Nothing sensitive comes across.** The copy is filtered: no `.env` of any kind (including
`.env.example`), no keys, no credentials. The only credential-shaped strings in `server/` are
`${POSTGRES_PASSWORD}`-style placeholders in `docker-compose.yml`. Re-check before any push —
this repository has been pushed to a public remote at least once.

**Which server, and where it runs, is deployment detail and is deliberately not recorded in
this repository.** It belongs in `CLAUDE.local.md`, which is git-ignored. Nothing in the code
may hard-code a hostname either — the live tests read `RMUX_LIVE_SERVER`, and the app asks the
operator. A repo that names its own deployment leaks it to everyone the repo is ever shared
with, and this one is meant to be shareable.

`GET /auth/config` reports what a server offers, and rmux asks it *before* offering any
sign-in method — which Jira, whether accounts exist, the org name are all that server's
configuration, not ours.

**Jira sign-in is the desktop OAuth flow**, not a password form:
`POST /auth/jira/start` (empty body — sending `redirectTo` switches the server into its *web*
flow, where the callback redirects instead of leaving an outcome to drain) → open `authUrl` in
the operator's **real browser** → poll `GET /auth/jira/poll?state=…`. The server **drains** the
outcome on read, so a successful poll must be acted on immediately and never retried.

## The app lock — PIN and face

Off by default. It appears only when a **sealed** session is stored, and that is the one
screen allowed to gate the workbench, because the operator asked for it.

- **The PIN is the lock, not a check.** `rmux-cowork/src/lock.rs` seals `StoredCredentials`
  with XChaCha20-Poly1305 under an argon2id key (64 MiB, t=3). Unlocking *is* decryption, so
  there is no comparison to bypass and a wrong PIN yields nothing. A boolean flag plus a
  screen would leave the token readable in the keychain — that is theatre, and it was the
  previous app's design.
- **The server's `POST /accounts/me/pin/verify` is deliberately unused.** Measured: it
  answers `{ok:false}` rather than 401, has no rate limiting and writes no audit row. It
  trusts the client to enforce the outcome, which is fine for an advisory check and useless
  as a lock. Checking locally also means the lock works with no network — which matters,
  since nothing the workbench does needs the server.
- **The derived key is held for the run** (`AuthStore::vault_key`). The token is rewritten
  while the app runs (SSO refresh rotates it), and without the key the only options would be
  re-prompting mid-session or writing it back unsealed. **Every save goes through
  `AuthStore::persist`** — a direct `credentials::save` from a command would silently
  un-seal the vault.
- **Face cannot decrypt anything**, because a biometric cannot derive a key. So face unlock
  calls `POST /auth/face/login` instead, which mints a *fresh* session from a match plus this
  machine's `rcwd_` device secret. That needs the network, so the PIN stays the offline path.
- **Face is never the security floor.** There is no liveness check anywhere in this stack and
  the server accepts any well-formed 128-float array, so a photograph passes — the org's own
  admin tool enrols from still JPEGs, which proves it. It is a convenience over typing.
- **The device secret never reaches the webview.** It is a bearer credential: it plus any
  matching descriptor mints a session. The UI hands *up* a descriptor and gets back an
  account. `sign_out` clears it too, or a "signed-out" machine could still mint sessions.
- **Don't re-enrol a face that already exists.** `POST /accounts/me/face/enroll` *appends* a
  sample; enrolling per machine grows the set every future login is matched against, which
  loosens the match. Use `device/trust` when `hasFace` is already true.
- The server 500s on a malformed descriptor rather than validating it, so `check_descriptor`
  rejects wrong lengths and non-finite floats client-side. NaN is the dangerous one: it makes
  the server's euclidean distance NaN, and `NaN > threshold` is false — it would *pass*.
- **Face models are downloaded, not bundled** (`src-tauri/src/face_models.rs`): 6.7 MB, on
  first use only, with **pinned SHA-256s** — they are fetched from a CDN and fed to a model
  runtime, so a mismatch is refused rather than cached. The webview cannot fetch them itself;
  `connect-src` is this origin plus IPC, hence the asset protocol and the Rust-side download.
  `@vladmandic/face-api@1.7.15` is not a free choice — a descriptor only compares to the
  enrolled ones if it came from the same weights.
- **The camera needs a real `.app`.** `src-tauri/Info.plist` carries
  `NSCameraUsageDescription`; macOS refuses the camera outright without it, and a bare
  `./target/release/rmux` has no Info.plist. PIN unlock works either way.
- Reading the keychain from a **fresh test binary** raises a macOS authorisation dialog and
  blocks forever with no output. `tests/live_lock.rs` takes the token from an env var for
  exactly this reason.

**Signing in is optional and must stay that way.** Terminals, files and Claude are a direct SSH
connection that never touches this server, so there is no login gate — the app opens straight
into the workbench and sign-in is a footer button. Restoring a stored session runs *beside* the
app, never in front of it, so an unreachable server delays a footer label and nothing else.

## Conventions

- **Never report something as ready before verifying the artefact itself.** Starting a build
  is not finishing one; `pnpm tauri build` returns from its Vite step minutes before the Rust
  compile produces a binary, so "rebuilding now, it'll relaunch" reads as "go and test it" and
  sends the operator back to the *same broken build* they just reported. This happened twice in
  one sitting and wasted more of their time than the original bug.

  The rule is to check the thing, not the command: the binary's mtime and size changed, the
  process is running, the socket is up, the test passed. State what was observed. If a
  verification is still pending, say that instead — "still compiling, do not test yet" is
  useful; "should be ready" is not.

  Same rule for fixes: a change that compiles is not a change that works. Say which one it is.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets` must be clean.
- `pnpm exec tsc --noEmit` for the UI; `pnpm exec vite build` to verify bundling. The
  `ui/*-check.ts` harnesses are in `tsconfig.json`'s `include` **on purpose** — they were
  outside it and silently unchecked, so `tsc` passed while one referenced an undefined
  symbol. Anything added beside them needs adding there too.
- Vite runs on port **5273** with `strictPort` — a silent port fallback changes the origin
  and silently discards all saved `localStorage` state.
- **A blank window means the binary is looking for Vite. There is exactly one correct way to
  build a shippable app: `pnpm tauri build`.**

  Tauri serves its embedded UI only when compiled with the **`custom-protocol`** feature.
  `pnpm tauri build` passes it; `cargo build --release` does **not**. Without it a *release*
  binary falls back to `devUrl` — `http://localhost:5273` — and with no Vite running the
  window is simply blank: no error, nothing in the log, nothing in the console. It is the
  same symptom as a debug build with no Vite, which is precisely why it gets misread as "the
  app is broken" rather than "this binary was built wrong".

  **Never hand-assemble a bundle.** Building with `cargo build --release` and copying the
  binary into an existing `.app` produces something that looks perfect — right size, valid
  signature, correct Info.plist — and does nothing at all when opened. This has happened, and
  it cost a working app in front of the operator. If you need a signed bundle: run
  `pnpm tauri build`, then sign, then install. No shortcuts through `cargo` directly, ever,
  no matter how much faster it looks.

  For development use `pnpm tauri dev`, which starts Vite and the binary together.
- **`cargo build --release` does not notice a changed `dist/`.** Nothing declares it as a
  build input, so a UI-only change relinks in under a second and embeds the *previous*
  bundle — the app then runs code you did not build, silently. `touch src-tauri/build.rs`
  first, and sanity-check that the binary is newer than `dist/index.html`. Assets are
  brotli-compressed inside the binary, so grepping it for a bundle filename proves nothing.
- Monaco is bundled, not CDN-loaded, and its workers need `worker-src 'self' blob:` in the
  Tauri CSP. Import worker entry points as `monaco-editor/editor/editor.worker.js?worker` —
  the package's `exports` map already prefixes `esm/vs`, so repeating it fails to resolve.
- Every mutating control reports its outcome *inline, next to that control*: disabled +
  progress label in flight, then confirmation or the error message. Errors persist until
  the next attempt; successes may fade after ~2.5s.
- Don't run Docker on this Mac; use a remote dev server for integration tests needing sshd.
- **A terminal's shell belongs to `rmux-agent`, not to rmux.** Terminals run
  `rmux-agent attach --session <tab id>` on the target, so the shell survives quitting the
  app, losing the network, and closing the lid — and the same tab id reattaches to the same
  shell. Two consequences: the tab id is *persisted state*, not a runtime handle, and closing
  a tab must send `rmux-agent kill <name>` or the shell leaks unreachable forever.
- `scripts/build-agents.sh` must be run before a remote terminal can be persistent — it
  cross-compiles the static musl agents into `src-tauri/agents/`, which the bundle ships as
  resources and uploads to a host on first use. Needs `cargo-zigbuild` + `zig`.
- **The installed agent's path carries a content fingerprint, not just a version.** Two
  builds share a version constantly (every dev build does), and a version-only check leaves
  the host running the old binary — which surfaces as `unknown option` from a version number
  that looks right. A live test pins this.
- **Claude runs under the agent too**, via `--login-command`, so a conversation keeps working
  with rmux closed. It must be a *login* shell: `claude` is installed by a version manager
  whose PATH only exists there, so spawning the binary directly gives "command not found" on
  a host where it is plainly installed.
- **The login shell is `-l` *and* `-i`, and both launch paths must build it the same way.**
  `zsh -l` reads `.zprofile` and `.zlogin` and **not** `.zshrc` — and `.zshrc` is where every
  version manager writes its PATH. So a login-but-not-interactive shell still reports
  "command not found: claude" on a zsh host where claude works when typed. Everything goes
  through `CommandSpec::login_shell()`; a hand-built `$SHELL -l` is the bug. This was fixed
  once in `rmux-agent` while `ClaudeSession::start_resuming` kept its own copy, so the
  direct path stayed broken and the two answers disagreed depending on whether the session
  was hosted by the agent. **Fixing the agent is not enough on its own**: the agent is a
  cross-compiled binary uploaded to the host, so `scripts/build-agents.sh` has to run and the
  app has to be rebuilt before the host sees the change at all.
- **`--dangerously-skip-permissions` is a per-session launch argument, never a stored
  default.** It is a judgement about one piece of work on one machine, made at the moment of
  starting it; a saved preference would carry it silently into work started weeks later
  somewhere else, which is exactly the case where nobody re-reads the flag. It *is* kept with
  the session, though, so a pane that restarts comes back with the latitude it was launched
  with — quietly gaining or losing permission checks across a reconnect is worse than either
  setting. Offered on both launch screens (`PermissionChoice`), defaulting to the safe one.
- **Claude's project directory name is not computable.** The scheme changed between versions
  (`/home/a.user/x` files under `-home-a-user-x` now, `-home-a.user-x` before),
  so `list_sessions_script` tries every spelling and then falls back to reading the `cwd` each
  transcript records. Reporting "no previous sessions" for a folder full of them is the worst
  possible failure — verified against a real server.
- **Never put `$HOME` in a path that gets `shell_quote`d.** Quoting is what stops it
  expanding, so it becomes a literal directory named `$HOME`. Resolve the home once
  (`provision::home_script`) and pass an absolute path down. A live test pins this.
- **Do not implement copy or paste on the terminal keydown.** xterm and the webview already
  handle both, and Tauri installs the default macOS menu (Edit > Copy/Paste) — so a handler
  here is a *second* implementation and every paste arrives twice. That was a real bug.
  `ui/src/lib/terminal-clipboard.ts` therefore adds only select-all, plus `copyViewport` /
  `copyAll` / `copySelection` for callers.
- **xterm's selection is invisible to the DOM** (measured: `window.getSelection()` is empty
  while `term.hasSelection()` is true), so anything relying on a DOM selection copies
  nothing. xterm does fill a native `copy` event, which is why Cmd-C works *when there is a
  selection*.
- **Every xterm host must load `WebglAddon`.** Without it xterm falls back to the DOM
  renderer, and scrolling or dragging over a constantly-redrawing TUI is visibly slow. The
  Claude pane shipped without it and that was the lag.
- **An IME is not simulable, and `ime-check.ts` is the proof.** It fires the composition
  events macOS was *assumed* to fire, passes 4/4 against a real xterm, and the app stayed
  broken for weeks underneath it. Marked text is produced by the input method **above the
  DOM**; a script cannot make one. The only thing that worked was recording the real events
  from the running app while a person typed. Three separate faults came out of two minutes of
  that, and none of them matched the simulation:

  1. **macOS Vietnamese has *two* modes and fires completely different events in each.** One
     recording had **no composition events at all** — the accent arrives as
     `insertReplacementText`, which xterm has no handler for, so the base letter went out and
     every accent was dropped (`ee ee ee` → `e e e`). The next recording used compositions
     throughout. Handle both or half of Vietnamese is broken.
  2. **xterm parks a NBSP in its textarea and never clears it** — measured, the value grew to
     `"…xin chào tôi đang gõ "` across one sentence. `compositionstart` records
     `start = value.length`, counting that NBSP, but the IME inserts *before* it, so the
     finalising slice begins one character late and **every word loses its first letter**
     (`Tiếng Việt` → `iếng iệt`). Clearing the textarea at `compositionstart` fixes it;
     clearing at any other time moves the ground under the same offsets.
  3. **xterm flushes marked text as it changes.** On the wire `Tiếng` left as `"Ti"` then
     `"ê"`, two sends, mid-word — so the tone key that came next had nothing left to modify
     and landed literally (`Tiêngs`). A terminal cannot unsend, so `onData` is **gated shut
     for the duration of a composition**. Its *partial* states are the fault; its committed
     one is correct, so the gate reopens in the capture phase at `compositionend` and xterm's
     own final send passes through. **Do not send the committed word yourself** — that was
     tried and doubled everything (`TiêngsêngsViệtViệt`).

  `lib/ime-replace.ts` is all three. Anything that changes typing must be checked against a
  real IME by a person, because nothing else can produce these events.
- **A recorder that posts per event is not free.** The instrument that found the above fired
  an HTTP POST on every `keydown`, `beforeinput`, `input`, `compositionupdate` and wire write
  — a dozen round trips per keystroke — and typing became visibly laggy. That was read as a
  regression in the fix. Measure with it, then take it out, and say which build has it.
- **Never put padding on the element `FitAddon` measures.** It reads
  `getComputedStyle(host).height`, and under `box-sizing: border-box` — which Tailwind sets
  globally — the resolved `height` is the *padding* box. The addon does subtract padding, but
  it reads it from `.xterm`, because it assumes the padding is on the terminal element rather
  than on the parent. So `p-2` on the host handed `fit()` 16px it did not own and never took
  back, and it asked for **one row more than the pane could hold**: measured at 25 rows ×
  19.5px = 487.5px into 476px of content, with `.xterm`, `.xterm-viewport`, `.xterm-screen`
  and the canvas all hanging 4px past the pane edge.

  The overshoot is real, measured and fixed. **It was not, however, the "black bar along the
  bottom of the Claude pane" that was reported** — that band survived this fix and nine more
  after it, because it was never in the terminal at all. See the `.corner` rule in
  `signal-room.css`: it was Claude's *decision card*, defeated by a cascade collision, laid
  out below the pane and clipped by `overflow-hidden` to a 12px sliver. Ten fixes aimed at
  xterm — viewport backgrounds, three scrollbar rules, this padding, the GPU renderer, native
  glass — could not have worked.

  **Two lessons, and the second is the expensive one.**

  `document.elementsFromPoint` plus `getComputedStyle().backgroundColor` is blind to
  scrollbars, pseudo-elements and renderer-drawn content. A probe built on it reported
  "nothing paints here" for nine rounds and was believed over the operator looking straight at
  the thing. When measurement and observation disagree, stop reading properties and paint the
  candidates.

  But painting then gave a result that was *true and completely misleading*: every layer from
  `.xterm-screen` up through `.panel`, the grid cell and `body` was given a distinct colour,
  and the band claimed none of them. That read as "nothing in the DOM paints this" and sent
  the search to native glass and the compositor. It actually meant **"you are painting the
  wrong subtree"** — the card is a *sibling* of the terminal, so it was never in the chain
  being coloured. A list of suspects chosen in advance cannot find a culprit that is not on
  it, and every round of narrowing made the list more confidently wrong.

  What worked in one round: dump the rects of **every child of the pane**, enumerated rather
  than named, and read which box the strip falls in. Prefer enumeration to a hypothesis list
  whenever something has already survived more than two fixes.

  The padding goes on a wrapper *inside* the `relative` pane — not on the pane itself, whose
  padding box positions the absolute overlays. `ui/blackbar-check.ts` builds the real DOM
  chain, fits, and fails on the old structure; `?fixed` builds the corrected one. Its first
  version never called `fit()` and so measured an unfitted 80×24 terminal, which is how the
  first two guesses got made — **a terminal harness that does not fit measures nothing.**
- **Select mode** (`ui/src/lib/mouse-modes.ts`) writes the mouse-reporting reset sequences
  *into xterm*, not to the program — mouse tracking is state in this terminal, so turning it
  off locally makes drags select again without the program knowing. The modes are tracked
  from the program's own output so they can be restored exactly; never reset a private mode
  that is not a mouse mode (`1049` is the alternate screen, and resetting it wipes the
  display). It also stops the round-trip-per-mouse-move that any-event tracking causes on a
  remote session.
- **In the Claude tab there usually is no selection**: its TUI turns on mouse reporting, so a
  drag goes to Claude instead of selecting. Option-drag selects, but that is not
  discoverable — hence the copy buttons in that tab's header, which need no selection.
  `ui/terminal-check.html` covers the key handling; `ui/xterm-clipboard-check.html` covers
  the above against a **real** xterm, which is the part a stub cannot prove.
- The office readers (`ui/src/lib/office.ts`) need `DOMParser`, so they cannot be tested in
  Node. `ui/office-check.html` runs them against real Office-produced fixtures in
  `ui/__fixtures/` — open `http://localhost:5273/office-check.html` and read the console.
  Fixtures live outside `ui/public/` deliberately; anything in `public/` is copied into the
  shipped bundle.
- **`.docx` HTML goes through `sanitize` before it reaches the DOM.** The file is untrusted
  input and the webview can reach Tauri IPC, so the CSP is not the only line of defence.
