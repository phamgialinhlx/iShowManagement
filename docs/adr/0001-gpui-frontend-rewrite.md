# 1. Replace the Tauri/webview frontend with a native gpui app

Date: 2026-08-11
Status: Accepted

## Context

rmux today is a Tauri app: ~10 UI-agnostic Rust crates (`rmux-ssh`,
`rmux-transport`, `rmux-fs`, `rmux-git`, `rmux-metrics`, `rmux-agent`,
`rmux-claude`, `rmux-control`, …) doing the real work, fronted by a React/
TypeScript UI running in a WKWebView. Terminals are xterm.js fed by a local PTY
over a Tauri `Channel`; editing is Monaco; appearance is a CSS glass/zoom system.

Three structural problems trace directly to the webview: performance/heat
(per-pane status polling, WindowServer compositing at 4K), and a long tail of
webview-only defects (backdrop-filter glass, `zoom`, the Vietnamese IME saga,
`localStorage` quota). The team also wants Zed-style **window management** — a
splittable pane/tab/dock workspace instead of the fixed 4×4 grid — and Zed's
native terminal/editor lineage.

## Decision

Replace `src-tauri` + `ui/` with a **native gpui application crate**. Keep the
backend crates as-is (they are the asset and encode the hard-won invariants),
**except `rmux-metrics`, which is dropped** along with the HOST/TOP-PROCESSES
widgets. The gpui app links the backend crates **directly as Rust functions** —
no IPC, no `Channel`, no JSON boundary; terminal bytes go straight from the local
PTY into the native terminal view.

Depth of Zed reuse is drawn as a line between Zed's **presentation stack** and
its **domain stack**:

- **Adopt, by copying into this repo** (vendored, pinned, owned): the **clean
  20-crate presentation core** — `gpui` + its internal closure (`gpui_macros`,
  `gpui_shared_string`, `gpui_util`, `collections`, `refineable`, `sum_tree`,
  `scheduler`, `http_client`), the `ui` cluster (`ui`, `ui_macros`, `icons`,
  `menu`, `component`), `theme` (+ `syntax_theme`), and support (`zlog`,
  `ztracing`, `ztracing_macro`, `util_macros`). This set pulls **no** domain
  stack, and — verified separately — **no** telemetry, `fs`, `git`, `proto`, or
  LLM-type crates either.
- **Deferred, not copied now — Zed's `settings` engine.** The `settings` crate
  is the sole source of a 22-crate drag (telemetry, `fs`→`git`/`proto`,
  `settings_content`→`language_model_core`→cloud/LLM types). "Adopt Zed's
  settings" means adopt the *engine* (`settings`/`settings_json`/
  `settings_macros`) with zmux's **own** schema, severing `settings_content`
  (Zed's schema) and the `fs` dependency — focused surgery, not a copy. It is off
  the critical path: the shell/terminal/theme need no settings crate (themes work
  standalone; only `theme_settings` binds to the engine). Sequenced as its own
  step, with the copy-engine-vs-own-thin choice made then. The earlier claim that
  the *whole* presentation stack including `settings` was domain-clean was wrong;
  it is clean only *without* `settings`.
- **Reject** (carry the local-filesystem model that fights remote-first):
  `project`, `worktree`, `editor`, `workspace`, `db`, `language`,
  `terminal_view`.

Zed's `settings` + `theme` are adopted deliberately, not merely tolerated: the
team wants Zed-the-app's customization *ceiling raised*, and owning the vendored
settings/theme system is what makes hand-editable JSON settings and a file-backed
configurable theme/ANSI palette (both already planned) achievable and extensible
past what Zed exposes.

The terminal is built **fresh on `alacritty_terminal`** (already pinned), using
Zed's `terminal_element.rs` and `mappings/` as a close read rather than a
dependency. The lightweight editor and the workspace (pane/dock/tab) are likewise
built on the adopted presentation stack rather than vendored from the domain
stack.

## Consequences

- The "local and remote are one code path" invariant, agent persistence,
  credentials, transcript/status logic all survive unchanged — they live in the
  backend, not the frontend.
- Dead on arrival (webview-only): xterm.js, Monaco, the glass/backdrop-filter CSS
  system, the `zoom` apparatus, the IME-replace saga, `localStorage` state, the
  Tauri capability/ACL layer, the separate settings webview window.
- gpui handles IME, GPU compositing, and multi-surface layout natively, which is
  what erases the perf/heat/quirk category the rewrite exists to escape.
- **Rejected — full Zed-derived (option c):** Zed's `project`/`worktree`/
  `language`/`editor` stack assumes a local (or LSP-served) filesystem. rmux's
  backend is remote-first ("grep runs on the machine that owns the disk", NUL-
  delimited remote records, length-framed remote reads). Adopting Zed's upper
  stack would import a data model that contradicts rmux's own.
- **Rejected — fork Zed's `terminal` crate (option i):** it drags Zed's
  `settings`/`theme`/`task` subsystems, and the painter (`terminal_element.rs`)
  lives in the workspace-coupled crate. A stripped two-file fork would be an
  orphan carrying Zed's abstractions into a codebase with a different thesis, un-
  tracked from upstream. A fresh build has one owner and no dead couplings, on
  the surface the app touches most.
- Cost accepted: no code editor and no terminal renderer come for free; both are
  built natively. Distribution (signing, notarisation, auto-update) must be
  rebuilt off Tauri (open question).
