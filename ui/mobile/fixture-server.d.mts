import type { Plugin } from "vite";
/** The dev-only fake rmux-web; see `fixture-server.mjs`. */
export function rmuxFixture(): Plugin;
export const SESSIONS: readonly { id: string; kind: string; name: string; status: string | null }[];
