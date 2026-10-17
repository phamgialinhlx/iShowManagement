import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'

/**
 * Port **5274**, `strictPort` — one above rmux's 5273, and pinned for the same
 * reason that one is: a silent fallback changes the origin, and the chrome
 * keeps its device presets and annotation drafts in `localStorage` against it.
 */
export default defineConfig({
  plugins: [react()],
  base: './',
  server: { port: 5274, strictPort: true },
  resolve: { alias: { '@shared': resolve(import.meta.dirname, '../shared') } },
  build: { outDir: resolve(import.meta.dirname, '../out/renderer'), emptyOutDir: true },
})
