import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// Dev: `vite` on :5173 proxies API + WS to the Rust `core` server on :7070,
// so the frontend runs with HMR while talking to the real backend.
// Prod: `vite build` emits static assets into `dist/`, which `core` serves.
export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:7070',
      '/ws': { target: 'ws://127.0.0.1:7070', ws: true },
    },
  },
})
