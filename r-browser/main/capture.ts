/**
 * R BROWSER — console, network, HAR and screenshots for one page view.
 * ====================================================================
 *
 * Everything here runs over the Chrome DevTools Protocol against a single
 * `WebContents`, and the whole file is shaped by one fact:
 *
 *   **Chromium allows exactly one debugger client per target, and the DevTools
 *   window is one of them.**
 *
 * So this module cannot assume it owns the debugger. It attaches when it can,
 * *says so* when it cannot, keeps a reduced console going via Electron's own
 * `console-message` in the meantime, and re-attaches the moment DevTools
 * closes. The one thing it must never do is go quiet: a log that simply stops
 * with no explanation reads as a broken app, and the operator's next move is to
 * report the tool rather than the page — which is the opposite of what a
 * development browser is for.
 *
 * Three rules from the rmux tree apply directly and are worth naming here,
 * because each one is invisible until it costs an afternoon:
 *
 *   - **Every ring is bounded.** A store bounded in one dimension reads as
 *     bounded right up until the moment it isn't. A chatty page left open over
 *     lunch emits console lines and requests without limit.
 *   - **Bodies are never stored eagerly.** See `body()`.
 *   - **Nothing measured is invented.** A HAR timing we did not observe is
 *     `-1`, not a plausible number. A reader can render "unknown"; it cannot
 *     un-believe a fabricated 200ms.
 */

import type { WebContents } from 'electron'
import type { CaptureState, ConsoleEntry, NetworkEntry } from '../shared/contract'

/* --------------------------------------------------------------- the shape */

export interface CaptureEvents {
  onConsole(entry: ConsoleEntry): void
  onNetwork(entry: NetworkEntry): void
  onState(state: CaptureState): void
}

export interface EmulationOptions {
  width?: number
  height?: number
  deviceScaleFactor?: number
  /** Page scale factor for the emulated frame. A 390px phone rendered into a
   *  300px slot is *scaled down*, never re-laid-out at 300px — the second would
   *  show a layout the device never produces, which is the whole thing device
   *  emulation exists to avoid. */
  scale?: number
  mobile?: boolean
  touch?: boolean
  userAgent?: string
  colorScheme?: 'auto' | 'light' | 'dark'
  reducedMotion?: boolean
  forcedColors?: boolean
}

/* ------------------------------------------------------------------ limits */

/** Console ring. 5000 lines is more than anyone scrolls and small enough that
 *  a page logging in a `requestAnimationFrame` loop cannot grow the process. */
const MAX_CONSOLE = 5000

/** Network ring. Smaller than the console ring because each entry carries two
 *  header maps, which are the expensive part. */
const MAX_NETWORK = 3000

/** A body over this is refused with a sentence rather than returned. Two
 *  megabytes of minified bundle in a prompt is not evidence, it is noise. */
const MAX_BODY_BYTES = 2 * 1024 * 1024

/** `Page.captureScreenshot` will happily be asked for a 200000px-tall image and
 *  will take the renderer down trying. Infinite-scroll pages report exactly
 *  that from `getLayoutMetrics`, so the clamp is not theoretical. */
const MAX_FULL_PAGE_PX = 30000

/** Matched by hand to `package.json`. It only ever lands in `log.creator`, and
 *  keeping it a plain constant is what lets `buildHar` be imported and tested
 *  with no Electron anywhere near it. */
export const R_BROWSER_VERSION = '0.1.0'

/* ------------------------------------------------- CDP shapes we care about
 *
 * Deliberately partial and deliberately all-optional. Binding to Chromium's
 * full protocol types would turn every Chromium release into "the console is
 * empty" — the same failure the transcript reader in rmux is built to avoid.
 * We pick fields out one at a time and skip what we do not recognise. */

interface RemoteObject {
  type?: string
  subtype?: string
  className?: string
  value?: unknown
  unserializableValue?: string
  description?: string
  preview?: ObjectPreview
}

interface PropertyPreview {
  name?: string
  type?: string
  subtype?: string
  value?: string
  valuePreview?: ObjectPreview
}

interface ObjectPreview {
  type?: string
  subtype?: string
  description?: string
  overflow?: boolean
  properties?: PropertyPreview[]
  entries?: Array<{ key?: ObjectPreview; value?: ObjectPreview }>
}

interface CallFrame {
  functionName?: string
  url?: string
  lineNumber?: number
  columnNumber?: number
}

interface StackTrace {
  callFrames?: CallFrame[]
  parent?: StackTrace
}

/* =========================================================================
 * Capture
 * ========================================================================= */

export class Capture {
  private readonly consoleRing: ConsoleEntry[] = []
  private readonly networkRing: NetworkEntry[] = []

  /** In-flight requests, keyed by the *CDP* request id. An entry leaves this
   *  map when it finishes, fails, or is redirected away — never on a timer,
   *  because a request that never completes is a finding, not a leak. */
  private readonly live = new Map<string, NetworkEntry>()

  /** How many times a given CDP request id has been redirected. It is what
   *  keeps entry ids unique across a redirect chain; see `noteRedirect`. */
  private readonly redirects = new Map<string, number>()

  private attached = false
  private disposed = false
  private seq = 0
  private lastState: CaptureState

  constructor(
    private readonly tabId: string,
    private readonly wc: WebContents,
    private readonly events: CaptureEvents,
  ) {
    this.lastState = { tabId, console: 'off', network: 'off' }

    // Listeners are registered **once**, here, and never inside `attach()`.
    // `attach` is idempotent and is also called again on every `devtools-closed`,
    // so registering there would add a second `message` listener per DevTools
    // visit and every console line would arrive twice, then three times.
    this.wc.debugger.on('message', this.handleMessage)
    this.wc.debugger.on('detach', this.handleDetach)
    this.wc.on('devtools-closed', this.handleDevtoolsClosed)
    this.wc.on('console-message', this.handleConsoleMessage)
  }

  /* ------------------------------------------------------------- lifecycle */

  /**
   * Idempotent. Tolerates the debugger already being attached, and **catches**
   * the attach failure rather than throwing: `debugger.attach()` throws when
   * DevTools already holds the target, which is an ordinary thing for an
   * operator to do and must not take a tab down.
   */
  attach(): void {
    if (this.disposed) return
    if (this.attached) return

    try {
      if (!this.wc.isDestroyed() && !this.wc.debugger.isAttached()) {
        this.wc.debugger.attach('1.3')
      }
    } catch (err) {
      // Almost always "Another debugger is already attached" — i.e. DevTools.
      this.publish({
        console: 'fallback',
        network: 'off',
        reason: this.wc.isDevToolsOpened()
          ? 'DevTools has the debugger — network capture is paused, console is still recorded'
          : `the debugger could not attach (${message(err)}) — network capture is paused, console is still recorded`,
      })
      return
    }

    this.attached = true
    this.publish({ console: 'cdp', network: 'cdp', reason: undefined })
    void this.enableDomains()
  }

  detach(): void {
    if (!this.attached) return
    this.attached = false
    try {
      if (!this.wc.isDestroyed() && this.wc.debugger.isAttached()) this.wc.debugger.detach()
    } catch {
      /* already gone; the state below is what the operator needs either way */
    }
    this.publish({
      console: 'fallback',
      network: 'off',
      reason: 'capture is detached — console is still recorded, network is not',
    })
  }

  dispose(): void {
    if (this.disposed) return
    this.disposed = true
    this.detach()
    try {
      this.wc.debugger.removeListener('message', this.handleMessage)
      this.wc.debugger.removeListener('detach', this.handleDetach)
      this.wc.removeListener('devtools-closed', this.handleDevtoolsClosed)
      this.wc.removeListener('console-message', this.handleConsoleMessage)
    } catch {
      /* a destroyed WebContents throws on removeListener; nothing left to leak */
    }
    this.live.clear()
    this.redirects.clear()
    this.publish({ console: 'off', network: 'off', reason: 'the tab is gone' })
  }

  private async enableDomains(): Promise<void> {
    // `Runtime` carries console API calls and uncaught exceptions; `Log` carries
    // everything the *browser* says rather than the page — failed requests, CSP
    // violations, mixed content. Without `Log` a blocked request produces a red
    // line in DevTools and nothing at all here, which is the single most
    // confusing way for this panel to be wrong.
    await this.send('Runtime.enable')
    await this.send('Log.enable')
    await this.send('Page.enable')
    // A generous buffer: the default is small enough that `getResponseBody` on
    // a request from thirty seconds ago frequently answers "no data found",
    // which reads as a bug in the expand button.
    await this.send('Network.enable', {
      maxTotalBufferSize: 64 * 1024 * 1024,
      maxResourceBufferSize: 16 * 1024 * 1024,
      maxPostDataSize: 64 * 1024,
    })
  }

  /** Every command goes through here. A page that navigates mid-command makes
   *  `sendCommand` reject, and an unhandled rejection in the main process takes
   *  the whole app with it — for a screenshot of a page that is already gone. */
  private async send<T = Record<string, unknown>>(
    method: string,
    params?: Record<string, unknown>,
  ): Promise<T | undefined> {
    if (this.disposed || !this.attached) return undefined
    try {
      if (this.wc.isDestroyed()) return undefined
      return (await this.wc.debugger.sendCommand(method, params)) as T
    } catch (err) {
      console.warn(`[capture ${this.tabId}] ${method} failed: ${message(err)}`)
      return undefined
    }
  }

  /**
   * Evaluate an expression in the page's **main world** and return its value.
   *
   * The only route to the framework expandos (`__reactFiber$…`, `__vnode`,
   * `__svelte_meta`): those live in the page's own world, and our preload is an
   * isolated one that cannot see them. `returnByValue` because the caller wants
   * plain JSON, not a remote object handle it would then have to release.
   *
   * Returns `undefined` when the debugger is detached — which is the ordinary
   * state while DevTools is open — rather than throwing, because the callers
   * are enrichment paths whose failure must not lose a pick.
   */
  async evaluate<T>(expression: string): Promise<T | undefined> {
    const r = await this.send<{ result?: { value?: T }; exceptionDetails?: unknown }>(
      'Runtime.evaluate',
      { expression, returnByValue: true, awaitPromise: false, timeout: 1000 },
    )
    if (!r || r.exceptionDetails) return undefined
    return r.result?.value
  }

  /* ---------------------------------------------------------------- state */

  state(): CaptureState {
    return { ...this.lastState }
  }

  private publish(next: Omit<CaptureState, 'tabId'>): void {
    this.lastState = { tabId: this.tabId, ...next }
    this.events.onState(this.state())
  }

  private handleDetach = (_event: unknown, reason: string): void => {
    this.attached = false
    // Do not go silently quiet. The console keeps working through Electron's
    // own `console-message` — with no stack and no network — and the reason
    // below is the difference between "paused, here is why" and "broken".
    const devtools = !this.wc.isDestroyed() && this.wc.isDevToolsOpened()
    this.publish({
      console: 'fallback',
      network: 'off',
      reason: devtools
        ? 'DevTools has the debugger — network capture is paused, console is still recorded'
        : `the debugger detached (${reason}) — network capture is paused, console is still recorded`,
    })
  }

  private handleDevtoolsClosed = (): void => {
    // The target is free again. Re-attaching is not optional: without it the
    // operator opens the inspector once and network capture is off for the rest
    // of the tab's life, with a stale reason explaining why.
    this.attach()
  }

  /* -------------------------------------------------------- console (CDP) */

  consoleEntries(): ConsoleEntry[] {
    return this.consoleRing.slice()
  }

  clearConsole(): void {
    this.consoleRing.length = 0
  }

  private nextId(prefix: string): string {
    this.seq += 1
    return `${prefix}-${this.tabId}-${this.seq}`
  }

  private pushConsole(entry: ConsoleEntry): void {
    this.consoleRing.push(entry)
    if (this.consoleRing.length > MAX_CONSOLE) {
      this.consoleRing.splice(0, this.consoleRing.length - MAX_CONSOLE)
    }
    this.events.onConsole(entry)
  }

  private handleMessage = (_event: unknown, method: string, params: Record<string, unknown>): void => {
    if (this.disposed) return
    const p = (params ?? {}) as Record<string, any>
    switch (method) {
      case 'Runtime.consoleAPICalled':
        this.onConsoleApi(p)
        break
      case 'Runtime.exceptionThrown':
        this.onException(p)
        break
      case 'Log.entryAdded':
        this.onLogEntry(p)
        break
      case 'Network.requestWillBeSent':
        this.onRequestWillBeSent(p)
        break
      case 'Network.requestWillBeSentExtraInfo':
        this.onRequestExtraInfo(p)
        break
      case 'Network.responseReceived':
        this.onResponseReceived(p)
        break
      case 'Network.responseReceivedExtraInfo':
        this.onResponseExtraInfo(p)
        break
      case 'Network.loadingFinished':
        this.onLoadingFinished(p)
        break
      case 'Network.loadingFailed':
        this.onLoadingFailed(p)
        break
      default:
        // Every other domain event is deliberately ignored rather than logged —
        // `Page` alone is dozens of lifecycle events per navigation.
        break
    }
  }

  private onConsoleApi(p: Record<string, any>): void {
    const frame: CallFrame | undefined = (p.stackTrace as StackTrace | undefined)?.callFrames?.[0]
    this.pushConsole({
      id: this.nextId('c'),
      level: normaliseLevel(String(p.type ?? 'log')),
      text: formatArgs((p.args as RemoteObject[]) ?? []),
      // CDP hands us its own monotonic clock. The contract says `at` is *this
      // machine's* clock, and mixing the two produces a log that sorts wrongly
      // against everything else in the app.
      at: Date.now(),
      url: frame?.url || undefined,
      line: typeof frame?.lineNumber === 'number' ? frame.lineNumber + 1 : undefined,
      column: typeof frame?.columnNumber === 'number' ? frame.columnNumber + 1 : undefined,
      source: 'console',
    })
  }

  private onException(p: Record<string, any>): void {
    const d = (p.exceptionDetails ?? {}) as Record<string, any>
    const thrown = d.exception as RemoteObject | undefined
    const text = thrown?.description ?? formatRemote(thrown) ?? String(d.text ?? 'Uncaught (unknown exception)')
    this.pushConsole({
      id: this.nextId('c'),
      level: 'error',
      text: text || String(d.text ?? 'Uncaught'),
      at: Date.now(),
      url: (d.url as string) || undefined,
      line: typeof d.lineNumber === 'number' ? d.lineNumber + 1 : undefined,
      column: typeof d.columnNumber === 'number' ? d.columnNumber + 1 : undefined,
      stack: formatStack(d.stackTrace as StackTrace | undefined),
      source: 'exception',
    })
  }

  private onLogEntry(p: Record<string, any>): void {
    const e = (p.entry ?? {}) as Record<string, any>
    const src = String(e.source ?? 'other')
    this.pushConsole({
      id: this.nextId('c'),
      level: normaliseLevel(String(e.level ?? 'info')),
      text: String(e.text ?? ''),
      at: Date.now(),
      url: (e.url as string) || undefined,
      line: typeof e.lineNumber === 'number' ? e.lineNumber + 1 : undefined,
      stack: formatStack(e.stackTrace as StackTrace | undefined),
      // This is where a failed request, a CSP refusal and a mixed-content block
      // arrive, and telling them apart from a `console.error` is the point of
      // carrying `source` at all.
      source: src === 'network' ? 'network' : src === 'violation' ? 'violation' : 'log',
    })
  }

  /* --------------------------------------------------- console (fallback)
   *
   * Electron's own event. Level and text only: no stack, no arguments, no
   * network. It is a worse console, and it is enormously better than none. */

  private handleConsoleMessage = (...args: unknown[]): void => {
    // **The double-record guard.** Electron fires this whether or not CDP is
    // attached, so without the check every line the page logs appears twice.
    if (this.attached || this.disposed) return

    const first = args[0] as Record<string, any> | undefined
    let level = 'log'
    let text = ''
    let line: number | undefined
    let url: string | undefined

    if (first && typeof first === 'object' && typeof first.message === 'string') {
      // Electron ≥ 35: one event object, `level` a string.
      level = normaliseLevel(String(first.level ?? 'info'))
      text = first.message
      line = numberOrUndefined(first.lineNumber)
      url = typeof first.sourceId === 'string' ? first.sourceId : undefined
    } else {
      // Electron < 35: `(event, level: number, message, line, sourceId)`.
      // Detected by inspecting the arguments rather than by version, because the
      // Electron version moves under this file and a version check is a thing
      // nobody updates.
      level = LEGACY_LEVELS[Number(args[1])] ?? 'log'
      text = String(args[2] ?? '')
      line = numberOrUndefined(args[3])
      url = typeof args[4] === 'string' ? args[4] : undefined
    }

    this.pushConsole({
      id: this.nextId('c'),
      level,
      text,
      at: Date.now(),
      url,
      line,
      source: 'console',
    })
  }

  /* ---------------------------------------------------------------- network */

  networkEntries(): NetworkEntry[] {
    return this.networkRing.slice()
  }

  clearNetwork(): void {
    this.networkRing.length = 0
    this.live.clear()
    this.redirects.clear()
  }

  private pushNetwork(entry: NetworkEntry): void {
    this.networkRing.push(entry)
    if (this.networkRing.length > MAX_NETWORK) {
      const dropped = this.networkRing.splice(0, this.networkRing.length - MAX_NETWORK)
      // A dropped in-flight entry must leave `live` too, or the map grows for
      // the lifetime of the tab holding entries nothing can ever render.
      for (const d of dropped) {
        const cdpId = cdpIdOf(d.id)
        if (this.live.get(cdpId) === d) this.live.delete(cdpId)
      }
    }
    this.events.onNetwork(entry)
  }

  /** Emitted on every update, not only on completion: the renderer upserts by
   *  `id`, and a request that only appears once finished makes a slow endpoint
   *  invisible for exactly as long as it is interesting. */
  private touch(entry: NetworkEntry): void {
    this.events.onNetwork(entry)
  }

  private onRequestWillBeSent(p: Record<string, any>): void {
    const cdpId = String(p.requestId ?? '')
    if (!cdpId) return
    const req = (p.request ?? {}) as Record<string, any>

    if (p.redirectResponse) {
      // A redirect closes the previous entry for this id and starts a new one.
      // Without this the 3xx hop vanishes entirely and the log shows one
      // request to a URL that was never actually fetched — which is precisely
      // the case somebody is looking at the network panel to understand.
      this.noteRedirect(cdpId, p.redirectResponse as Record<string, any>)
    }

    const n = this.redirects.get(cdpId) ?? 0
    const entry: NetworkEntry = {
      // The *first* entry keeps the bare CDP id; each hop after a redirect gets
      // a suffix. `body()` strips it back off, so the ids stay unique without a
      // second lookup table.
      id: n === 0 ? cdpId : `${cdpId}#${n}`,
      method: String(req.method ?? 'GET'),
      url: String(req.url ?? ''),
      type: p.type ? String(p.type) : undefined,
      startedAt: Date.now(),
      requestHeaders: headerMap(req.headers),
    }
    this.live.set(cdpId, entry)
    this.pushNetwork(entry)
  }

  private noteRedirect(cdpId: string, redirect: Record<string, any>): void {
    const prev = this.live.get(cdpId)
    if (!prev) return
    prev.status = numberOrUndefined(redirect.status)
    prev.statusText = redirect.statusText ? String(redirect.statusText) : undefined
    prev.mime = redirect.mimeType ? String(redirect.mimeType) : undefined
    prev.responseHeaders = headerMap(redirect.headers) ?? prev.responseHeaders
    prev.endedAt = Date.now()
    prev.durationMs = prev.endedAt - prev.startedAt
    prev.fromCache = Boolean(redirect.fromDiskCache)
    this.live.delete(cdpId)
    this.redirects.set(cdpId, (this.redirects.get(cdpId) ?? 0) + 1)
    this.touch(prev)
  }

  private onRequestExtraInfo(p: Record<string, any>): void {
    // Trivial merge only: this event carries the headers as they actually went
    // on the wire (cookies included), and it can arrive before or after the
    // request itself. Anything more — blocked-cookie reasons, connect timing —
    // is a second model of a request and is deliberately not built here.
    const entry = this.live.get(String(p.requestId ?? ''))
    if (!entry) return
    const headers = headerMap(p.headers)
    if (headers) {
      entry.requestHeaders = { ...(entry.requestHeaders ?? {}), ...headers }
      this.touch(entry)
    }
  }

  private onResponseReceived(p: Record<string, any>): void {
    const entry = this.live.get(String(p.requestId ?? ''))
    if (!entry) return
    const r = (p.response ?? {}) as Record<string, any>
    entry.status = numberOrUndefined(r.status)
    entry.statusText = r.statusText ? String(r.statusText) : undefined
    entry.mime = r.mimeType ? String(r.mimeType) : undefined
    entry.responseHeaders = headerMap(r.headers) ?? entry.responseHeaders
    entry.fromCache = Boolean(r.fromDiskCache)
    if (p.type) entry.type = String(p.type)
    this.touch(entry)
  }

  private onResponseExtraInfo(p: Record<string, any>): void {
    const entry = this.live.get(String(p.requestId ?? ''))
    if (!entry) return
    const headers = headerMap(p.headers)
    if (headers) {
      entry.responseHeaders = { ...(entry.responseHeaders ?? {}), ...headers }
      this.touch(entry)
    }
  }

  private onLoadingFinished(p: Record<string, any>): void {
    const cdpId = String(p.requestId ?? '')
    const entry = this.live.get(cdpId)
    if (!entry) return
    entry.endedAt = Date.now()
    entry.durationMs = entry.endedAt - entry.startedAt
    const encoded = numberOrUndefined(p.encodedDataLength)
    if (encoded !== undefined) entry.size = encoded
    this.live.delete(cdpId)
    this.touch(entry)
  }

  private onLoadingFailed(p: Record<string, any>): void {
    const cdpId = String(p.requestId ?? '')
    const entry = this.live.get(cdpId)
    if (!entry) return
    entry.failed = true
    // `canceled` is a real outcome and not an error; saying so stops a navigated-
    // away-from request reading as a server fault.
    entry.errorText = p.canceled ? String(p.errorText ?? 'canceled') : String(p.errorText ?? 'failed')
    entry.endedAt = Date.now()
    entry.durationMs = entry.endedAt - entry.startedAt
    this.live.delete(cdpId)
    this.touch(entry)
  }

  /* ------------------------------------------------------------------ HAR */

  har(): string {
    return buildHar(this.networkRing, R_BROWSER_VERSION)
  }

  /* ----------------------------------------------------------------- body */

  /**
   * Fetched **on demand, never stored**.
   *
   * Response bodies are large and frequently hold tokens — a session cookie, a
   * bearer in a JSON login reply, a signed URL. A capture buffer that quietly
   * accumulated them would be a credential store nobody meant to build, sitting
   * in a process that also exports HAR files and pastes into prompts. So the
   * ring holds addresses and sizes, and the bytes are pulled only when the
   * operator points at one request and asks.
   *
   * `requestId` is an *entry* id, redirect suffix and all.
   */
  async body(requestId: string): Promise<{ request?: string; response?: string }> {
    if (!this.attached) {
      throw new Error(
        'response bodies need the debugger, which DevTools currently holds — close DevTools and expand this request again',
      )
    }
    const cdpId = cdpIdOf(requestId)
    const entry = this.networkRing.find((e) => e.id === requestId)
    const out: { request?: string; response?: string } = {}

    const post = await this.send<{ postData?: string }>('Network.getRequestPostData', { requestId: cdpId })
    if (post?.postData !== undefined) out.request = capBody(post.postData)

    if (entry?.size !== undefined && entry.size > MAX_BODY_BYTES) {
      // Refused before the fetch: pulling two megabytes across the protocol to
      // then decline to show it is the worst of both.
      out.response = `[not shown — ${entry.size} bytes exceeds the ${MAX_BODY_BYTES} byte cap]`
      return out
    }

    const res = await this.send<{ body?: string; base64Encoded?: boolean }>('Network.getResponseBody', {
      requestId: cdpId,
    })
    if (res?.body === undefined) return out

    if (res.base64Encoded) {
      const bytes = Buffer.from(res.body, 'base64')
      if (isTextual(entry?.mime)) out.response = capBody(bytes.toString('utf8'))
      else out.response = `[binary ${entry?.mime ?? 'response'}, ${bytes.length} bytes — not shown]`
    } else {
      out.response = capBody(res.body)
    }
    return out
  }

  /* ---------------------------------------------------------- screenshots */

  /** Base64 PNG with no `data:` prefix — which is exactly what CDP returns, and
   *  exactly what `rmux_control::Report` and `Annotation.screenshot` expect. */
  async captureViewport(): Promise<string> {
    if (!this.attached) return this.fallbackShot()
    const res = await this.send<{ data?: string }>('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: false,
    })
    return res?.data ?? (await this.fallbackShot())
  }

  async captureFullPage(): Promise<string> {
    if (!this.attached) {
      // There is no fallback for this one — `capturePage` can only ever return
      // what is on screen. An error that names the cause is a fine outcome; a
      // silently cropped image presented as the whole page is not.
      throw new Error('full-page capture needs the debugger — close DevTools and try again')
    }
    const metrics = await this.send<Record<string, any>>('Page.getLayoutMetrics')
    const size = (metrics?.cssContentSize ?? metrics?.contentSize ?? {}) as Record<string, any>
    const rawW = Math.ceil(Number(size.width ?? 0)) || 0
    const rawH = Math.ceil(Number(size.height ?? 0)) || 0
    if (rawW <= 0 || rawH <= 0) return this.captureViewport()

    const width = Math.min(rawW, MAX_FULL_PAGE_PX)
    const height = Math.min(rawH, MAX_FULL_PAGE_PX)
    if (width !== rawW || height !== rawH) {
      // An infinite-scroll page reports a document tens of thousands of pixels
      // tall and Chromium will take the renderer down trying to raster it.
      console.warn(
        `[capture ${this.tabId}] full page clamped from ${rawW}x${rawH} to ${width}x${height} css px`,
      )
    }
    const res = await this.send<{ data?: string }>('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: true,
      clip: { x: 0, y: 0, width, height, scale: 1 },
    })
    if (!res?.data) throw new Error('full-page capture returned no image')
    return res.data
  }

  /**
   * `rect` is in **page** coordinates, CSS px — the same space `ElementRef.rect`
   * is in, so an element picked while scrolled down still shoots correctly.
   *
   * `captureBeyondViewport` is what makes that work without scrolling the page
   * to bring the element into view. Scrolling to shoot moves the page under the
   * operator's hands, which this tree treats as a rule and not a preference.
   */
  async captureRect(rect: { x: number; y: number; width: number; height: number }): Promise<string> {
    if (!this.attached) return this.fallbackShot(rect)
    const res = await this.send<{ data?: string }>('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: true,
      clip: {
        x: rect.x,
        y: rect.y,
        width: Math.max(1, rect.width),
        height: Math.max(1, rect.height),
        scale: 1,
      },
    })
    return res?.data ?? (await this.fallbackShot(rect))
  }

  /** Electron's own capture. Viewport-relative and screen-only — a rect below
   *  the fold cannot be shot this way at all, which is why full-page refuses
   *  rather than pretending. */
  private async fallbackShot(rect?: { x: number; y: number; width: number; height: number }): Promise<string> {
    if (this.wc.isDestroyed()) throw new Error('the tab is gone')
    const image = rect
      ? await this.wc.capturePage({
          x: Math.round(rect.x),
          y: Math.round(rect.y),
          width: Math.max(1, Math.round(rect.width)),
          height: Math.max(1, Math.round(rect.height)),
        })
      : await this.wc.capturePage()
    return image.toPNG().toString('base64')
  }

  /* ------------------------------------------------------------ emulation */

  async setEmulation(opts: EmulationOptions): Promise<void> {
    if (!this.attached) {
      // A mutating control has to report its outcome, and this one reports it
      // through `CaptureState.reason` rather than by throwing. Throwing was
      // tried first and is wrong here: `setEmulation` is called from the layout
      // pass on every window resize, so a rejection would arrive dozens of times
      // during one drag, from a call nobody awaited — an unhandled rejection
      // storm in the main process, caused by the operator opening the inspector.
      // The reason below is on the channel the chrome already renders.
      this.publish({
        console: 'fallback',
        network: 'off',
        reason:
          'DevTools has the debugger — network capture is paused and the device cannot be changed; console is still recorded',
      })
      return
    }

    if (opts.width && opts.height) {
      await this.send('Emulation.setDeviceMetricsOverride', {
        width: Math.round(opts.width),
        height: Math.round(opts.height),
        // 0 means "use the host's own ratio", which is the right answer for a
        // desktop frame and wrong for a phone preset — hence the explicit value
        // travelling with every device.
        deviceScaleFactor: opts.deviceScaleFactor ?? 0,
        mobile: Boolean(opts.mobile),
        ...(opts.scale !== undefined ? { scale: opts.scale } : {}),
      })
    } else {
      await this.send('Emulation.clearDeviceMetricsOverride')
    }

    await this.send('Emulation.setTouchEmulationEnabled', {
      enabled: Boolean(opts.touch),
      maxTouchPoints: opts.touch ? 5 : 1,
    })

    if (opts.userAgent !== undefined) {
      // On `Network`, not `Emulation`: the Emulation copy is deprecated and does
      // not reach the header the server actually sees.
      await this.send('Network.setUserAgentOverride', { userAgent: opts.userAgent })
    }

    const features: Array<{ name: string; value: string }> = []
    // `auto` clears the feature rather than sending an empty value: an empty
    // string is a *valid* media feature value to Chromium and evaluates to
    // neither light nor dark, so every `prefers-color-scheme` query in the page
    // stops matching — a site that looked fine goes unstyled and the device
    // picker gets blamed.
    if (opts.colorScheme && opts.colorScheme !== 'auto') {
      features.push({ name: 'prefers-color-scheme', value: opts.colorScheme })
    }
    features.push({ name: 'prefers-reduced-motion', value: opts.reducedMotion ? 'reduce' : 'no-preference' })
    features.push({ name: 'forced-colors', value: opts.forcedColors ? 'active' : 'none' })
    await this.send('Emulation.setEmulatedMedia', { features })
  }
}

/* ========================================================================
 * HAR 1.2
 *
 * Exported separately from the class so it can be exercised with no Electron
 * anywhere near it — it is pure, and a pure function is the only part of this
 * file that can be tested without a live browser.
 *
 * `rmux_control::Report::Har` carries this as an **opaque string** on purpose,
 * so the shape only has to satisfy HAR readers and never a Rust deserialiser.
 * ======================================================================== */

export function buildHar(entries: NetworkEntry[], version: string): string {
  return JSON.stringify(
    {
      log: {
        version: '1.2',
        creator: { name: 'R Browser', version },
        entries: entries.map(harEntry),
      },
    },
    null,
    2,
  )
}

function harEntry(e: NetworkEntry): Record<string, unknown> {
  const reqHeaders = harHeaders(e.requestHeaders)
  const resHeaders = harHeaders(e.responseHeaders)
  return {
    startedDateTime: new Date(e.startedAt).toISOString(),
    // `-1` throughout for anything not measured. A HAR reader renders that as
    // "unknown"; it cannot un-believe a number we made up.
    time: e.durationMs ?? -1,
    request: {
      method: e.method,
      url: e.url,
      // Not observed — CDP reports the protocol only in `responseReceived`'s
      // extra info, which is not collected here. Empty rather than a guessed
      // "HTTP/1.1", which would be wrong on every h2 origin.
      httpVersion: '',
      cookies: [],
      headers: reqHeaders,
      queryString: harQueryString(e.url),
      headersSize: -1,
      bodySize: -1,
    },
    response: {
      status: e.status ?? 0,
      statusText: e.statusText ?? '',
      httpVersion: '',
      cookies: [],
      headers: resHeaders,
      content: { size: e.size ?? -1, mimeType: e.mime ?? '' },
      redirectURL: headerValue(e.responseHeaders, 'location') ?? '',
      headersSize: -1,
      bodySize: e.size ?? -1,
    },
    cache: {},
    timings: { send: -1, wait: -1, receive: -1 },
    // Underscore-prefixed fields are the HAR spec's own extension mechanism, so
    // a failure survives export instead of reading as a 0-status success.
    ...(e.failed ? { _failed: true, _errorText: e.errorText ?? '' } : {}),
    ...(e.type ? { _resourceType: e.type } : {}),
    ...(e.fromCache ? { _fromCache: true } : {}),
  }
}

function harHeaders(map: Record<string, string> | undefined): Array<{ name: string; value: string }> {
  if (!map) return []
  return Object.entries(map).map(([name, value]) => ({ name, value: String(value) }))
}

function harQueryString(url: string): Array<{ name: string; value: string }> {
  try {
    const parsed = new URL(url)
    const out: Array<{ name: string; value: string }> = []
    parsed.searchParams.forEach((value, name) => out.push({ name, value }))
    return out
  } catch {
    // `data:`, `blob:` and a malformed URL all land here. An entry with no
    // query string is correct; throwing would lose the whole export.
    return []
  }
}

function headerValue(map: Record<string, string> | undefined, name: string): string | undefined {
  if (!map) return undefined
  for (const [k, v] of Object.entries(map)) if (k.toLowerCase() === name) return v
  return undefined
}

/* ------------------------------------------------------------- formatting */

/** CDP levels, Electron levels and `Log.entryAdded` levels are three vocabularies
 *  for one idea. They are folded here so a filter in the UI is one list, and
 *  anything unrecognised passes through untouched — the contract says an unknown
 *  level must not drop the entry. */
function normaliseLevel(level: string): string {
  switch (level) {
    case 'warning':
      return 'warn'
    case 'verbose':
      return 'debug'
    case 'assert':
      return 'error'
    default:
      return level
  }
}

/** Electron < 35 numbered them. 0 was verbose, not "log". */
const LEGACY_LEVELS: Record<number, string> = { 0: 'debug', 1: 'info', 2: 'warn', 3: 'error' }

/**
 * Turn `console.log`'s arguments into one line.
 *
 * The thing that matters here is that `console.log(obj)` must not arrive as
 * `[object Object]` — an object log rendered that way is worse than absent,
 * because it looks like the page logged nothing useful when in fact it logged
 * the one value someone needed.
 */
function formatArgs(args: RemoteObject[]): string {
  if (!args.length) return ''
  const first = args[0]
  const fmt = first && first.type === 'string' && typeof first.value === 'string' ? first.value : undefined

  if (fmt !== undefined && fmt.includes('%')) {
    const rest = args.slice(1)
    let i = 0
    const head = fmt.replace(/%([sdifoOjc%])/g, (_m: string, spec: string) => {
      if (spec === '%') return '%'
      if (spec === 'c') {
        // **`%c` and its style argument are dropped.** Without this every styled
        // log — and a framework's banner is always styled — arrives with a wodge
        // of CSS in the middle of the message.
        if (i < rest.length) i += 1
        return ''
      }
      if (i >= rest.length) return `%${spec}`
      const arg = rest[i++]
      if (spec === 'd' || spec === 'i') {
        const n = Number(arg?.value ?? arg?.description)
        return Number.isFinite(n) ? String(Math.trunc(n)) : 'NaN'
      }
      if (spec === 'f') {
        const n = Number(arg?.value ?? arg?.description)
        return Number.isFinite(n) ? String(n) : 'NaN'
      }
      return formatRemote(arg)
    })
    const tail = rest.slice(i).map(formatRemote)
    return tail.length ? `${head} ${tail.join(' ')}` : head
  }

  return args.map(formatRemote).join(' ')
}

function formatRemote(o: RemoteObject | undefined): string {
  if (!o) return ''
  if (o.type === 'undefined') return 'undefined'
  if (o.subtype === 'null') return 'null'
  if (o.unserializableValue) return o.unserializableValue
  if (o.type && o.type !== 'object' && o.type !== 'function') {
    if (o.value !== undefined) return String(o.value)
    if (o.description) return o.description
  }
  // The preview is the only thing that carries an object's *contents* across
  // the protocol — `description` for a plain object is the literal string
  // "Object", which is the `[object Object]` failure by another route.
  if (o.preview) return previewToString(o.preview, 0)
  if (o.description) return o.description
  if (o.value !== undefined) {
    try {
      return JSON.stringify(o.value)
    } catch {
      return String(o.value)
    }
  }
  return o.className ?? o.subtype ?? o.type ?? ''
}

function previewToString(p: ObjectPreview, depth: number): string {
  // Two levels. A deeper walk produces a line nobody reads, and CDP stops
  // sending nested previews below that anyway.
  if (depth > 2) return p.description ?? '…'
  const props = p.properties ?? []
  const body = props.map((prop) => propToString(prop, depth)).join(', ')
  const more = p.overflow ? (body ? ', …' : '…') : ''

  if (p.subtype === 'array') return `[${body}${more}]`
  if (p.subtype === 'map' || p.subtype === 'set') {
    const entries = (p.entries ?? [])
      .map((en) =>
        en.key
          ? `${previewToString(en.key, depth + 1)} => ${en.value ? previewToString(en.value, depth + 1) : ''}`
          : en.value
            ? previewToString(en.value, depth + 1)
            : '',
      )
      .join(', ')
    return `${p.description ?? p.subtype} {${entries}${p.overflow ? ', …' : ''}}`
  }
  const label = p.description && p.description !== 'Object' ? `${p.description} ` : ''
  return `${label}{${body}${more}}`
}

function propToString(prop: PropertyPreview, depth: number): string {
  const name = prop.name ?? ''
  let value: string
  if (prop.valuePreview) value = previewToString(prop.valuePreview, depth + 1)
  else if (prop.type === 'string') value = JSON.stringify(prop.value ?? '')
  else if (prop.value !== undefined) value = prop.value
  else value = prop.subtype ?? prop.type ?? 'undefined'
  return `${name}: ${value}`
}

/** `url:line:col functionName`, outermost frame first — the order a stack is
 *  read in. Async parents are appended, because a promise rejection whose own
 *  frames are all framework code is only identifiable from its parent. */
function formatStack(trace: StackTrace | undefined): string | undefined {
  if (!trace) return undefined
  const lines: string[] = []
  let cur: StackTrace | undefined = trace
  let guard = 0
  while (cur && guard++ < 8) {
    for (const f of cur.callFrames ?? []) {
      const where = `${f.url ?? '<anonymous>'}:${(f.lineNumber ?? 0) + 1}:${(f.columnNumber ?? 0) + 1}`
      lines.push(f.functionName ? `${where} ${f.functionName}` : where)
    }
    cur = cur.parent
  }
  return lines.length ? lines.join('\n') : undefined
}

/* ----------------------------------------------------------------- scraps */

function headerMap(raw: unknown): Record<string, string> | undefined {
  if (!raw || typeof raw !== 'object') return undefined
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) out[k] = String(v)
  return out
}

function numberOrUndefined(v: unknown): number | undefined {
  const n = Number(v)
  return Number.isFinite(n) ? n : undefined
}

/** Entry ids carry a `#n` redirect suffix; CDP only knows the bare id. */
function cdpIdOf(entryId: string): string {
  const hash = entryId.indexOf('#')
  return hash === -1 ? entryId : entryId.slice(0, hash)
}

function capBody(text: string): string {
  if (text.length <= MAX_BODY_BYTES) return text
  return `${text.slice(0, MAX_BODY_BYTES)}\n[truncated — ${text.length} bytes exceeds the ${MAX_BODY_BYTES} byte cap]`
}

function isTextual(mime: string | undefined): boolean {
  if (!mime) return false
  return (
    mime.startsWith('text/') ||
    mime.includes('json') ||
    mime.includes('javascript') ||
    mime.includes('xml') ||
    mime.includes('svg') ||
    mime.includes('x-www-form-urlencoded')
  )
}

function message(err: unknown): string {
  return err instanceof Error ? err.message : String(err)
}
