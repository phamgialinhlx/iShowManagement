# Image Paste into Remote Terminals

**Status:** planned · grilled 2026-07-23
**Goal:** copy a screenshot, hit Cmd+V in a `console`/`tmux` terminal, and the
CLI running there (Claude Code, aider, …) can use it — the image lands as a
file in the remote host's `/tmp` and its path is typed into the terminal.

This is Zed's image-paste model (intercept paste → temp file → inject path)
extended one hop over SSH. Companion to
`plans/2026-07-23-bidirectional-clipboard.md` (text directions; shipped).

## Decision log (ADR)

| # | Decision | Choice | Why / consequences |
|---|----------|--------|--------------------|
| 1 | Use case | Feed pasted images to CLIs by **path** (Claude Code workflow) | A terminal is a byte stream; the only thing every CLI understands is a file path on its own filesystem. Not inline image *display* (sixel etc.) — different feature. |
| 2 | Remote location | `/tmp/ishow-paste-<ts>-<rand>.<ext>`, **no active cleanup** | Screenshots are small; /tmp is OS-reaped. Eager deletion would break the consumer (Claude Code reads the file after we return). Filename is generated from a safe alphabet → no shell-quoting concerns. |
| 3 | Mode scope | **console + tmux only** | User's call (narrower than recommended). `local` and `docker-exec` pastes are not intercepted at all — behavior there is unchanged (xterm ignores non-text paste). docker-exec would need a second `docker cp` hop; add later if needed. |
| 4 | Feedback UX | **Silent while uploading; standard dialog on error** | Sub-second scp over ControlMaster; the path appearing IS the success signal. Failures use lib/dialogs.svelte.ts (WKWebView has no native dialogs). No status-bar state, no placeholder-erasing tricks. |
| 5 | Clipboard access | DOM `paste` event only (no `navigator.clipboard.read`) | The paste event exposes image items even in WKWebView because it's a native gesture — same reason Cmd+V text paste already works. No permission prompts. |
| 6 | Injection path | Frontend `term.paste(path)` after the POST returns | Rides the existing keystroke→WS→PTY path; core needs no session registry. `term.paste()` applies bracketed paste when the remote app enabled it, so Claude Code sees a paste, not typed keys. |
| 7 | Transport | `scp` over the existing ControlMaster (stage to a local temp file first) | Mirrors files.rs download's scp arm; rsync buys nothing for one small file. Local temp deleted after upload regardless of outcome. |
| 8 | Formats & limits | Any `image/*` the paste event yields; extension from MIME (fallback `.png`); **20 MiB cap** (413 above); first image item only on multi-item pastes | Screenshots are PNG on macOS; keep bytes as-is, no conversion. |

## Design

### New: `POST /api/servers/{id}/paste-image` (core)
- Raw request body = image bytes; `Content-Type` gives the extension.
  Register in `lib.rs` next to the other `/api/servers/{id}/…` routes; the
  existing origin-guard middleware and loopback bind already cover it.
- Handler (new fn in `files.rs`, beside `download`):
  1. Validate alias (`safe_name`), size cap via `DefaultBodyLimit` layer.
  2. Write body to a local temp file.
  3. `scp <tmp> <alias>:/tmp/ishow-paste-<UTC ts>-<4 hex>.<ext>` using
     `ssh::control_args`-style options (same as download's scp arm).
  4. Delete local temp; return `{ "path": "/tmp/ishow-paste-…" }` or a
     `502` with scp's stderr.

### Changed: `web/src/lib/Terminal.svelte`
- On mount, when `mode` is `console`/`tmux`: add a capture-phase `paste`
  listener on the host div (fires before xterm's textarea handler):
  - No image item in `clipboardData` → do nothing (text paste unchanged).
  - Image item → `preventDefault`/`stopImmediatePropagation`, POST the blob
    to `/api/servers/{alias}/paste-image`, then `term.paste(path + ' ')`.
  - On failure → error dialog via `lib/dialogs.svelte.ts`.
- Remove the listener in `onDestroy`.

### Changed: `web/src/lib/api.ts`
- One helper: `pasteImage(alias: string, blob: Blob): Promise<{path: string}>`.

### Unchanged
- `ws.rs`, `pty.rs`, `clipboard.rs` — the path is injected as ordinary
  keystrokes by the frontend.
- `local` / `docker-exec` / `docker-logs` terminals — paste behaves as today.

## Steps

1. Core endpoint + scp upload → **verify:** unit test filename/arg
   construction; `curl --data-binary @shot.png -H 'Content-Type: image/png'
   http://127.0.0.1:<port>/api/servers/<alias>/paste-image` returns a path,
   and `ssh <alias> file <path>` says PNG.
2. Size cap + error mapping → **verify:** >20 MiB body → 413; bad alias →
   400; unreachable host → 502 with stderr in the message.
3. `api.ts` helper + Terminal.svelte paste interception → **verify:** in the
   app, Cmd+V a screenshot into a console session → path appears in the
   terminal; Cmd+V plain text still pastes text; image paste in a `local`
   terminal does nothing (unchanged).
4. End-to-end with the real consumer → **verify:** remote Claude Code session,
   paste screenshot, path arrives via bracketed paste, Claude Code reads the
   image.
5. README: extend the Clipboard feature bullet with image paste.

## Out of scope
- docker-exec (second `docker cp` hop) and `local` mode interception.
- Inline image display (sixel/iTerm2/kitty protocols).
- Remote-side cleanup, multi-image paste, progress UI, format conversion.

## Glossary
- **Paste event image items** — `ClipboardEvent.clipboardData.items` of kind
  `file` / type `image/*`; available without clipboard-read permission
  because the paste is a user gesture.
- **Path injection** — typing the uploaded file's remote path into the PTY as
  if the user typed it; via `term.paste()` so bracketed paste applies.
- **ControlMaster** — ssh connection multiplexing already used by every ssh
  call in core; makes the scp hop connection-free (~fast).
- **Bracketed paste** — see the bidirectional-clipboard plan's glossary; here
  it's what makes Claude Code treat the injected path as a paste.
