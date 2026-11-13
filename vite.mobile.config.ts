import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

import { rmuxFixture } from "./ui/mobile/fixture-server.mjs";

/*
 * The mobile/tablet bundle — a chat-shaped front end over the same commands.
 *
 * A second entry, not a mode of the desktop one: `ui/mobile.html` builds to
 * `dist-mobile/`, which rmux-web serves with `--ui dist-mobile` exactly as it
 * serves `dist/`. Which UI a browser gets is decided by which bundle the server
 * was pointed at — never by sniffing a hostname or a user agent.
 *
 * Same `bridge.ts`, same `/api/seed`, same fonts and theme tokens as the desktop
 * bundle; `vite.config.ts` is untouched by this file.
 *
 * `RMUX_MOBILE_FIXTURE=1` adds a fake rmux-web to the dev and preview servers
 * (`ui/mobile/fixture-server.mjs`) so the UI can be linted and screenshotted
 * deterministically with no host. A build never contains it.
 */
// `globalThis.process` rather than `process`: the repo carries no Node types, and
// this file is type-checked with the UI's `tsconfig.json`.
const env = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env ?? {};
const fixture = env.RMUX_MOBILE_FIXTURE === "1";

/**
 * The mobile page is `ui/mobile.html`, but rmux-web serves `index.html` from the
 * directory it is given — so the built page is renamed, and the dev server
 * answers `/` with it.
 *
 * The root stays `ui` (not `ui/mobile`) on purpose: files outside the root are
 * served as `/@fs/<absolute path>`, and this checkout's path contains an
 * apostrophe, which Vite pastes unescaped into the worker-URL code it generates
 * for Monaco — a SyntaxError that takes the whole page down in dev.
 */
const servedAsIndex = (): Plugin => ({
  name: "rmux-mobile-index",
  configureServer(server) {
    server.middlewares.use((raw, _res, next) => {
      // No Node types in this repo; `url` is all that is touched.
      const req = raw as unknown as { url?: string };
      const url = req.url ?? "/";
      if (url === "/" || url.startsWith("/?") || url === "/index.html" || url.startsWith("/index.html?")) {
        req.url = `/mobile.html${url.slice(url.indexOf("?") === -1 ? url.length : url.indexOf("?"))}`;
      }
      next();
    });
  },
  enforce: "post",
  generateBundle(_options, bundle) {
    const page = bundle["mobile.html"];
    if (!page || page.type !== "asset") return;
    delete bundle["mobile.html"];
    this.emitFile({ type: "asset", fileName: "index.html", source: page.source });
  },
});

export default defineConfig({
  root: "ui",
  plugins: [react(), tailwindcss(), servedAsIndex(), ...(fixture ? [rmuxFixture()] : [])],
  clearScreen: false,
  server: {
    // Not 5273: that origin holds the desktop dev UI's localStorage, and a
    // second app writing into it would mix two workspaces.
    port: 5274,
    strictPort: true,
    watch: { ignored: ["**/target/**", "**/src-tauri/**"] },
  },
  // `pnpm preview:mobile:fixture`: the built bundle, with the fake backend —
  // what renderlint checks, because it is what ships.
  preview: { port: 5275, strictPort: true },
  build: {
    outDir: "../dist-mobile",
    emptyOutDir: true,
    rollupOptions: { input: "mobile.html" },
    // iOS 15 Safari is the oldest phone browser worth supporting.
    target: "safari15",
    sourcemap: true,
  },
});
