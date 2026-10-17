/**
 * Three targets, three formats, one script.
 *
 * Electron's main process and its preloads must be **CommonJS**. A sandboxed
 * preload in particular is loaded by a bespoke loader that does not understand
 * `import`, and the failure is silent — the preload simply does not run, so the
 * picker never appears and nothing anywhere says why. The renderer is an
 * ordinary Vite build and stays ESM.
 */
import { build } from 'esbuild'
import { build as viteBuild } from 'vite'
import { rmSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const dev = process.argv.includes('--dev')

rmSync(resolve(root, 'out'), { recursive: true, force: true })
mkdirSync(resolve(root, 'out'), { recursive: true })

const common = {
  bundle: true,
  platform: 'node',
  target: 'node22',
  format: 'cjs',
  sourcemap: dev ? 'inline' : false,
  minify: !dev,
  // Electron is supplied by the runtime, never bundled.
  external: ['electron'],
  logLevel: 'info',
  define: { 'process.env.NODE_ENV': JSON.stringify(dev ? 'development' : 'production') },
}

await build({
  ...common,
  entryPoints: [resolve(root, 'main/index.ts')],
  outfile: resolve(root, 'out/main/index.cjs'),
})

await build({
  ...common,
  entryPoints: [resolve(root, 'main/preload-chrome.ts')],
  outfile: resolve(root, 'out/main/preload-chrome.cjs'),
})

await build({
  ...common,
  entryPoints: [resolve(root, 'page/preload-page.ts')],
  outfile: resolve(root, 'out/main/preload-page.cjs'),
})

if (!dev) {
  await viteBuild({ root: resolve(root, 'renderer'), base: './' })
}

console.log(dev ? 'main + preloads built (dev)' : 'built')
