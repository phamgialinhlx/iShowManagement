# Bidirectional Clipboard

**Status:** planned · grilled 2026-07-23
**Goal:** clipboard "just works" over SSH, the way Zed's terminal does it —
remote copies (tmux yank, nvim `"+y`, `printf` OSC 52) land on the macOS
clipboard; Cmd+V pastes the macOS clipboard into the remote terminal.

Model verified against Zed source (`references/zed/crates/terminal/src/`):
Zed enables OSC 52 *copy* for PTY terminals, keeps OSC 52 *read* disabled
(alacritty's `OnlyCopy` default), disables OSC 52 entirely for display-only
terminals, and pastes via ordinary keybindings. No clipboard daemon/sync.

## Decision log (ADR)

| # | Decision | Choice | Why / consequences |
|---|----------|--------|--------------------|
| 1 | Feature model | Zed's: OSC 52 copy **on**, OSC 52 read **off**, paste via Cmd+V | Read direction lets any remote process silently exfiltrate the Mac clipboard; `OnlyCopy` is the battle-tested posture (Alacritty default). |
| 2 | Interception point | **Core**, in the PTY output path (`ws.rs bridge()`) | `navigator.clipboard.writeText` in WKWebView is gated on recent user activation — OSC 52 arrives after a WS→ssh round trip, so the frontend addon (`@xterm/addon-clipboard`) is silently flaky in the Tauri shell. Core is always on the same Mac (loopback-only bind), so a server-side write is deterministic in both the app and browser mode. |
| 3 | Mode gating | Enabled for `local`, `console`, `tmux`, `docker-exec`; **disabled for `docker-logs`** | Mirrors Zed's display-only exclusion: a tailed log can contain attacker-printed OSC 52 with no user driving the session. |
| 4 | tmux out-of-box | **Auto-configure on attach**: append `set -g set-clipboard on` + `set -as terminal-features ',xterm*:clipboard'` to our attach command | Default `set-clipboard external` swallows inner-app OSC 52, and stock `xterm-256color` terminfo lacks `Ms`, so tmux would never emit OSC 52 to us. Cost: mutates global options on the remote tmux server until it restarts — accepted; both are broadly-safe defaults. |
| 5 | Paste/copy UX | **Just Cmd+C / Cmd+V**, verified in Tauri shell + browser | xterm.js already handles DOM copy/paste events; Tauri v2's default macOS menu provides the Edit roles. No copy-on-select, no right-click paste. |
| 6 | Clipboard writer | `pbcopy` subprocess with `LC_CTYPE=UTF-8` env, via `spawn_blocking`; `cfg(target_os = "macos")`, no-op elsewhere | Zero new dependency (vs `arboard` crate); app is macOS-only. `LC_CTYPE` prevents non-ASCII mangling. Failures are logged, never kill the session. |
| 7 | Stream handling | **Pass-through tee** — scanner never modifies the byte stream | xterm.js without the clipboard addon ignores OSC 52 silently, so stripping buys nothing and rewriting chunks risks corrupting split sequences. |
| 8 | Limits & queries | Decoded payload cap **1 MiB**; unterminated sequence abandoned after 2 MiB; OSC 52 query (`Pd = ?`) ignored, never answered | Cap bounds memory; ignoring `?` matches Alacritty `OnlyCopy` (apps treat no-reply as unsupported). |
| 9 | Configurability | **None** — no settings toggle | Not requested; matches project rule against speculative config. Add a toggle only if a real need appears. |

## Design

### New: `core/src/clipboard.rs`
- `Scanner` — per-session state machine fed each output chunk
  (`fn feed(&mut self, chunk: &[u8]) -> Vec<String>` returning decoded copies).
  - Recognizes `ESC ] 52 ; Pc ; Pd (BEL | ESC \)` split at **any** byte
    boundary across chunks (carry state, not buffering the whole stream).
  - `Pc` (clipboard selector) ignored — macOS has one pasteboard.
  - `Pd = ?` (query) → ignored. Invalid base64 / empty / oversized → ignored.
- `fn copy_to_clipboard(text: String)` — pipes to `pbcopy` (`LC_CTYPE=UTF-8`),
  logs on failure. macOS only.

### Changed: `core/src/ws.rs`
- Decide `clipboard: bool` from mode (`docker-logs` → false) before `bridge()`.
- In `bridge()`'s output arm: `scanner.feed(&bytes)` → for each copy,
  `tokio::task::spawn_blocking(copy_to_clipboard)`. Bytes forwarded unchanged.

### Changed: `core/src/ssh.rs`
- `tmux_remote()` becomes attach-then-configure:
  `tmux new-session -A -s <s> \; set -g set-clipboard on \; set -as terminal-features ',xterm*:clipboard'`
  (mind remote-shell quoting: `\;` for tmux command separators, single-quotes
  around the terminal-features value). Update the existing unit test.

### Unchanged
- `web/src/lib/Terminal.svelte` — no addon needed; OSC 52 passes through and
  xterm ignores it. Only touched if Cmd+C/V verification fails.
- `pty.rs` — scanner lives in the WS layer, not the reader thread.

## Steps

1. `clipboard.rs` Scanner + unit tests → **verify:** `cargo test` covers:
   BEL and ST terminators, sequence split across 2–3 chunks, split ESC at
   chunk edge, `?` query ignored, bad base64 ignored, >1 MiB dropped,
   interleaved normal output untouched.
2. `copy_to_clipboard` via pbcopy → **verify:** unit-test the command
   construction; manual `pbpaste` check in step 3.
3. Wire into `ws.rs` with mode gate → **verify:** in the app's local-shell
   terminal run `printf '\e]52;c;%s\a' "$(printf hello | base64)"`, then
   `pbpaste` prints `hello`; same over `console` to a real host.
4. `tmux_remote()` update → **verify:** unit test string; manually attach a
   tmux session from the sidebar, `tmux show -g set-clipboard` says `on`,
   copy-mode yank lands in `pbpaste`; nvim `"+y` inside tmux lands in `pbpaste`.
5. Cmd+C / Cmd+V verification pass in the Tauri app and Chrome (paste into
   remote vim to confirm bracketed paste) → fix only if broken.
6. README: add clipboard line to Features.

## Out of scope
- OSC 52 read/query answering (clipboard exfiltration risk).
- System-clipboard *sync* (daemon, pbcopy shims on the remote).
- Copy-on-select, right-click paste, settings toggle.
- `docker-logs` clipboard extraction.

## Glossary
- **OSC 52** — xterm "Operating System Command" 52: `ESC ] 52 ; Pc ; Pd ST`,
  lets terminal programs set (or with `?`, query) the terminal's host
  clipboard. `Pd` is base64 text.
- **Pc / Pd** — OSC 52 params: clipboard selector (`c`, `p`, `s`, `0-7`) and
  base64 payload.
- **BEL / ST** — OSC terminators: `\x07` or `ESC \`.
- **set-clipboard** (tmux) — `external` (default): only tmux's own yanks are
  forwarded; `on`: inner applications' OSC 52 is forwarded too.
- **Ms / terminal-features clipboard** — how tmux decides the outer terminal
  supports OSC 52; stock `xterm-256color` terminfo lacks `Ms`, hence ADR #4.
- **Bracketed paste** — terminal wraps pasted text in `ESC[200~ … ESC[201~` so
  apps (vim, shells) treat it as a paste, not keystrokes; xterm.js does this.
- **Pass-through tee** — core observes the PTY byte stream for OSC 52 but
  forwards it byte-identical to the frontend.
