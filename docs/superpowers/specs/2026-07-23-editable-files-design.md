# Editable Files in the Files Browser

**Date:** 2026-07-23
**Branch:** `feat/files/nano`
**Status:** Approved (pending spec review)

## Goal

Make text files editable directly in the Files browser — click a file, edit it in
place with syntax highlighting, and have edits save back to the file (local disk or
the remote host over ssh). The editing experience should feel like a normal editor
(highlighting, line numbers, find) while the feature scope stays simple: edit one
file at a time, autosave, done.

## Scope

**In scope**
- Clicking a text file opens it in an editable CodeMirror 6 pane (syntax
  highlighting, line numbers, find).
- Edits autosave (debounced) back to the file — local disk or remote host over ssh
  — via an atomic write.
- Visual save status: unsaved / saving / saved / save-failed.

**Out of scope (kept simple)**
- No multi-tab or multi-file editing; one file open at a time.
- No project explorer, no git/diff, no image editing.
- No create/rename/delete from the editor.
- Images, binary, and >512 KB files stay exactly as today (preview/download only).
- Non-UTF-8 / invalid-UTF-8 files are not editable (shown read-only) to avoid
  corrupting binary content on round-trip.

## Editable rule

A file is editable when **all** of the following hold (mirrors today's preview rule):

1. It is a regular file (not a dir, not a link-target that resolves to a dir).
2. `size <= TEXT_LIMIT` (512 KB).
3. `is_text_preview(name, mime)` is true (the existing predicate in `files.rs`).
4. The base64-decoded content is valid UTF-8.

If a file satisfies 1–3 but fails UTF-8 decode (rule 4), the UI renders it read-only
with a small "not editable: not UTF-8" note, rather than risk a corrupting save.

## Backend: the write path

### New primitive in `core/src/ssh.rs`

`exec_with_input` — same as `exec`, but pipes `input` to the command's stdin
(binary-safe, no argument-size limit, no base64):

```rust
pub async fn exec_with_input(
    target: Target<'_>,
    command: &str,
    input: &[u8],
    timeout: std::time::Duration,
) -> ExecOutput
```

Implementation: spawn with `Stdio::piped()` for stdin; `write_all(input)` then close
stdin; collect stdout/stderr under the timeout. Local path uses `sh -c`; remote
rides the same ControlMaster as `exec` (same `exec_args` / `wait_for_master` flow).

### New `save` command in `core/src/files.rs`

The remote command writes stdin to a temp file in the same directory, preserves the
original file's mode, then atomically moves it over the target:

```sh
p=<quoted path>
dir=$(dirname "$p"); tmp=$(mktemp "$dir/.ism-XXXXXX")
cat > "$tmp"
chmod --reference="$p" "$tmp" 2>/dev/null
mv -f "$tmp" "$p"
```

`chmod --reference` is GNU-specific; this is consistent with the existing `list`
command, which already assumes GNU `find -printf`. `mktemp` in the same directory
guarantees `mv` is an atomic rename (same filesystem).

### New `save` handler in `core/src/files.rs`

`files::save` — `POST /api/servers/{id}/files/save?path=…`, request body = raw text
bytes (`content-type: text/plain; charset=utf-8`):

1. Validate target via the existing `target()` helper; reject if invalid.
2. Reject if `path` is empty.
3. `stat` the file via the existing `stat_path`:
   - Reject if `kind == "dir"`.
   - Reject if `size > TEXT_LIMIT` (defense-in-depth; the frontend also enforces this).
4. Enforce a body-size guard `body.len() <= TEXT_LIMIT` server-side.
5. **Local:** write `body` to a temp file in the same directory, copy the original
   file's mode onto the temp, then `tokio::fs::rename` (atomic).
6. **Remote:** `ssh::exec_with_input(remote, &save_command(path), body, 20s)`.
7. Return `Json({ "ok": true, "saved": path })` on success, or
   `(StatusCode::BAD_REQUEST, Json({ "error": msg }))` on failure (matching the
   error shape used by `list` / `view`).

### New route in `core/src/lib.rs`

One line, next to the existing files routes (`post` is already imported):

```rust
.route("/api/servers/{id}/files/save", post(files::save))
```

### `view` response change

The `/files/view` response (`FileView` JSON) gains an `editable: boolean` field
computed by the backend from the editable rule above. The frontend uses it to decide
whether to render the editor or the read-only view. (The backend already runs a
`stat` and a base64 read in `view`; computing `editable` reuses that work — no extra
remote round-trip.)

**UTF-8 subtlety:** the existing `view` code decodes the file with
`String::from_utf8_lossy`, which *always* succeeds (bad bytes become U+FFFD). That
lossy text is fine for read-only display, but it must **not** be treated as
editable — saving lossy text would corrupt the original bytes. So `editable` is
computed via a **strict** UTF-8 check (`std::str::from_utf8(&bytes).is_ok()`) on the
same decoded bytes, independent of the lossy decode that fills the `text` field. A
text file that fails the strict check is returned as `type: "text"` (lossy text for
read-only display) with `editable: false`; the frontend renders it read-only with the
"not editable: not UTF-8" note.

### Permission preservation

Atomic `mv` / `rename` replaces the inode, so the file's mode would otherwise reset
to the umask (a `755` script could become `644`). We preserve the original mode:
locally via Rust's portable `std::fs::Permissions` API copied from the original onto
the temp before rename; remotely via `chmod --reference="$p"`. Tradeoff accepted:
`chmod --reference` is GNU-specific, but the remote `list` already assumes GNU
`find -printf`, so the platform assumption is unchanged.

## Frontend: editor + autosave

### New dependency

Add to `web/package.json`:
- `codemirror` (meta-package, pulls `basicSetup`).
- `@codemirror/theme-one-dark`.
- `@codemirror/lang-*`: rust, python, javascript, json, html, css, markdown, yaml,
  sql.
- `@codemirror/legacy-modes` (for shell, via `StreamLanguage`).

CodeMirror 6 is tree-shakeable; only the imported language packages are bundled.
Language is chosen by file extension; unknown extensions fall back to plain text
(still fully editable, just unhighlighted).

### `web/src/lib/api.ts`

```ts
export const saveFile = (id: string, path: string, content: string): Promise<Response> =>
  fetch(`${base(id)}/files/save?path=${enc(path)}`, {
    method: 'POST',
    headers: { 'content-type': 'text/plain; charset=utf-8' },
    body: content,
  }).then(ok)
```

`FileView` gains an optional `editable?: boolean` field.

### `web/src/lib/Editor.svelte` (new, thin wrapper)

Mounts a CodeMirror `EditorView` into a `<div>` with:
- `basicSetup` (line numbers, history, find, etc.).
- `oneDark` theme (matches the app's dark palette: `--bg #0b0c0e`, `--ink #e9eaec`).
- `EditorView.lineWrapping` (matches today's `pre-wrap` preview behavior).
- A `lang` prop selecting the language extension by extension.
- A `text` prop for the initial document.
- An `onchange` callback emitting the new doc on each transaction that changes it.

The editor is created once on mount; subsequent file switches update its document
via a transaction rather than recreating the editor.

### `web/src/lib/Files.svelte` changes

- Render `Editor.svelte` when `preview.type === 'text' && preview.editable`.
- Everything else (image / too_large / unsupported / non-UTF-8 read-only note) stays
  as today.
- **Autosave state machine** (debounce ~800 ms):
  - On a content change: `dirty = true`; (re)start a debounce timer.
  - When the timer fires: if no save is in flight, call `saveFile` and set status
    `saving…`. If a save is already in flight, set `pending = true` instead.
  - On save success: `dirty = false`, status `saved HH:MM:SS`. If `pending`, fire
    another save with the latest doc.
  - On save failure: status `⚠ save failed: <err>`, keep the edits in the editor,
    `dirty` stays true, show a small `retry save` button. Do not discard edits.
  - Only one save in flight at a time.
- **Switching files or changing host flushes the pending save first**: `open()`
  and `load()` await any in-flight or pending save (draining the debounce timer
  immediately) before the editor is destroyed. `load()` flushing covers the "up"
  button and directory navigation too. This means edits are never silently lost —
  without a confirm dialog, matching the autosave choice.
  - **Accepted edge (confirmed 2026-07-27):** the one exception is a save that has
    *already failed*. `flushSave` returns early when `saveError` is set, so the
    switch proceeds and the failed file's edits are discarded without a notice —
    the user must hit `retry` before switching. This is deliberate: forcing a
    retry or warning on switch would reintroduce the blocking/confirm behavior the
    autosave choice rejected. The `⚠ save failed: <err> [retry]` status is the
    signal. Do not add a confirm dialog or a switch-blocking gate here.
- A programmatic content set on file-switch is suppressed from triggering a save
  via a `loading` guard, so loading a file is not mistaken for an edit.
- **Status line in `pv-head`**: `● unsaved` / `saving…` / `saved 12:03:45` /
  `⚠ save failed: <err> [retry]`.

## Testing & verification

This project has no frontend test runner; CI runs `npm run build` +
`npm run check` (svelte-check typecheck) for the web crate and `cargo test -p core`
for the backend.

### Backend (`cargo test -p core`)

- `exec_with_input(Local, "cat", b"hello", …)` → `stdout == "hello"` — real test of
  the new primitive via local `sh`.
- `save_command(...)` string-shape unit test (mirrors the existing
  `parse_listing` / `parse_stat` test style).
- `atomic_write_local(path, bytes)` extracted as a pure helper → unit test in a std
  temp dir: writes a temp, renames, asserts final content == `bytes` and the
  destination mode equals the original mode.

### Frontend

- `cd web && npm run build` and `npm run check` pass (typecheck + build).
- Manual verification of the autosave flow on a local file: edit → wait → confirm
  file on disk changed; switch file mid-edit → confirm first file saved; break the
  save (e.g. read-only file) → confirm `save failed` status and `retry` button.

### Verify loop

`cargo test -p core` green **and** `cd web && npm run build && npm run check` green.

## Files touched

- `core/src/ssh.rs` — add `exec_with_input`.
- `core/src/files.rs` — add `save` command + handler; extend `view` to return
  `editable`; extract `atomic_write_local` helper for the local path; new tests.
- `core/src/lib.rs` — one new `.route(...)` line.
- `web/package.json` — add CodeMirror packages.
- `web/src/lib/api.ts` — add `saveFile`; extend `FileView` with `editable`.
- `web/src/lib/Editor.svelte` — new thin CodeMirror wrapper.
- `web/src/lib/Files.svelte` — render editor; autosave state machine; status line.
