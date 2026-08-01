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
crates/rmux-browser     chromiumoxide CDP (not yet built)
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
- `alacritty_terminal` is pinned with `=` — it offers no stability guarantee across minor
  versions.

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

Blur is **per-panel, not app-wide** — that is what keeps the backdrop sharp between panels
and makes the material read as glass. Fonts (SFU Futura, IBM Plex Mono) are bundled, never
fetched from a CDN; this app must look right with no network.

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

The server itself lives in a **separate repository** (`redstone-cowork`), which has its own
git history and remote. Do not vendor it in here: two copies of a tracked codebase means two
sources of truth, and this repo would carry the one nobody deploys from.

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

- `cargo test --workspace` and `cargo clippy --workspace --all-targets` must be clean.
- `pnpm exec tsc --noEmit` for the UI; `pnpm exec vite build` to verify bundling. The
  `ui/*-check.ts` harnesses are in `tsconfig.json`'s `include` **on purpose** — they were
  outside it and silently unchecked, so `tsc` passed while one referenced an undefined
  symbol. Anything added beside them needs adding there too.
- Vite runs on port **5273** with `strictPort` — a silent port fallback changes the origin
  and silently discards all saved `localStorage` state.
- **A debug build loads `devUrl`, not the bundled files.** `./target/debug/rmux` fetches the
  UI from `http://localhost:5273`, so with no Vite running the window is simply **blank** —
  no error, nothing in the log. Use `pnpm tauri dev` (starts both), or keep Vite running
  alongside the binary. `cargo build --release` embeds `dist/` instead and needs no server;
  that is what to hand someone for actual use.
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
