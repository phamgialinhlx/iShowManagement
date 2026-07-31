# CLAUDE.md

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.

---

## Project ops: desktop build & install loop

To see changes in the installed macOS app, rebuild and reinstall. The trap: `cargo tauri build` has **no `beforeBuildCommand`**, so it does **not** rebuild the frontend, and rust-embed (`Assets` in `core/src/lib.rs`, `#[folder = "../web/dist"]`) will **not** re-embed changed assets unless `core` recompiles.

```bash
# 1. Frontend  — ONLY if you changed web/**   (`npm run check` does NOT emit dist)
cd web && npm run build
# 2. Force re-embed — ONLY if web/dist changed
touch core/src/lib.rs
# 3. Bundle (from repo root; watch for "Compiling core")
cargo tauri build
# 4. Install + relaunch
osascript -e 'tell application "iShowManagement" to quit'
rm -rf /Applications/iShowManagement.app
ditto target/release/bundle/macos/iShowManagement.app /Applications/iShowManagement.app
open /Applications/iShowManagement.app
# 5. Verify the running app serves the new build
curl -s http://127.0.0.1:7070/ | grep -oE 'index-[A-Za-z0-9_]+\.js'
```

| Changed | Steps |
|---|---|
| Frontend only (`web/**`) | 1 → 2 → 3 → 4 |
| Rust only (`core/`, `desktop/`) | 3 → 4 (cargo recompiles Rust automatically) |
| Both | 1 → 2 → 3 → 4 |

- **Verify via the running app** (`:7070`, step 5) or by `curl`-ing an asset — do **not** `grep` the built binary; embedded assets aren't greppable there.
- **Claude tmux tree / status / context features only query remote hosts** (`claude_inventory` rejects the local host), so a connected remote host is needed to see them; pure UI tweaks show anywhere.
- **Release:** bump `desktop/tauri.conf.json` version, commit, then `git tag vX.Y.Z && git push origin vX.Y.Z` — the tag triggers `release.yml` (which sets the version from the tag and builds the DMGs).
