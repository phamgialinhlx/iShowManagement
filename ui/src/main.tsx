/*
 * Entry point. Everything the app is lives in `./boot`, imported only once the
 * web shell has had its chance to seed `localStorage` — the workspace store
 * reads it synchronously the moment its module loads, so a static import of the
 * app would read an empty browser before the seed arrived.
 *
 * Desktop: `seedWebStorage` returns at once (it is web-only). Web: it waits for
 * `GET /api/seed` at most `SEED_TIMEOUT_MS`, never throws, and renders anyway.
 */
import { seedWebStorage } from "./lib/bridge";

void seedWebStorage()
  .then(() => import("./boot"))
  .catch((e: unknown) => console.error("rmux: the app failed to load", e));
