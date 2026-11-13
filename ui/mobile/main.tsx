/*
 * The mobile bundle's entry. Loaded by `ui/mobile.html`. Same shape as `ui/src/main.tsx`, for the same
 * reason: the workspace store reads `localStorage` the moment its module loads,
 * so the app is imported only after the web shell has had its chance to seed a
 * new browser from `GET /api/seed`.
 */
import { seedWebStorage } from "../src/lib/bridge";

void seedWebStorage()
  .then(() => import("../src/mobile/boot"))
  .catch((e: unknown) => console.error("rmux: the app failed to load", e));
