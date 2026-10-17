/**
 * Runs every `checks/*-check.ts` harness.
 *
 * The repo's convention is a runnable harness rather than a test framework, and
 * the reason it is a *script* rather than a list is the lesson `tsconfig.json`
 * already taught here: a harness that has to be registered somewhere is a
 * harness that eventually is not. Anything dropped into `checks/` runs.
 *
 * Harnesses that need a browser are excluded by name (`*-check.html`), because
 * a DOM is not simulable and a harness that fakes one passes on exactly the
 * output that is broken.
 */
import { readdirSync } from 'node:fs'
import { build } from 'esbuild'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, resolve, join } from 'node:path'
import { tmpdir } from 'node:os'
import { mkdtempSync } from 'node:fs'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const out = mkdtempSync(join(tmpdir(), 'rb-checks-'))

const files = readdirSync(join(root, 'checks')).filter((f) => f.endsWith('-check.ts')).sort()
if (!files.length) {
  console.error('no checks found')
  process.exit(1)
}

let failed = 0
for (const file of files) {
  const js = join(out, file.replace(/\.ts$/, '.mjs'))
  await build({
    entryPoints: [join(root, 'checks', file)],
    bundle: true, platform: 'node', format: 'esm', outfile: js,
    external: ['electron'], logLevel: 'error',
  })
  console.log(`\n── ${file} ${'─'.repeat(Math.max(0, 60 - file.length))}`)
  // `RB_ROOT`, because a bundled harness runs from a temp directory and
  // `import.meta.url` then points there. A check that reads the *source* — and
  // the ones guarding rules no type-checker can see must read the source — has
  // no other way to find it.
  const run = spawnSync(process.execPath, [js], { stdio: 'inherit', env: { ...process.env, RB_ROOT: root } })
  if (run.status !== 0) failed++
}

console.log(`\n${failed ? `${failed} of ${files.length} harnesses FAILED` : `all ${files.length} harnesses passed`}`)
process.exit(failed ? 1 : 0)
