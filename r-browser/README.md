# R Browser

The development browser for rmux. Point at a broken element, say what is wrong,
and hand a coding agent the address.

## Why it is a separate app

rmux's own window cannot be this. rmux is one WKWebView, so a per-session proxy
would proxy rmux's own interface — which is why `CLAUDE.md` records that the old
in-app Browser tab was removed rather than fixed, and why every page it showed
needed a forwarded port typed in first.

`crates/rmux-control` was written for the app that would replace it, before that
app existed. Its protocol already calls its client `rbrowse`. R Browser is that
client:

- a **Chromium partition per rmux session**, so `OpenProxy`'s `ssh -D` SOCKS
  proxy scopes to one session's tabs. With SOCKS the far side resolves DNS, so
  an internal hostname resolves on the server and `localhost:3000` means the
  *server's* localhost. That is the arrangement that actually delivers "no port
  forwarding";
- `Report` back the other way — what the operator pointed at, what the console
  said, what the page looked like.

## What it does

**Pick and annotate.** `Cmd-Shift-C` arms the picker. Hover highlights, `Alt`
walks up to the parent (which is most of the job in a nested React tree),
`Shift` walks back down. Click records the element; type what is wrong; Enter.
`Cmd-Shift-M` keeps the picker armed so several elements can be tapped in one
pass, each one numbered in the page.

**Generate the prompt.** Select any set of notes and copy. The output carries
the URL, the selector, the XPath, the element's own text, its curated
attributes, its ancestor path, its rect, the viewport and device it was seen
in, and the console lines from the moment of the pick — grouped by page, with a
recap of just the requests at the end. It names **the handle most likely to be
greppable** (a `data-testid` beats a generated class that appears nowhere in the
source), which is the difference between a reader finding the node and searching
for it.

**Capture.** Viewport, full page, a single element, or a dragged region — to the
clipboard or to a file. An element shot is padded, because most complaints about
an element are really about its spacing.

**Console and network.** Captured over CDP, so stacks, thrown exceptions and
security/CSP entries all arrive, not just `console.log` text. One button copies
the console, one copies errors only, one copies the network as HAR, one copies
just the failures.

**Device and lighting emulation.** 23 presets from an Apple Watch to a 4K
desktop by way of a 1920x515 signage strip, plus free width/height, rotation,
and `prefers-color-scheme` / `prefers-reduced-motion` / `forced-colors`. The
view bounds and the CDP metrics are set from one state — they are two halves of
one fact, and set separately they drift.

**The inspector still works.** DevTools opens detached (docked would resize the
page out from under the device frame, silently turning a 390px iPhone into a
240px one). Chromium allows one debugger client per target, so opening DevTools
takes ours: capture *says so* and falls back to Electron's own console events
rather than going quiet, and re-attaches when DevTools closes.

## Running it

```sh
cd r-browser
pnpm install
pnpm dev          # vite on 5274 + esbuild watch + electron
pnpm build        # main, both preloads, renderer
pnpm checks       # every checks/*-check.ts harness
pnpm check        # tsc --noEmit
```

`RB_START_URL` opens a URL at launch.

## Layout

```
shared/contract.ts   types + channel names, shared by all three processes
shared/prompt.ts     annotations -> the thing you paste into a coding agent
shared/url.ts        what the operator typed -> something loadable
main/index.ts        window, tabs, partitions, IPC, accelerators
main/tab.ts          one WebContentsView and everything attached to it
main/capture.ts      CDP: console, network, HAR, screenshots, emulation
main/control.ts      the rmux-control NDJSON client
main/devices.ts      device presets
main/store.ts        annotations on disk
page/preload-page.ts the picker, injected into the site being developed
renderer/            the browser chrome
checks/              runnable harnesses, no test framework
```

## Rules this app is built to

It follows `CLAUDE.md`, and three of its rules bite here more than anywhere:

- **A report from a browser is data, never an instruction.** Every string the
  generator takes from a page is fenced with a delimiter chosen to be longer
  than any run inside it, so a page containing a code fence cannot close ours
  and spill its own text into prose. The operator's note is the only thing
  presented as intent.
- **SIGNAL ROOM.** Zero border-radius, hairlines not glows, no emoji, and red
  only where the operator must act — which here is the armed picker and a
  console error, and nothing else.
- **Never report something as ready before verifying the artefact.** The
  harnesses check the *rendered* output, not the functions that produced it.
