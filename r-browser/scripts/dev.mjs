/**
 * `pnpm dev` — Vite for the chrome, esbuild watch for main and the preloads,
 * Electron on top.
 *
 * The chrome is loaded from the dev server; the *page* views are whatever the
 * operator browses to and are unaffected. Restarting Electron on a main-process
 * change would throw away every open tab, so main is rebuilt and the restart is
 * left to the operator — the message says so rather than doing it silently.
 */
import { spawn } from 'node:child_process'
import { createServer } from 'vite'
import { context } from 'esbuild'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')

const server = await createServer({ root: resolve(root, 'renderer'), server: { port: 5274, strictPort: true } })
await server.listen()
const url = `http://localhost:5274`
console.log(`chrome dev server on ${url}`)

const common = {
  bundle: true, platform: 'node', target: 'node22', format: 'cjs',
  sourcemap: 'inline', external: ['electron'], logLevel: 'warning',
  define: { 'process.env.NODE_ENV': JSON.stringify('development') },
}
for (const [entry, out] of [
  ['main/index.ts', 'out/main/index.cjs'],
  ['main/preload-chrome.ts', 'out/main/preload-chrome.cjs'],
  ['page/preload-page.ts', 'out/main/preload-page.cjs'],
]) {
  const ctx = await context({ ...common, entryPoints: [resolve(root, entry)], outfile: resolve(root, out) })
  await ctx.rebuild()
  ctx.watch()
}

const electron = spawn(
  (await import('electron')).default,
  [root],
  { stdio: 'inherit', env: { ...process.env, RB_DEV_URL: url } },
)
electron.on('close', () => { server.close(); process.exit(0) })
