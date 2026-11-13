/**
 * renderlint for the mobile/tablet bundle (`ui/mobile.html` → `dist-mobile/`).
 *
 *     pnpm lint:mobile
 *
 * It builds the bundle, then checks the **built** bundle served by
 * `pnpm preview:mobile:fixture`: `vite preview` plus a fake rmux-web
 * (`ui/mobile/fixture-server.mjs`) that speaks the real wire protocol with canned
 * sessions and transcripts. No host, no Claude, nothing sent anywhere — and the
 * real `bridge.ts`, status stream and terminal attach run unmodified.
 *
 * The built bundle rather than the dev server, because the built one is what
 * ships (and, separately, Vite's dev server cannot serve this checkout: its path
 * contains an apostrophe, which breaks a module Vite generates for Monaco).
 */
const TRANSCRIPT = "**/api/invoke/claude_transcript";

export default {
  serve: "pnpm preview:mobile:fixture",
  url: "http://localhost:5275",
  crawl: false,
  routes: false,
  pages: [
    // The session list (tablet+ also shows the "pick a session" column).
    "/",
    // A realistic long conversation: code blocks, long paths and URLs, a table,
    // collapsed tool rows, hidden system notes, LOAD EARLIER.
    "/?s=session-login",
    // Blocked on a question: the quick-key row.
    "/?s=session-blocked",
    // pi, working; a long session name in the header.
    "/?s=pisession-parser",
    "/?s=session-deploy",
    // A conversation with nothing in it yet.
    "/?s=codexsession-tests",
    // A link to a session this browser does not hold.
    "/?s=no-such-session",
  ],
  viewports: ["375x812", "768x1024", "1280x800"],
  colorSchemes: ["light", "dark"],
  // The app polls nothing on load that could settle later than this.
  waitFor: ".m-app",
  delay: 1500,
  minTapTarget: 44,
  states: [
    {
      name: "composer with multi-line text",
      page: "/?s=session-login",
      fill: {
        "textarea.m-input":
          "Two things before you merge:\n1. keep the old redirect for /signin as an alias\n2. add a test for next=%2F%2Fevil.example (encoded)\nand then run the whole suite, not just src/auth.",
      },
      delay: 500,
    },
    {
      name: "conversation loading",
      page: "/?s=session-login",
      mock: { [TRANSCRIPT]: { json: { entries: [] }, delay: 20000 } },
      delay: 1500,
    },
    {
      name: "conversation failed to load",
      page: "/?s=session-login",
      mock: {
        [TRANSCRIPT]: {
          status: 500,
          json: { message: "ssh: connect to host devbox port 22: Connection timed out" },
        },
      },
      delay: 1500,
    },
    {
      name: "no sessions in this browser",
      page: "/",
      mock: { "**/api/seed": { status: 404, json: { message: "no seed configured" } } },
    },
  ],
  // Only the mobile sources: the desktop UI has its own conventions.
  scanDirs: ["ui/src/mobile", "ui/mobile"],
};
