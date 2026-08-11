# v1 Demolition Manifest (branch `zmux`)

Status: **proposed — nothing deleted yet, awaiting approval**

Big-bang strip of the `zmux` branch (ADR-0001/0002). `main` preserves all source;
the installed rmux app remains the daily driver during the rewrite. Every edge
below was verified against the current tree.

## Crates — KEEP (v1 backend spine)

| Crate | Role |
|---|---|
| `rmux-transport` | `Target` — local/remote one code path |
| `rmux-ssh` | SSH via system `ssh` binary |
| `rmux-agent` | Persistent session daemon |
| `rmux-term` | Terminal/PTY abstraction (used by `rmux-agent` **and** `rmux-claude`) |
| `rmux-claude` | Drives the Claude CLI on a `Target` |
| `rmux-fs` | Remote file read/write, search |
| `rmux-git` | "What changed" — git pane |
| `askpass/` | askpass helper (used by `rmux-ssh`) |

## Crates — DELETE

| Crate | Why | Safe? |
|---|---|---|
| `rmux-metrics` | Feature dropped (HOST/TOP-PROCESSES widgets gone) | Only a **dev-dep** of `rmux-fs` → also delete the metrics test blocks in `rmux-fs/tests/{over_ssh,live_ssh}.rs` |
| `rmux-control` | Browser control bridge — deferred | Used only by `src-tauri` (deleted) |
| `rmux-cowork` | Cowork server / sign-in / lock — deferred | Used only by `src-tauri` (deleted) |
| `rmux-core` | Empty placeholder | **Zero dependents** (verified) |
| `rmux-proto` | Empty placeholder | **Zero dependents** (verified) |

## Frontend — DELETE wholesale

- `src-tauri/` — the entire Tauri bridge. Its job (marshalling backend crates to a
  webview over IPC) is obsolete; the gpui app links the backend crates directly.
  Logic worth keeping (e.g. askpass wiring, notification hooks) is **reimplemented
  natively**, not ported file-for-file.
- `ui/` — the React/TypeScript webview app.
- `web/`, `dist/` — webview assets and built bundle.
- `node_modules/`, `package.json`, `pnpm-lock.yaml`, `vite.config.ts`,
  `tsconfig.json`, `index.html` — the JS toolchain.
- `.github/` — `tauri-action` CI (Linux/Windows). v1 is macOS, released from the
  dev machine; rebuild CI later if wanted.
- `sounds/` — notification sounds (notifications deferred).
- `tmp/` — scratch.

## KEEP — infra, some adapted

- `scripts/build-agents.sh`, `scripts/build-askpass.mjs` — unchanged.
- `scripts/release-mac.sh` — **adapt**: replace the single `pnpm tauri build`
  line with the gpui `.app` bundling step (adapted from Zed `script/bundle-mac`);
  the entire codesign / notarytool / stapler / hdiutil tail is reused as-is.
- `references/` — Zed source to **copy from** and read as reference; keep through
  development.
- `icon.png` — app icon.
- `CLAUDE.md`, `README.md` — updated after the rewrite lands, not deleted.
- `Cargo.toml` / `Cargo.lock` — edit workspace `members` to drop deleted crates
  and add the new ones below.

## NEW — to create (greenfield)

- `crates/rmux-app` — the gpui application crate: workspace shell (own tab trait +
  lifted `pane_group.rs`/`dock.rs` geometry), fresh alacritty terminal view,
  lightweight tree-sitter editor, session rail, Claude/transcript/git tabs,
  native settings, `~/.rmux/{settings,state}.json` persistence.
- `vendor/` (or `crates/vendor/`) — copied-in Zed presentation stack: `gpui` +
  closure (`gpui_macros`, `gpui_shared_string`, `gpui_util`, `collections`,
  `refineable`, `util_macros`, `sum_tree`, `scheduler`, `stacksafe`,
  `http_client`), plus `ui`, `theme` (+ `syntax_theme`, `palette`), and the
  `settings` system (+ `settings_content/json/macros`, `paths`, `migrator`, `fs`,
  `icons`, `menu`, `component`), pinned to one Zed commit.

## Not present (no action)

`server/` does not exist in this checkout. `target/` is a build artefact.
