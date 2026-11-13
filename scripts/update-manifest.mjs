#!/usr/bin/env node
// Write or merge the updater manifest (`latest.json`) that every installed rmux
// reads from the release marked *latest*.
//
//   node scripts/update-manifest.mjs \
//     --in  <existing latest.json, or "-" for none> \
//     --out <path> \
//     --version 0.2.28 \
//     [--notes-url https://github.com/…/releases/tag/rmux-v0.2.28] \
//     --entry <platform>=<url>=<file.sig> [--entry …]
//
// ## The one rule it enforces
//
// The manifest has **one** top-level version, and every platform entry is
// installed as that version. An entry left over from an older release would
// therefore tell a 0.2.27 Windows install that 0.2.28 is available and hand it
// the 0.2.27 installer — which it would install, restart into, and be offered
// again, forever. So when the version changes, entries for *other* releases are
// dropped. An entry's release is read from its URL (`/download/<tag>/`); one is
// kept only when that tag names the manifest's version.
//
// Pure file work: no network, no key. The signing happens before this runs.

import { readFileSync, writeFileSync } from "node:fs";

function fail(message) {
  console.error(`update-manifest: ${message}`);
  process.exit(1);
}

const args = process.argv.slice(2);
const opt = { entries: [] };
for (let i = 0; i < args.length; i++) {
  const key = args[i];
  const value = args[++i];
  if (value === undefined) fail(`${key} needs a value`);
  if (key === "--in") opt.in = value;
  else if (key === "--out") opt.out = value;
  else if (key === "--version") opt.version = value;
  else if (key === "--notes-url") opt.notes = value;
  else if (key === "--entry") opt.entries.push(value);
  else fail(`unknown argument ${key}`);
}
if (!opt.out) fail("--out is required");
if (!/^\d+\.\d+\.\d+$/.test(opt.version ?? "")) fail(`--version must be X.Y.Z, got ${opt.version}`);

/** The X.Y.Z of the release an asset URL points into, or null. */
export function urlVersion(url) {
  const m = /\/releases\/download\/rmux-v(\d+\.\d+\.\d+)(?:-win-linux)?\//.exec(url);
  return m ? m[1] : null;
}

let existing = { platforms: {} };
if (opt.in && opt.in !== "-") {
  try {
    existing = JSON.parse(readFileSync(opt.in, "utf8"));
  } catch (e) {
    fail(`cannot read ${opt.in}: ${e.message}`);
  }
}

const platforms = {};
for (const [name, entry] of Object.entries(existing.platforms ?? {})) {
  if (urlVersion(entry.url ?? "") === opt.version) platforms[name] = entry;
  else console.error(`update-manifest: dropping ${name} (${entry.url}) — not ${opt.version}`);
}

for (const spec of opt.entries) {
  const first = spec.indexOf("=");
  const last = spec.lastIndexOf("=");
  if (first < 0 || last === first) fail(`--entry must be platform=url=sigfile, got ${spec}`);
  const name = spec.slice(0, first);
  const url = spec.slice(first + 1, last);
  const sigFile = spec.slice(last + 1);
  if (!url.startsWith("https://github.com/")) fail(`${name}: url must be a GitHub download, got ${url}`);
  if (urlVersion(url) !== opt.version) fail(`${name}: ${url} is not a ${opt.version} asset`);
  const signature = readFileSync(sigFile, "utf8").trim();
  if (!signature) fail(`${name}: ${sigFile} is empty`);
  platforms[name] = { signature, url };
}

const manifest = {
  version: opt.version,
  notes: opt.notes ? `See what's new: ${opt.notes}` : (existing.version === opt.version && existing.notes ? existing.notes : `rmux ${opt.version}`),
  pub_date: existing.version === opt.version && existing.pub_date ? existing.pub_date : new Date().toISOString(),
  platforms,
};

writeFileSync(opt.out, JSON.stringify(manifest, null, 2) + "\n");
console.error(`update-manifest: ${opt.out} → ${opt.version}: ${Object.keys(platforms).sort().join(", ") || "(no platforms)"}`);
