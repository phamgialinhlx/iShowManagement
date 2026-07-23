# Editable Files Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make text files in the Files browser editable in a CodeMirror 6 pane with debounced autosave back to local disk or a remote host over ssh, via an atomic write.

**Architecture:** Backend gains a `POST /files/save` route that writes the request body to a temp file in the same directory (preserving the original mode) and atomically renames it over the target — locally via `tokio::fs`, remotely by piping stdin over `ssh exec`. The existing `/files/view` route gains an `editable` flag (text + valid UTF-8 + ≤512KB). The frontend swaps the read-only `<pre>` for a thin CodeMirror wrapper and runs a debounce-based autosave state machine.

**Tech Stack:** Rust (axum 0.8, tokio), Svelte 5 (runes), TypeScript, CodeMirror 6.

## Global Constraints

- **Build web before any `cargo` command.** `core` embeds `../web/dist` via `rust-embed`; the folder is gitignored and absent on a fresh checkout, so `cargo test -p core` / `cargo build` fail to compile without it. Before Task 1, run once: `cd web && npm install && npm run build`. Re-run after frontend changes (Tasks 5–7) before re-running `cargo`.
- axum **0.8**. The raw-body extractor is `axum::body::Bytes` (see `core/src/browser.rs:38` for the pattern). axum's default body limit is 2 MB; our 512 KB cap is well under it — no body-limit config needed.
- `TEXT_LIMIT` is the existing `const TEXT_LIMIT: u64 = 512 * 1024;` in `core/src/files.rs:24`. Reuse it; do not redefine.
- `shell_quote` is `pub fn shell_quote(s: &str) -> String` in `core/src/security.rs`, imported as `q` in `files.rs`. It single-quotes its argument.
- `ssh::exec(target, command, timeout) -> ExecOutput { ok, stdout, stderr }` rides the console's ControlMaster. `Target` is `Local | Remote(&str)`. `wait_for_master` and `exec_args` are `pub(crate)`.
- App theme is **dark** (`--bg #0b0c0e`, `--ink #e9eaec`); use CodeMirror's `oneDark`. Mono font token is `var(--font-mono)` (Lilex).
- Frontend has **no test runner**. Web verification = `cd web && npm run check` (svelte-check, strict — `checkJs: true`) **and** `npm run build`. Backend verification = `cargo test -p core`.
- The project's backend test style is **pure-helper unit tests** (see `files.rs` `mod tests`: `parse_listing`, `parse_stat`). Handlers are thin glue verified by `cargo build`; do not introduce an axum test harness.
- One commit per task, ending with the `Co-Authored-By: Claude <noreply@anthropic.com>` trailer.
- This is a macOS-only app (`keyring`), but the new tests use `Target::Local` so they run on any Unix.

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `core/src/ssh.rs` | one-shot ssh/local command exec | add `exec_with_input` (stdin pipe) |
| `core/src/files.rs` | file list/stat/preview/download + new save | add `save_command`, `save` handler, `atomic_write_local`, `decode_text`, `is_editable`; extend `view` with `editable` |
| `core/src/lib.rs` | route wiring | one new `.route(...)` line |
| `web/package.json` | web deps | add CodeMirror 6 packages |
| `web/src/lib/api.ts` | typed REST client | add `saveFile`; `FileView.editable` |
| `web/src/lib/Editor.svelte` | thin CodeMirror 6 wrapper | new file |
| `web/src/lib/Files.svelte` | files browser UI | render editor, autosave state machine, status line |

---

### Task 1: `exec_with_input` primitive in `ssh.rs`

A version of `exec` that pipes bytes to the command's stdin (binary-safe, no argument-size limit, no base64). Used by the remote save path. Tested via `Target::Local` + `cat`.

**Files:**
- Modify: `core/src/ssh.rs` (add `exec_with_input` after `exec`, ending at line 192; add a test inside the existing `mod tests` at line 195)

**Interfaces:**
- Consumes: `Target`, `exec_args`, `wait_for_master` (all in this file)
- Produces: `pub async fn exec_with_input(target: Target<'_>, command: &str, input: &[u8], timeout: std::time::Duration) -> ExecOutput` — same return shape as `exec`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` (after the `tmux_remote_is_attach_or_create` test, before the closing `}` of `mod tests` at line 230):

```rust
    #[tokio::test]
    async fn exec_with_input_pipes_stdin_to_cat() {
        let out = exec_with_input(
            Target::Local,
            "cat",
            b"hello world",
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(out.ok, "stderr: {}", out.stderr);
        assert_eq!(out.stdout, "hello world");
    }

    #[tokio::test]
    async fn exec_with_input_survives_binary_and_large_payload() {
        // 200 KB of arbitrary bytes incl. NULs and high bits — must round-trip intact.
        let mut bytes = Vec::with_capacity(200_000);
        let mut i = 0u32;
        while bytes.len() < 200_000 {
            bytes.push((i & 0xff) as u8);
            bytes.push(0);
            i = i.wrapping_add(7);
        }
        let out = exec_with_input(
            Target::Local,
            "cat > /dev/stdout",
            &bytes,
            std::time::Duration::from_secs(10),
        )
        .await;
        assert!(out.ok, "stderr: {}", out.stderr);
        assert_eq!(out.stdout.as_bytes(), &bytes[..]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p core --lib ssh::tests::exec_with_input_pipes_stdin_to_cat`
Expected: FAIL — `cannot find function exec_with_input` (compile error).

- [ ] **Step 3: Implement `exec_with_input`**

Add immediately after the end of `exec` (after line 192, before `#[cfg(test)]`):

```rust
/// Like [`exec`], but pipes `input` to the command's stdin. Binary-safe and
/// uncoupled from the argument-length limit, so it can move a file's worth of
/// bytes (the save path). Local runs `sh -c`; remote rides the ControlMaster.
pub async fn exec_with_input(
    target: Target<'_>,
    command: &str,
    input: &[u8],
    timeout: std::time::Duration,
) -> ExecOutput {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;
    let mut cmd = match target {
        Target::Local => {
            let mut c = Command::new("sh");
            c.arg("-c").arg(command);
            c
        }
        Target::Remote(alias) => {
            wait_for_master(alias, std::time::Duration::from_secs(3)).await;
            let mut c = Command::new("ssh");
            for a in exec_args(alias) {
                c.arg(a);
            }
            c.arg(command); // ssh runs this as the remote command
            c
        }
    };
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ExecOutput {
                ok: false,
                stdout: String::new(),
                stderr: e.to_string(),
            }
        }
    };
    // Write the payload, then drop stdin so the command sees EOF.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input).await;
    }
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => ExecOutput {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Ok(Err(e)) => ExecOutput {
            ok: false,
            stdout: String::new(),
            stderr: e.to_string(),
        },
        Err(_) => ExecOutput {
            ok: false,
            stdout: String::new(),
            stderr: "command timed out".into(),
        },
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p core --lib ssh::tests`
Expected: PASS — both new `exec_with_input_*` tests pass alongside the existing ssh tests.

- [ ] **Step 5: Commit**

```bash
git add core/src/ssh.rs
git commit -m "feat(ssh): add exec_with_input to pipe stdin over ssh exec

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: Save-path helpers in `files.rs` (TDD)

Three pure-ish helpers the `save` and `view` handlers build on. `atomic_write_local` is the local save (temp + preserve-mode + atomic rename). `decode_text` returns lossy text plus a strict-UTF-8 flag. `is_editable` encodes the editable rule.

**Files:**
- Modify: `core/src/files.rs` (add helpers in the `// ---- util ----` section near line 490; add tests in `mod tests`)

**Interfaces:**
- Consumes: `TEXT_LIMIT`, `is_text_preview`, `Stat` (this file), `q` (security)
- Produces:
  - `async fn atomic_write_local(path: &str, bytes: &[u8]) -> Result<(), String>`
  - `fn decode_text(bytes: &[u8]) -> (String, bool)` — `(lossy_text, valid_utf8)`
  - `fn is_editable(stat: &Stat, valid_utf8: bool) -> bool`
  - `fn save_command(target_path: &str) -> String`

- [ ] **Step 1: Write the failing tests**

Add inside `mod tests` (after the `safe_download_name_strips_separators` test):

```rust
    #[tokio::test]
    async fn atomic_write_local_replaces_content_and_preserves_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ism-awt-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("target.txt");
        tokio::fs::write(&path, b"old").await.unwrap();
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
        atomic_write_local(&path.to_string_lossy(), b"new content")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "new content");
        let mode = tokio::fs::metadata(&path).await.unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode should be preserved across rename");
        // No leftover temp file in the directory.
        let left: Vec<_> = tokio::fs::read_dir(&dir).await.unwrap().collect::<Result<_, _>>().await.unwrap();
        assert_eq!(left.len(), 1, "only the target should remain");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn decode_text_valid_and_invalid_utf8() {
        let (text, ok) = decode_text(b"hello");
        assert_eq!(text, "hello");
        assert!(ok);
        let (text, ok) = decode_text(&[0xff, 0xfe, 0xfd]);
        assert!(!ok);
        assert!(text.contains('\u{fffd}'), "lossy text has replacement char");
    }

    #[test]
    fn is_editable_enforces_all_conditions() {
        let mk = |kind: &str, size: u64, name: &str, mime: &str| Stat {
            kind: kind.into(),
            size,
            mime: mime.into(),
            name: name.into(),
            path: name.into(),
        };
        assert!(is_editable(&mk("file", 100, "a.txt", "text/plain"), true));
        assert!(!is_editable(&mk("dir", 100, "a", "text/plain"), true), "dirs not editable");
        assert!(
            !is_editable(&mk("file", TEXT_LIMIT + 1, "a.txt", "text/plain"), true),
            "over limit not editable"
        );
        assert!(
            !is_editable(&mk("file", 100, "a.bin", "application/octet-stream"), true),
            "binary not editable"
        );
        assert!(
            !is_editable(&mk("file", 100, "a.txt", "text/plain"), false),
            "invalid utf8 not editable"
        );
    }

    #[test]
    fn save_command_shape() {
        let cmd = save_command("/home/u/notes.txt");
        assert!(cmd.contains("/home/u/notes.txt"), "path present: {cmd}");
        assert!(cmd.contains("mktemp"), "writes to a temp: {cmd}");
        assert!(cmd.contains("cat > "), "reads stdin into temp: {cmd}");
        assert!(cmd.contains("chmod --reference="), "preserves mode: {cmd}");
        assert!(cmd.contains("mv -f "), "atomic rename: {cmd}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p core --lib files::tests`
Expected: FAIL — `cannot find function atomic_write_local` / `decode_text` / `is_editable` / `save_command`.

- [ ] **Step 3: Implement the helpers**

Add in the `// ---- util ----` section (near line 490, alongside `trim300` / `err`):

```rust
/// Decode bytes as text for display: the lossy string plus whether the input
/// was *strictly* valid UTF-8. The strict flag drives editability — saving
/// lossy text would corrupt the original bytes, so invalid-UTF-8 files are
/// shown read-only even when they look like text.
fn decode_text(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), true),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), false),
    }
}

/// A file is editable when it's a regular text file under the size limit with
/// valid UTF-8 (see the spec's editable rule).
fn is_editable(stat: &Stat, valid_utf8: bool) -> bool {
    stat.kind == "file"
        && stat.size <= TEXT_LIMIT
        && is_text_preview(&stat.name, &stat.mime)
        && valid_utf8
}

/// The remote save command: stream stdin into a temp in the same directory
/// (so the `mv` is an atomic rename), preserve the original mode, then move
/// it over the target. `chmod --reference` is GNU — consistent with `list`'s
/// `find -printf` assumption.
fn save_command(target_path: &str) -> String {
    [
        format!("p={}", q(target_path)),
        r#"dir=$(dirname "$p")"#.into(),
        r#"tmp=$(mktemp "$dir/.ism-XXXXXX")"#.into(),
        r#"cat > "$tmp""#.into(),
        r#"chmod --reference="$p" "$tmp" 2>/dev/null"#.into(),
        r#"mv -f "$tmp" "$p""#.into(),
    ]
    .join("; ")
}

/// Write `bytes` to `path` atomically on the local machine: a temp file in the
/// same directory, the original mode copied onto it, then an atomic rename.
/// Assumes `path` already exists (the caller stats it first).
async fn atomic_write_local(path: &str, bytes: &[u8]) -> Result<(), String> {
    let p = std::path::Path::new(path);
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    let base = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".ism-{}-{}-{}", std::process::id(), n, base));
    tokio::fs::write(&tmp, bytes).await.map_err(|e| e.to_string())?;
    // Preserve the original mode across the inode swap.
    let perms = tokio::fs::metadata(path).await.map_err(|e| e.to_string())?.permissions();
    tokio::fs::set_permissions(&tmp, perms).await.map_err(|e| e.to_string())?;
    tokio::fs::rename(&tmp, path).await.map_err(|e| e.to_string())?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p core --lib files::tests`
Expected: PASS — all four new tests pass alongside the existing `parse_listing`, `parse_stat`, `preview_type_detection`, `safe_download_name_strips_separators`.

- [ ] **Step 5: Commit**

```bash
git add core/src/files.rs
git commit -m "feat(files): add save helpers (atomic local write, decode, editable rule)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: `files::save` handler + route

Wire the helpers into a `POST /files/save` handler that validates, then writes locally or pipes to the remote save command. Register the route.

**Files:**
- Modify: `core/src/files.rs` (add `save` handler near `view`, after line 293)
- Modify: `core/src/lib.rs:149` (add one route line after the `files/download` route)

**Interfaces:**
- Consumes: `exec_with_input` (Task 1), `atomic_write_local` / `save_command` (Task 2), `target`, `is_local`, `stat_path`, `err`, `trim300`, `TEXT_LIMIT`, `Target`, `ssh`
- Produces: `pub async fn save(...) -> Result<Json<Value>, (StatusCode, Json<Value>)>` and the route `POST /api/servers/{id}/files/save`

- [ ] **Step 1: Add the `save` handler**

Add after the `view` handler (after line 293, before `fn with_type`):

```rust
// ------------------------------------------------------------ save ----

/// `POST /api/servers/{id}/files/save?path=…` — body is the new file contents
/// (text/plain, UTF-8). Writes atomically: locally via temp+rename, remotely by
/// piping stdin into the save command. Rejects directories and oversized files.
pub async fn save(
    State(_): State<AppState>,
    Path(id): Path<String>,
    Query(pq): Query<PathQuery>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let tgt = target(&id).ok_or(err(StatusCode::BAD_REQUEST, "bad server id"))?;
    if pq.path.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "path is required"));
    }
    if body.len() as u64 > TEXT_LIMIT {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "file too large to edit"));
    }
    let stat = stat_path(tgt, &pq.path).await.map_err(|e| err(StatusCode::BAD_REQUEST, &e))?;
    if stat.kind == "dir" {
        return Err(err(StatusCode::BAD_REQUEST, "cannot save a directory"));
    }
    if stat.size > TEXT_LIMIT {
        return Err(err(StatusCode::BAD_REQUEST, "file too large to edit"));
    }
    let bytes = body.to_vec();
    let result = if is_local(&id) {
        atomic_write_local(&stat.path, &bytes).await
    } else {
        let r = ssh::exec_with_input(tgt, &save_command(&pq.path), &bytes, Duration::from_secs(20)).await;
        if r.ok {
            Ok(())
        } else {
            Err(trim300(if r.stderr.trim().is_empty() { "save failed" } else { &r.stderr }))
        }
    };
    match result {
        Ok(()) => Ok(Json(json!({ "ok": true, "saved": stat.path }))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": trim300(&e) })))),
    }
}
```

- [ ] **Step 2: Register the route**

In `core/src/lib.rs`, change the block at lines 147–149 from:

```rust
        .route("/api/servers/{id}/files", get(files::list))
        .route("/api/servers/{id}/files/view", get(files::view))
        .route("/api/servers/{id}/files/download", get(files::download))
```

to:

```rust
        .route("/api/servers/{id}/files", get(files::list))
        .route("/api/servers/{id}/files/view", get(files::view))
        .route("/api/servers/{id}/files/download", get(files::download))
        .route("/api/servers/{id}/files/save", post(files::save))
```

(`post` is already imported at `lib.rs:31`.)

- [ ] **Step 3: Verify it compiles and tests pass**

Run: `cargo test -p core`
Expected: PASS — compiles; all existing + Task 1/2 tests still green. (The handler itself is glue; its logic is covered by the `atomic_write_local`, `exec_with_input`, and `save_command` unit tests. A real end-to-end save is verified manually in Task 7.)

- [ ] **Step 4: Commit**

```bash
git add core/src/files.rs core/src/lib.rs
git commit -m "feat(files): POST /files/save — atomic write of edited content

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: `view` returns the `editable` flag

Switch the text branch of `view` from lossy-only decode to `decode_text` + `is_editable`, and set `editable: true` on the JSON when the file is editable. Other branches (`image`, `too_large`, `unsupported`) leave `editable` absent (falsy on the client).

**Files:**
- Modify: `core/src/files.rs` (the text branch of `view`, lines 288–292)

**Interfaces:**
- Consumes: `decode_text`, `is_editable` (Task 2)
- Produces: `view`'s JSON gains `"editable": true` on the text branch when editable

- [ ] **Step 1: Replace the text branch**

Change lines 288–292 from:

```rust
    // Text: decode base64 to bytes → utf8 (lossy).
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(cleaned.as_bytes()).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Json(with_type(base, "text", Some(text))))
```

to:

```rust
    // Text: decode base64 → bytes → utf8 (lossy for display, strict flag for editability).
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(cleaned.as_bytes()).unwrap_or_default();
    let (text, valid) = decode_text(&bytes);
    let mut v = with_type(base, "text", Some(text));
    if is_editable(&stat, valid) {
        v["editable"] = json!(true);
    }
    Ok(Json(v))
```

- [ ] **Step 2: Verify it compiles and tests pass**

Run: `cargo test -p core`
Expected: PASS — compiles; existing tests green. (The editable computation is covered by the `decode_text` and `is_editable` unit tests in Task 2.)

- [ ] **Step 3: Commit**

```bash
git add core/src/files.rs
git commit -m "feat(files): view reports editability for text files

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: Frontend deps + `saveFile` + `FileView.editable`

Add the CodeMirror 6 packages, a `saveFile` client function, and the `editable` field to the `FileView` type.

**Files:**
- Modify: `web/package.json` (add to `dependencies`)
- Modify: `web/src/lib/api.ts` (the `FileView` interface at line 195, and the `-- Files --` section at line 209)

**Interfaces:**
- Consumes: the new `POST /files/save` route (Task 3), the `editable` field from `view` (Task 4)
- Produces: `saveFile(id, path, content)`, `FileView.editable?: boolean`

- [ ] **Step 1: Add CodeMirror dependencies**

Replace the `dependencies` block in `web/package.json` with:

```json
  "dependencies": {
    "@codemirror/lang-css": "^6.0.0",
    "@codemirror/lang-html": "^6.0.0",
    "@codemirror/lang-javascript": "^6.0.0",
    "@codemirror/lang-json": "^6.0.0",
    "@codemirror/lang-markdown": "^6.0.0",
    "@codemirror/lang-python": "^6.0.0",
    "@codemirror/lang-rust": "^6.0.0",
    "@codemirror/lang-sql": "^6.0.0",
    "@codemirror/lang-yaml": "^6.0.0",
    "@codemirror/language": "^6.0.0",
    "@codemirror/legacy-modes": "^6.0.0",
    "@codemirror/state": "^6.0.0",
    "@codemirror/theme-one-dark": "^6.0.0",
    "@codemirror/view": "^6.0.0",
    "@fontsource/geist-mono": "^5.3.0",
    "@fontsource/geist-sans": "^5.3.0",
    "@fontsource/lilex": "^5.3.0",
    "@xterm/addon-fit": "^0.11.0",
    "@xterm/xterm": "^6.0.0",
    "codemirror": "^6.0.0"
  }
```

- [ ] **Step 2: Extend `FileView` and add `saveFile`**

In `web/src/lib/api.ts`, change the `FileView` interface (lines 195–204) from:

```ts
export interface FileView {
  type: 'text' | 'image' | 'too_large' | 'unsupported'
  name: string
  path: string
  size: number
  mime: string
  text?: string
  dataUrl?: string
  limit?: number
}
```

to:

```ts
export interface FileView {
  type: 'text' | 'image' | 'too_large' | 'unsupported'
  name: string
  path: string
  size: number
  mime: string
  text?: string
  dataUrl?: string
  limit?: number
  editable?: boolean
}
```

Then, after the `downloadFileUrl` export (line 213) and before the `// -- Forward + Browser` comment, add:

```ts
export const saveFile = (id: string, path: string, content: string): Promise<Response> =>
  fetch(`${base(id)}/files/save?path=${enc(path)}`, {
    method: 'POST',
    headers: { 'content-type': 'text/plain; charset=utf-8' },
    body: content,
  }).then(ok)
```

- [ ] **Step 3: Install and verify the typecheck/build**

Run: `cd web && npm install && npm run check && npm run build`
Expected: PASS — `npm install` adds the CodeMirror packages; `npm run check` (svelte-check) passes; `npm run build` produces `web/dist` (which core embeds). (No code imports CodeMirror yet — that comes in Task 6 — so there are no unused-import errors; the deps are simply available.)

- [ ] **Step 4: Commit**

```bash
git add web/package.json web/package-lock.json web/src/lib/api.ts
git commit -m "feat(web): add CodeMirror 6 deps, saveFile client, FileView.editable

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 6: `Editor.svelte` — thin CodeMirror 6 wrapper

A focused component that mounts one CodeMirror view with `basicSetup` + `oneDark` + line wrapping, a language picked from the file name, and an `onchange` callback. Mounted once per file (the parent keys by path, so it is created fresh and destroyed cleanly on each file switch — mirroring `Terminal.svelte`'s `onMount`/cleanup pattern).

**Files:**
- Create: `web/src/lib/Editor.svelte`

**Interfaces:**
- Consumes: the CodeMirror packages from Task 5
- Produces: a component with props `{ text: string; name: string; onchange?: (value: string) => void }`

- [ ] **Step 1: Create `Editor.svelte`**

Create `web/src/lib/Editor.svelte` with:

```svelte
<script lang="ts">
  import { onMount } from 'svelte'
  import { basicSetup } from 'codemirror'
  import { EditorView } from '@codemirror/view'
  import { EditorState, type Extension } from '@codemirror/state'
  import { StreamLanguage } from '@codemirror/language'
  import { oneDark } from '@codemirror/theme-one-dark'
  import { rust } from '@codemirror/lang-rust'
  import { python } from '@codemirror/lang-python'
  import { javascript } from '@codemirror/lang-javascript'
  import { json } from '@codemirror/lang-json'
  import { html } from '@codemirror/lang-html'
  import { css } from '@codemirror/lang-css'
  import { markdown } from '@codemirror/lang-markdown'
  import { yaml } from '@codemirror/lang-yaml'
  import { sql } from '@codemirror/lang-sql'
  import { shell } from '@codemirror/legacy-modes/mode/shell'

  interface Props {
    text: string
    name: string
    onchange?: (value: string) => void
  }
  let { text, name, onchange }: Props = $props()

  let host: HTMLDivElement
  let view: EditorView | undefined

  // Pick a language extension by file extension. Unknown → no highlighting
  // (still fully editable). Returns an array so the caller can spread it.
  function langFor(name: string): Extension[] {
    const ext = name.split('.').pop()?.toLowerCase() ?? ''
    switch (ext) {
      case 'rs': return [rust()]
      case 'py': return [python()]
      case 'js': case 'mjs': case 'cjs': return [javascript()]
      case 'ts': return [javascript({ typescript: true })]
      case 'jsx': return [javascript({ jsx: true })]
      case 'tsx': return [javascript({ jsx: true, typescript: true })]
      case 'json': return [json()]
      case 'html': case 'htm': return [html()]
      case 'css': return [css()]
      case 'md': case 'markdown': return [markdown()]
      case 'yaml': case 'yml': return [yaml()]
      case 'sql': return [sql()]
      case 'sh': case 'bash': case 'zsh': case 'conf': case 'env': case 'toml':
        return [StreamLanguage.define(shell)]
      default: return []
    }
  }

  onMount(() => {
    view = new EditorView({
      state: EditorState.create({
        doc: text,
        extensions: [
          basicSetup,
          oneDark,
          EditorView.lineWrapping,
          ...langFor(name),
          EditorView.updateListener.of((u) => {
            if (u.docChanged && onchange) onchange(u.state.doc.toString())
          }),
        ],
      }),
      parent: host,
    })
    return () => view?.destroy()
  })
</script>

<div class="cm-host" bind:this={host}></div>

<style>
  .cm-host {
    height: 100%;
  }
  :global(.cm-host .cm-editor) {
    height: 100%;
  }
  :global(.cm-host .cm-scroller) {
    font: 12px/1.6 var(--font-mono);
  }
</style>
```

- [ ] **Step 2: Verify the typecheck/build**

Run: `cd web && npm run check && npm run build`
Expected: PASS — `Editor.svelte` typechecks (note: it is not yet imported anywhere, so svelte-check still compiles it as a standalone module). If `npm run check` reports an unused-component warning, ignore it; it is imported in Task 7.

- [ ] **Step 3: Commit**

```bash
git add web/src/lib/Editor.svelte
git commit -m "feat(web): add Editor.svelte — thin CodeMirror 6 wrapper

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 7: Autosave integration in `Files.svelte`

Render the `Editor` for editable text files, run the debounced autosave state machine, flush pending saves on file/host switch, and show a save status line.

**Files:**
- Modify: `web/src/lib/Files.svelte` (the whole `<script>`, the `.pv-head` and `.pv-body` blocks in the template, and add a few styles)

**Interfaces:**
- Consumes: `Editor.svelte` (Task 6), `saveFile` + `FileView.editable` (Task 5), the `POST /files/save` route (Task 3)
- Produces: editable file pane with autosave

- [ ] **Step 1: Replace the `<script>` block**

Replace the entire `<script lang="ts"> … </script>` block (lines 1–66) with:

```svelte
<script lang="ts">
  import { untrack } from 'svelte'
  import {
    listFiles,
    viewFile,
    saveFile,
    downloadFileUrl,
    type Listing,
    type FileEntry,
    type FileView,
  } from './api'
  import Editor from './Editor.svelte'

  interface Props {
    id: string
  }
  let { id }: Props = $props()

  let listing = $state<Listing>()
  let preview = $state<FileView>()
  let error = $state('')
  let loading = $state(false)

  // --- autosave state for the editable text file currently open ---
  let currentPath = $state('') // path loaded into the editor
  let currentId = $state('') // host id of that file (so a host switch saves to the right host)
  let currentText = $state('') // latest editor text (what the next save writes)
  let dirty = $state(false) // unsaved edits exist
  let saving = $state(false) // a save is in flight
  let saveStatus = $state('') // '' | 'unsaved' | 'saving…' | 'saved HH:MM:SS' | '⚠ save failed: …'
  let saveError = $state('')
  let loadingFile = $state(false) // suppresses save during a programmatic load
  let debounceTimer: ReturnType<typeof setTimeout> | undefined
  let inFlight: Promise<void> | null = null

  function fmtSize(n: number): string {
    if (!n) return ''
    const u = ['B', 'K', 'M', 'G']
    let i = 0
    while (n >= 1024 && i < u.length - 1) {
      n /= 1024
      i++
    }
    return `${i === 0 ? n : n.toFixed(1)}${u[i]}`
  }

  async function load(path = '') {
    loading = true
    error = ''
    preview = undefined
    try {
      listing = await listFiles(id, path)
    } catch (e) {
      error = String(e)
    } finally {
      loading = false
    }
  }

  async function open(entry: FileEntry) {
    if (entry.type === 'dir') {
      await load(entry.path)
      return
    }
    // Flush the file we're leaving before switching.
    await flushSave()
    loadingFile = true
    loading = true
    error = ''
    try {
      preview = await viewFile(id, entry.path)
      if (preview?.type === 'text') {
        currentId = id
        currentPath = preview.path
        currentText = preview.text ?? ''
        dirty = false
        saving = false
        saveError = ''
        saveStatus = ''
      }
    } catch (e) {
      error = String(e)
    } finally {
      loading = false
      loadingFile = false
    }
  }

  // Editor reported a change → mark dirty and (re)start the debounce timer.
  function onEdit(value: string) {
    currentText = value
    scheduleSave()
  }

  function scheduleSave() {
    if (loadingFile) return // ignore the "edit" that is just us loading a file
    dirty = true
    saveError = ''
    saveStatus = 'unsaved'
    if (debounceTimer) clearTimeout(debounceTimer)
    debounceTimer = setTimeout(runSave, 800)
  }

  // Save once, using the current text. If a save is already in flight, bail:
  // when it finishes it will see `dirty` still true (currentText differs from
  // what it saved) and reschedule, so the latest edits are not lost.
  async function runSave() {
    debounceTimer = undefined
    if (inFlight || !currentPath) return
    saving = true
    saveStatus = 'saving…'
    const saveId = currentId
    const path = currentPath
    const text = currentText // capture exactly what we are saving
    inFlight = (async () => {
      try {
        await saveFile(saveId, path, text)
        saveError = ''
      } catch (e) {
        saveError = String(e)
      }
    })()
    await inFlight
    inFlight = null
    saving = false
    if (saveError) {
      saveStatus = `⚠ save failed: ${saveError}`
      // dirty stays true so the retry button / next edit can re-attempt
    } else {
      // Only clear dirty if the editor did not move on while we were saving.
      dirty = currentText !== text
      saveStatus = dirty ? 'unsaved' : `saved ${clock()}`
    }
    if (dirty && !saveError) scheduleSave() // edits landed during the save
  }

  // Force pending edits to disk before switching files / hosts. Bypasses the
  // debounce and drains until quiet or an error.
  async function flushSave() {
    if (debounceTimer) {
      clearTimeout(debounceTimer)
      debounceTimer = undefined
    }
    while (true) {
      while (inFlight) await inFlight
      if (!dirty || !currentPath || saveError) return
      void runSave() // sets inFlight synchronously
    }
  }

  function clock(): string {
    return new Date().toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    })
  }

  // (Re)load when the target server changes. Flush the outgoing file first.
  // untrack keeps this effect dependent on `id` only, not the save state.
  $effect(() => {
    const _ = id
    untrack(() => {
      void flushSave().then(() => load(''))
    })
  })
</script>
```

- [ ] **Step 2: Replace the `.right` block in the template**

Replace the entire `<div class="right"> … </div>` block (lines 93–114) with:

```svelte
  <div class="right">
    {#if preview}
      <div class="pv-head">
        <span class="pv-name">{preview.name}</span>
        <span class="muted">{preview.mime} · {fmtSize(preview.size)}</span>
        {#if preview.type === 'text' && preview.editable}
          <span
            class="save-status"
            class:unsaved={dirty && !saving && !saveError}
            class:saving={saving}
            class:err={!!saveError}
          >
            {saveStatus}
            {#if saveError}
              <button class="retry" onclick={() => void runSave()}>retry</button>
            {/if}
          </span>
        {/if}
        <a class="dl" href={downloadFileUrl(id, preview.path)}>download</a>
      </div>
      <div class="pv-body">
        {#if preview.type === 'text'}
          {#if preview.editable}
            {#key preview.path}
              <Editor text={preview.text} name={preview.name} onchange={onEdit} />
            {/key}
          {:else}
            <pre>{preview.text}</pre>
            <div class="muted ro-note">not editable: not UTF-8 — download to edit externally.</div>
          {/if}
        {:else if preview.type === 'image'}
          <img src={preview.dataUrl} alt={preview.name} />
        {:else if preview.type === 'too_large'}
          <div class="muted pad">Too large to preview ({fmtSize(preview.size)}). Use download.</div>
        {:else}
          <div class="muted pad">No preview for this file type. Use download.</div>
        {/if}
      </div>
    {:else}
      <div class="muted pad">Select a file to preview.</div>
    {/if}
  </div>
```

- [ ] **Step 3: Add the new styles**

Add inside the existing `<style>` block (before its closing `</style>` at line 230):

```svelte
  .save-status {
    margin-left: 0.5rem;
    font-size: 11.5px;
    font-family: var(--font-mono);
    color: var(--ink-faint);
  }
  .save-status.unsaved {
    color: var(--accent);
  }
  .save-status.saving {
    color: var(--ink-dim);
  }
  .save-status.err {
    color: var(--danger);
  }
  .retry {
    margin-left: 0.4rem;
    background: none;
    border: 1px solid var(--danger);
    color: var(--danger);
    border-radius: 6px;
    padding: 0.05rem 0.4rem;
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .retry:hover {
    background: rgba(211, 121, 111, 0.12);
  }
  .ro-note {
    padding: 0.4rem 0.9rem 0;
    font-size: 12px;
  }
```

- [ ] **Step 4: Verify the typecheck/build**

Run: `cd web && npm run check && npm run build`
Expected: PASS — `Files.svelte` typechecks and builds; `web/dist` is regenerated.

- [ ] **Step 5: Verify the full stack builds and backend tests pass**

Run: `cargo test -p core`
Expected: PASS — core re-embeds the freshly built `web/dist` and all tests pass.

- [ ] **Step 6: Manual verification of the autosave flow**

Run the app (e.g. `cargo run -p core` and open `http://127.0.0.1:7070`; use the `local` server), then confirm:
1. Click a small text file → it opens in the CodeMirror editor (highlighted, line numbers).
2. Type a change → status shows `unsaved`, then after ~800 ms `saving…`, then `saved HH:MM:SS`. Reopen the file (or check on disk) → the change persisted.
3. Mid-edit, click a different file → the first file saves before the second opens; no edits lost.
4. Make a file read-only on disk (`chmod 444`), edit, wait → status shows `⚠ save failed: …` with a `retry` button; edits stay in the editor.
5. An image / a >512 KB file / a binary file → still preview/download only, no editor.
6. A non-UTF-8 text file → shows read-only text with the `not editable: not UTF-8` note.

- [ ] **Step 7: Commit**

```bash
git add web/src/lib/Files.svelte
git commit -m "feat(web): editable Files pane with debounced autosave

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Notes for the implementer

- **`currentId` is load-bearing.** The autosave must save to the host the file came from, not whatever `id` is current when the save fires (a host switch would otherwise write the old file's content to the new host). Never replace `currentId` with `id` inside `runSave` / `flushSave`.
- **The dirty-check on save completion** (`dirty = currentText !== text`) is what makes autosave correct under concurrent edits: if the user typed during the save, `dirty` stays true and another save is scheduled. Do not simplify it to `dirty = false`.
- **`{#key preview.path}`** in the template is what makes the editor recreate cleanly on file switch (destroying the old CodeMirror view via the `onMount` cleanup). Do not remove it.
- **Switching hosts with an unsaved failed save** is an accepted edge: the failed file is left unsaved (the user must `retry` before switching). Do not add a confirm dialog (the spec chose autosave + no confirms).
- **Permission preservation** is GNU-only (`chmod --reference`), consistent with the existing `list` command's `find -printf` assumption. Do not add a BSD branch.
