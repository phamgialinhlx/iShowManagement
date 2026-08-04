#!/usr/bin/env node
/**
 * Build the `SSH_ASKPASS` helper for *this* machine and stage it where Tauri
 * expects a sidecar.
 *
 * ## Why this exists at all
 *
 * `askpass::helper_path` looks for `rmux-askpass` **beside the main
 * executable**, which is true for `cargo run` and was true of no shipped build
 * ever. The bundle carried the agents and the icon and nothing else, so every
 * release logged
 *
 *     askpass bridge unavailable — password and 2FA hosts will fail fast
 *
 * and then did exactly that. The operator's symptom is not a missing dialog, it
 * is `Permission denied (publickey,password)` with no prompt: with no helper
 * registered, `env_for_gui_prompts` tells `ssh` not to wait for a terminal, so
 * it gives up rather than hanging. Which is the right behaviour — but it made a
 * packaging gap look like a credentials problem, and it cost an evening on a
 * `cloudflared` host that was only ever asking for a password.
 *
 * ## Why it runs from `pnpm build` rather than by hand
 *
 * The one repeated failure in this project is a fix that exists in the source
 * and not in the artefact — a rebuilt agent that was never bundled, a `dist/`
 * that cargo did not notice. A helper you have to remember to build is that
 * failure waiting to happen, and it fails *silently*, because the app starts
 * perfectly and only a password-authenticated host ever notices.
 *
 * `beforeBuildCommand` runs this before cargo, so `pnpm tauri build` cannot
 * produce a bundle without it, on a developer's Mac or on any CI runner.
 *
 * ## Why Node and not a shell script
 *
 * The release workflow builds on Linux and Windows runners too. `pnpm build`
 * resolves through cmd on Windows, where a `bash scripts/…` line depends on Git
 * Bash being both installed and first on PATH. Node is already running — it is
 * what invoked this.
 *
 * ## Why the file is named with a target triple
 *
 * That is Tauri's sidecar convention: `externalBin: ["binaries/rmux-askpass"]`
 * resolves `binaries/rmux-askpass-<triple>` and copies it into the bundle under
 * the plain name, next to the main binary — precisely where `helper_path`
 * looks.
 */
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const run = (cmd, args) =>
  execFileSync(cmd, args, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });

// Ask rustc rather than deriving one from `process.platform`/`process.arch`.
// A mapping table here would be a second opinion about the host triple, and the
// only one that matters is the one cargo will actually build for.
const triple = run("rustc", ["-vV"])
  .split("\n")
  .find((l) => l.startsWith("host:"))
  ?.slice("host:".length)
  .trim();

if (!triple) {
  console.error("could not determine the host target triple from `rustc -vV`");
  process.exit(1);
}

console.log(`==> rmux-askpass for ${triple}`);
run("cargo", ["build", "-p", "rmux-askpass", "--bin", "rmux-askpass", "--release"]);

const exe = process.platform === "win32" ? ".exe" : "";
const out = join(root, "src-tauri", "binaries");
mkdirSync(out, { recursive: true });

// The triple goes *before* the extension: Tauri matches `<name>-<triple><ext>`.
const dest = join(out, `rmux-askpass-${triple}${exe}`);
copyFileSync(join(root, "target", "release", `rmux-askpass${exe}`), dest);
console.log(`    ${dest}`);
