// Fails if anything under ui/src other than lib/bridge.ts imports @tauri-apps/api.
//
//   node ui/bridge-imports-check.mjs
//
// `bridge.ts` is the one seam between the UI and its shell (Tauri or rmux-web,
// docs/rmux-web.md). A second direct import works perfectly on the desktop and
// silently breaks that feature in the browser — nothing on the desktop can see
// it, which is why this is a check and not a convention.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(fileURLToPath(new URL(".", import.meta.url)), "src");
const allowed = join(root, "lib", "bridge.ts");
// Static `import … from`, side-effect `import "…"`, `export … from`, and `import("…")`.
const pattern = /(?:\bfrom\s*|\bimport\s*\(?\s*)["']@tauri-apps\/api(?:\/[^"']*)?["']/g;

function* walk(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) yield* walk(path);
    else if (/\.(m?[jt]sx?)$/.test(name)) yield path;
  }
}

let scanned = 0;
const offenders = [];
for (const file of walk(root)) {
  scanned += 1;
  if (file === allowed) continue;
  const text = readFileSync(file, "utf8");
  text.split("\n").forEach((line, i) => {
    pattern.lastIndex = 0;
    if (pattern.test(line)) offenders.push(`${relative(process.cwd(), file).split(sep).join("/")}:${i + 1}: ${line.trim()}`);
  });
}

if (!readFileSync(allowed, "utf8").match(pattern)) {
  console.error("FAIL  lib/bridge.ts no longer imports @tauri-apps/api — is the guard looking at the right file?");
  process.exit(1);
}
if (offenders.length) {
  console.error(`FAIL  ${offenders.length} import(s) of @tauri-apps/api outside lib/bridge.ts — import from lib/bridge instead:`);
  for (const o of offenders) console.error(`  ${o}`);
  process.exit(1);
}
console.log(`PASS  ${scanned} files under ui/src; only lib/bridge.ts imports @tauri-apps/api`);
