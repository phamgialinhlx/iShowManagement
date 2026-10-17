/**
 * R BROWSER — the wire contract between the three worlds.
 * =======================================================
 *
 * R browser is the "separate Chromium app" that `crates/rmux-control` was
 * written for. rmux's own webview cannot be a development browser: it is one
 * WKWebView, so a per-session proxy would proxy rmux's own UI. This app can
 * scope a proxy to a partition, so it is the half that makes "no port
 * forwarding" real — and it is also where an operator can point at an element
 * and say what is wrong with it.
 *
 * Three processes share these types and nothing else:
 *
 *   - **main** owns the windows, the page views and the CDP session.
 *   - **page** is a preload injected into the *site being developed*. It draws
 *     the picker and computes selectors. It exposes nothing on `window`.
 *   - **renderer** is the browser chrome — tabs, address bar, the annotation
 *     list, the prompt.
 *
 * The channel names live here too, as `const`s, because a channel name typed
 * twice is a channel name that gets typed differently once. A mismatched
 * `ipcRenderer.invoke` does not fail loudly — it hangs until its promise is
 * garbage, which reads as "the button does nothing".
 */

/* ------------------------------------------------------------------ pieces */

/** CSS pixels, plus the ratio a screenshot has to be read against. */
export interface Viewport {
  width: number
  height: number
  /** 2.0 on a Retina display. A 2560px shot of a 1280px viewport reads as a
   *  desktop layout when it is a laptop one, so this always travels with it. */
  deviceScaleFactor?: number
}

/**
 * A guess at where this element is authored, and how the guess was made.
 *
 * **Always a hypothesis, never a fact, and the payload must say so.** Hallucinated
 * file paths are a documented failure across Copilot, Cursor and Claude Code, and
 * a stale source attribution asserted confidently is worse than none at all — it
 * sends the agent to edit a file that no longer holds the element.
 *
 * The obvious route is also the dead one: React populated `fiber._debugSource`
 * from the JSX dev-source transform, and **React 19 removed it** — verified in
 * this repo's own `react-dom@19.2.8`, which contains zero occurrences. What
 * remains is `_debugStack`, an `Error` captured at the JSX call site, whose top
 * frame under a dev server points at the real source path because the served
 * path *is* the source path.
 */
export interface SourceHint {
  /** `react` | `vue` | `svelte` | `angular` | `attribute` — so the prompt can
   *  say where the guess came from rather than presenting it as fact. */
  framework: string
  /** The component's display name. Greppable, and function-level granularity
   *  measures best for repair; frequently more useful than the file. */
  component?: string
  /** Absolute or project-relative path, when one could be recovered. */
  file?: string
  line?: number
  column?: number
  /** One short phrase naming the mechanism, e.g. `React fiber _debugStack`.
   *  The reader needs it to judge how much to trust `file`. */
  via: string
  /** Enclosing components, nearest first. The element's own component may be a
   *  shared leaf (`Button`); its owner is what makes the instance findable. */
  owners?: string[]
}

/**
 * A literal string to search the source tree for, with its odds attached.
 *
 * Ordered best-first by the generator. This exists because **Claude Code has no
 * semantic index** — `Grep` is ripgrep and `Glob` matches filenames, and the
 * tool reference documents no embedding search at all. A handle that is not a
 * literal present in the source is worth nothing to it.
 */
export interface GrepAnchor {
  value: string
  kind: 'testid' | 'component' | 'text' | 'class' | 'id' | 'attr'
  /** Why this one might miss — an i18n'd label lives in a locale file and the
   *  source holds only its key. A caveat costs a line and saves a wrong edit. */
  caveat?: string
}

/**
 * Where an element is, said several ways on purpose.
 *
 * A coding agent gets handed this and has to find the same node in a source
 * tree it can read but cannot run. One selector is not enough: a generated
 * class name (`.css-1x2y3z`) is stable in the DOM and absent from the source,
 * while `data-testid` or the element's own text is usually greppable. So we
 * send every handle we have and let the reader pick.
 */
export interface ElementRef {
  /** Shortest selector that still resolves uniquely, from the picker. */
  selector: string
  /** Absolute, always resolves, never pretty. The fallback when `selector`
   *  was built on a class the source does not contain. */
  xpath: string
  /** The element's own visible text, trimmed and capped. Usually the single
   *  most greppable thing about it. */
  text?: string
  tag: string
  id?: string
  classes?: string[]
  /**
   * Curated attributes only — `data-testid`, `aria-label`, `name`, `role`,
   * `href`, `type`, `placeholder`. Never the whole attribute map: a React app
   * routinely carries serialised props, and dumping those into a prompt buries
   * the three fields that identify the node.
   */
  attrs?: Record<string, string>
  /** CSS px in *page* coordinates (scroll included), so a later screenshot
   *  clip does not depend on where the page happened to be scrolled. */
  rect: { x: number; y: number; width: number; height: number }
  /** A short ancestor trail (`main > form > div.row`), for a human reading the
   *  prompt and wondering where in the page this sits. */
  path?: string
  /** The frame's URL when the pick was inside an iframe, absent at top level.
   *  Without it a selector that does not resolve in the top document looks like
   *  a picker bug rather than a different document. */
  frameUrl?: string
  /** Best-first literals to grep for. The most valuable field in this struct. */
  anchors?: GrepAnchor[]
  /** Where it might be authored, if the framework would say. */
  source?: SourceHint
  /** A few computed values that bear on a *visual* complaint — never the whole
   *  dump. `getComputedStyle` returns 300+ properties, and pasting those buys
   *  localisation at the price of instruction-following. */
  computed?: Record<string, string>
}

/** One console line, from CDP where possible and `console-message` otherwise. */
export interface ConsoleEntry {
  id: string
  /** `log`, `warn`, `error`, `debug`, … Free string: browsers add levels, and
   *  an unknown one must not drop the entry. */
  level: string
  text: string
  /** Epoch ms, this machine's clock. */
  at: number
  url?: string
  line?: number
  column?: number
  /** Present for thrown exceptions, which is where it matters most. */
  stack?: string
  source?: 'console' | 'exception' | 'log' | 'network' | 'violation'
}

/** One request, flattened from the several CDP events that describe it. */
export interface NetworkEntry {
  id: string
  method: string
  url: string
  status?: number
  statusText?: string
  /** CDP `resourceType`: Document, XHR, Fetch, Script, … */
  type?: string
  mime?: string
  startedAt: number
  endedAt?: number
  durationMs?: number
  /** Encoded bytes over the wire. */
  size?: number
  fromCache?: boolean
  failed?: boolean
  errorText?: string
  requestHeaders?: Record<string, string>
  responseHeaders?: Record<string, string>
  /** Only ever filled for a request the operator explicitly asked to expand —
   *  response bodies are large and frequently hold tokens. */
  requestBody?: string
  responseBody?: string
}

/**
 * One thing the operator pointed at and said something about.
 *
 * This is the unit the whole app exists to produce. It is *data about a page
 * rmux did not write* — see `promptFor`: it is never phrased as an instruction.
 */
/**
 * A change the operator made to the picked element in edit mode, recorded as a
 * before/after so the prompt can ask for it precisely.
 *
 * `prop` is `"text"` for the element's text content, otherwise a CSS property
 * name (`color`, `background`, `font-size`, `padding`, …). The change is applied
 * live in the page as a preview; the annotation carries only the description, so
 * the agent makes the real edit in the source.
 */
export interface ElementEdit {
  prop: string
  from: string
  to: string
}

/**
 * What the page sends back on a pick. The `ref` is the element; `note` and
 * `edits` are what the operator typed and changed in the in-page panel that
 * opens at the cursor — the panel lives in the page because a native page view
 * covers the chrome's own DOM, so a box drawn there would be hidden behind it.
 */
export interface PickedFromPage {
  ref: ElementRef
  note?: string
  edits?: ElementEdit[]
}

export interface Annotation {
  id: string
  /** Epoch ms. Ordering is by this, not by array position: annotations are
   *  edited and re-selected, and a list that reorders under the operator is
   *  the "nothing moves under your hands" rule broken. */
  at: number
  url: string
  title?: string
  /** Absent for a note about the page as a whole rather than one element. */
  element?: ElementRef
  /** What the operator typed. The actual message; everything else is address. */
  note: string
  /** Concrete changes the operator made to the element in edit mode — the
   *  "make it look like this" half of a note. Empty or absent for a plain
   *  comment. */
  edits?: ElementEdit[]
  /** Base64 PNG, no `data:` prefix — matching `rmux_control::Report`. */
  screenshot?: string
  /**
   * Absolute path to the same PNG on disk.
   *
   * **The payload carries this path, never the base64.** A coding agent in a
   * terminal can open a file; it cannot reach into a clipboard. And base64 in
   * the text is thousands of tokens that say nothing — the earlier build wrote
   * `[screenshot attached: element, 420x38]` into the prompt while attaching
   * nothing at all, which is a claim the app did not honour.
   *
   * The clipboard route still exists, as an explicit per-note button, because
   * pasting into a chat UI that accepts images is a different job from handing
   * a path to an agent. One artefact, two ways out, neither pretending to be
   * the other.
   */
  screenshotPath?: string
  viewport: Viewport
  /** The device preset in force, so a layout bug carries the layout it was in. */
  device?: string
  colorScheme?: ColorScheme
  /** Console lines from around the moment of the pick. Captured *then*, not
   *  read later: by the time a prompt is generated the page has usually been
   *  reloaded and the errors that prompted the note are gone. */
  consoleTail?: ConsoleEntry[]
}

/**
 * What the page preload reports about its own document.
 *
 * Pinned here rather than being implied by whatever the preload happens to
 * send, because it crosses a process boundary between two files nobody edits
 * together. A field renamed on one side does not fail — it arrives
 * `undefined`, and a full-page screenshot silently becomes a viewport one.
 */
export interface PageMetrics {
  viewport: Viewport
  scrollX: number
  scrollY: number
  scrollWidth: number
  scrollHeight: number
  url: string
  frameUrl?: string
}

export type ColorScheme = 'auto' | 'light' | 'dark'

/**
 * A device the viewport can be emulated as.
 *
 * `width`/`height` are CSS px in portrait. Rotation swaps them rather than
 * being a second entry per device.
 */
export interface DevicePreset {
  id: string
  label: string
  /** Grouping for the picker: phones, tablets, desktops, watches. */
  group: string
  width: number
  height: number
  deviceScaleFactor: number
  mobile: boolean
  touch: boolean
  userAgent?: string
}

/** Everything about the emulated environment, set as one unit. */
export interface EmulationState {
  /** `null` is "fit the window" — the ordinary desktop case. */
  deviceId: string | null
  rotated: boolean
  colorScheme: ColorScheme
  reducedMotion: boolean
  forcedColors: boolean
  /** 0 means fit-to-window; otherwise a fixed scale for the device frame. */
  zoom: number
  /** Free width/height when no preset is chosen but the operator dragged the
   *  frame. Ignored while `deviceId` is set. */
  customWidth?: number
  customHeight?: number
}

export interface TabState {
  id: string
  /**
   * Which tab is in front, decided by main and *sent*, never inferred.
   *
   * The chrome cannot keep this itself: main changes the active tab on its own
   * — a popup opens as a tab, rmux pushes an `openUrl`, a close lands on the
   * neighbour. A renderer tracking it from its own `activate()` calls would
   * show the wrong tab highlighted in exactly those cases, and the operator
   * would be typing into a page that is not the one outlined.
   */
  active: boolean
  url: string
  /** What the operator typed, which is not always what loaded. Kept apart so
   *  the address bar does not rewrite itself under a half-typed URL. */
  title: string
  loading: boolean
  canGoBack: boolean
  canGoForward: boolean
  favicon?: string
  /** Set when the last navigation failed, so the chrome can say why rather
   *  than showing an empty page that looks like a hang. */
  error?: string
  emulation: EmulationState
  devtoolsOpen: boolean
  /** False while DevTools holds the debugger — see `CaptureState`. */
  capturing: boolean
  /** The rmux session this tab's partition belongs to, when rmux is connected. */
  sessionId?: string
}

/**
 * Whether console/network capture is live.
 *
 * Chromium allows **one** debugger client per target, and the DevTools window
 * is one. So opening the inspector detaches our CDP session — capture has to
 * say so rather than silently going quiet, which reads as "the log stopped
 * working". While detached, console falls back to Electron's own
 * `console-message` (level and text, no stack, no network).
 */
export interface CaptureState {
  tabId: string
  console: 'cdp' | 'fallback' | 'off'
  network: 'cdp' | 'off'
  reason?: string
}

/* ------------------------------------------------- history & bookmarks */

/**
 * One address-bar suggestion.
 *
 * The whole point of this feature: a dev browser visits the same handful of
 * servers and routes over and over, so typing one should surface the ones
 * already seen. A suggestion is either a **bookmark** (pinned on purpose, always
 * ranked first) or a **history** hit (somewhere visited before, ranked by how
 * often and how recently — the "frecency" a real address bar uses).
 */
export interface Suggestion {
  url: string
  title?: string
  kind: 'bookmark' | 'history'
  /** Times visited, for history hits. Shown as a faint hint and folded into the
   *  rank, so the server opened forty times beats the one opened once. */
  visitCount?: number
}

/** A pinned address. Unlike history it is never pruned by age — the operator
 *  chose it, so it stays until they unpin it. */
export interface Bookmark {
  url: string
  title?: string
  /** Epoch ms it was added; the list shows newest first. */
  at: number
}

/* ------------------------------------------------------------ credentials */

/**
 * A saved login, as it leaves main to *fill* a page. The password is only ever
 * in a message on this exact trip — into the page preload that owns the login
 * form, or out to the manager on an explicit reveal. It is never persisted in
 * the clear: on disk it is encrypted with the OS keychain (Electron
 * `safeStorage`), and the renderer is handed metadata, not secrets, unless the
 * operator asks to reveal one.
 */
export interface SavedCredential {
  /** The exact `URL.origin` the login was seen on. Autofill matches on this and
   *  nothing looser — a credential saved for `https://a.com` is never offered to
   *  `https://b.com` or to `http://a.com`, which is the whole phishing guard. */
  origin: string
  username: string
  password: string
  updatedAt: number
}

/** What the manager lists: everything but the secret. */
export interface CredentialMeta {
  origin: string
  username: string
  updatedAt: number
}

/** The page → main report when a login is submitted. */
export interface CredentialSubmit {
  origin: string
  username: string
  password: string
}

/** main → page, to fill a login form. */
export interface FillCredentials {
  username: string
  password: string
}

/**
 * main → renderer: offer to save what was just submitted. It carries **no
 * password** — the plaintext stays in main, held against the tab, and the
 * renderer's Save simply tells main to commit the pending one. `kind` picks the
 * wording: a brand-new login versus an updated password for a known user.
 */
export interface CredentialOffer {
  tabId: string
  origin: string
  username: string
  kind: 'new' | 'update'
}

/** How rmux itself is reachable, if at all. R browser runs perfectly well
 *  without it — rmux being closed must degrade one panel, not the app. */
export interface ControlState {
  connected: boolean
  app?: string
  version?: number
  error?: string
  sessions: Array<{ id: string; name: string; host?: string; folder: string }>
  activeSessionId?: string
}

/* ---------------------------------------------------------------- channels */

/** renderer → main, request/response. */
export const RB = {
  tabNew: 'rb:tab:new',
  tabClose: 'rb:tab:close',
  tabActivate: 'rb:tab:activate',
  tabNavigate: 'rb:tab:navigate',
  tabBack: 'rb:tab:back',
  tabForward: 'rb:tab:forward',
  tabReload: 'rb:tab:reload',
  tabStop: 'rb:tab:stop',
  tabDevtools: 'rb:tab:devtools',
  tabList: 'rb:tab:list',

  /** The chrome tells main how much room it has left for the page. */
  layout: 'rb:layout',

  emulationSet: 'rb:emulation:set',
  devices: 'rb:devices',

  pickStart: 'rb:pick:start',
  pickStop: 'rb:pick:stop',
  highlight: 'rb:highlight',
  remeasure: 'rb:remeasure',

  captureViewport: 'rb:capture:viewport',
  captureFullPage: 'rb:capture:fullpage',
  captureElement: 'rb:capture:element',
  captureRegion: 'rb:capture:region',

  consoleGet: 'rb:console:get',
  consoleClear: 'rb:console:clear',
  networkGet: 'rb:network:get',
  networkClear: 'rb:network:clear',
  networkHar: 'rb:network:har',
  networkBody: 'rb:network:body',

  clipboardText: 'rb:clipboard:text',
  clipboardImage: 'rb:clipboard:image',
  savePng: 'rb:save:png',

  storeGet: 'rb:store:get',
  storeSet: 'rb:store:set',

  dataClear: 'rb:data:clear',

  /** Saved-password manager and the save/never decisions on an offer. */
  passwordsList: 'rb:passwords:list',
  passwordReveal: 'rb:passwords:reveal',
  passwordDelete: 'rb:passwords:delete',
  passwordSaveOffer: 'rb:passwords:save-offer',
  passwordDismissOffer: 'rb:passwords:dismiss-offer',

  /** Address-bar history suggestions and the bookmark set. */
  historySuggest: 'rb:history:suggest',
  historyClear: 'rb:history:clear',
  bookmarksList: 'rb:bookmarks:list',
  bookmarkAdd: 'rb:bookmarks:add',
  bookmarkRemove: 'rb:bookmarks:remove',
  bookmarkHas: 'rb:bookmarks:has',

  controlStatus: 'rb:control:status',
  controlConnect: 'rb:control:connect',
  controlReport: 'rb:control:report',
  controlActivate: 'rb:control:activate',
  controlProxy: 'rb:control:proxy',
} as const

/** main → renderer, pushed. */
export const RBEvent = {
  tabs: 'rb:evt:tabs',
  picked: 'rb:evt:picked',
  pickCancelled: 'rb:evt:pick-cancelled',
  consoleEntry: 'rb:evt:console',
  networkEntry: 'rb:evt:network',
  captureState: 'rb:evt:capture-state',
  controlState: 'rb:evt:control-state',
  openUrl: 'rb:evt:open-url',
  shortcut: 'rb:evt:shortcut',
  /** Offer to save a submitted login. `null` clears the bar. */
  credentialOffer: 'rb:evt:credential-offer',
} as const

/** main ↔ page preload. Separate namespace: these cross into a document the
 *  operator does not control, and must never collide with the chrome's. */
export const RBPage = {
  setPickMode: 'rb:page:set-pick-mode',
  picked: 'rb:page:picked',
  cancelled: 'rb:page:cancelled',
  highlight: 'rb:page:highlight',
  remeasure: 'rb:page:remeasure',
  scrollTo: 'rb:page:scroll-to',
  ready: 'rb:page:ready',
  metrics: 'rb:page:metrics',
  /** page → main: this top frame loaded, here is its origin — send back any
   *  saved login for it. main → page: fill these into the login form. */
  credentialRequest: 'rb:page:credential-request',
  fillCredentials: 'rb:page:fill-credentials',
  /** page → main: a login was just submitted here. */
  credentialSubmit: 'rb:page:credential-submit',
} as const

/** Pick mode. `multi` keeps the picker armed after a pick, which is what makes
 *  "tap several things then write them up" one gesture instead of five. */
export interface PickMode {
  active: boolean
  multi: boolean
  /** Ask the page for a screenshot clip rect as well as the ref. */
  shoot: boolean
}
