/**
 * One tab: a `WebContentsView` holding a page, plus everything attached to it.
 *
 * **`WebContentsView`, not a `<webview>` tag and not `BrowserView`.** The
 * `<webview>` tag is a second, quietly-different embedder with its own bug
 * surface and its own event names, and `BrowserView` is deprecated. More to the
 * point, a `<webview>` lives *inside* the chrome's document, so a page that
 * crashes or spins takes the browser UI with it. A separate view is a separate
 * renderer process: a page can hang and the tab strip still closes it.
 *
 * The cost is that the page is **not in the chrome's DOM**, so the chrome
 * cannot lay it out. It measures the hole it left and tells us where it is —
 * see `RB.layout`. That is why `setBounds` is called from a message rather than
 * from a CSS rule.
 */
import { WebContentsView, type Session } from 'electron'
import { join } from 'node:path'
import { randomUUID } from 'node:crypto'
import { Capture } from './capture'
import { deviceById } from './devices'
import { RBPage } from '../shared/contract'
import { normaliseUrl } from '../shared/url'
import type {
  CaptureState, ConsoleEntry, CredentialSubmit, ElementEdit, ElementRef, EmulationState,
  FillCredentials, NetworkEntry, PickedFromPage, TabState, Viewport,
} from '../shared/contract'

export interface TabEvents {
  onChanged(): void
  onConsole(tabId: string, entry: ConsoleEntry): void
  onNetwork(tabId: string, entry: NetworkEntry): void
  onCaptureState(state: CaptureState): void
  onPicked(
    tabId: string,
    element: ElementRef,
    viewport: Viewport,
    note?: string,
    edits?: ElementEdit[],
  ): void
  onPickCancelled(tabId: string): void
  /** The page loaded (or SPA-navigated) and wants any saved login for `origin`. */
  onCredentialRequest(tabId: string, origin: string): void
  /** A login was submitted on the page. */
  onCredentialSubmit(tabId: string, sub: CredentialSubmit): void
  /** A full navigation completed — record it in history. */
  onVisit(url: string, title?: string): void
  /** The page's title resolved — update the history entry without re-counting. */
  onTitle(url: string, title: string): void
}

/** Where the chrome says the page belongs, in window coordinates. */
export interface Slot { x: number; y: number; width: number; height: number }

const DEFAULT_EMULATION: EmulationState = {
  deviceId: null,
  rotated: false,
  colorScheme: 'auto',
  reducedMotion: false,
  forcedColors: false,
  zoom: 0,
}

export class Tab {
  readonly id = randomUUID()
  readonly view: WebContentsView
  readonly capture: Capture

  title = 'New tab'
  /** What actually loaded. The chrome keeps the half-typed value separately —
   *  an address bar that rewrites itself mid-edit is unusable. */
  url = ''
  loading = false
  error?: string
  favicon?: string
  emulation: EmulationState = { ...DEFAULT_EMULATION }
  sessionId?: string

  /** Document size reported by the page preload, needed for a full-page shot
   *  and for the fit-to-window zoom. */
  metrics: { viewport: Viewport; scrollWidth: number; scrollHeight: number } | null = null

  private slot: Slot = { x: 0, y: 0, width: 0, height: 0 }
  private remeasureSeq = 0
  private readonly pendingRemeasure = new Map<number, (e: ElementRef | null) => void>()

  constructor(session: Session, private readonly events: TabEvents) {
    this.view = new WebContentsView({
      webPreferences: {
        session,
        preload: join(__dirname, 'preload-page.cjs'),
        // The three that matter, on a view whose entire job is rendering
        // pages nobody here wrote. `sandbox` keeps the preload to the
        // ipcRenderer subset — which is all the picker needs — and means a
        // renderer compromise does not start with a Node handle.
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: true,
        webSecurity: true,
        // Off for the same reason rmux turns it off: with it on, the native
        // window swallows a drop and the page never sees the event.
        // A development browser must be able to have a file dragged into it.
        webviewTag: false,
      },
    })

    this.capture = new Capture(this.id, this.view.webContents, {
      onConsole: (e) => this.events.onConsole(this.id, e),
      onNetwork: (e) => this.events.onNetwork(this.id, e),
      onState: (s) => this.events.onCaptureState(s),
    })

    this.wire()
    this.capture.attach()
  }

  private wire(): void {
    const wc = this.view.webContents

    const changed = () => this.events.onChanged()

    wc.on('page-title-updated', (_e, title) => {
      this.title = title
      if (this.url) this.events.onTitle(this.url, title)
      changed()
    })
    wc.on('page-favicon-updated', (_e, icons) => { this.favicon = icons[0]; changed() })
    wc.on('did-start-loading', () => { this.loading = true; this.error = undefined; changed() })
    wc.on('did-stop-loading', () => { this.loading = false; this.url = wc.getURL(); changed() })
    wc.on('did-navigate', (_e, url) => {
      this.url = url
      // A completed top-frame navigation is a visit worth suggesting later.
      this.events.onVisit(url, this.title)
      changed()
    })
    wc.on('did-navigate-in-page', (_e, url) => { this.url = url; changed() })

    // A failed navigation must say *why*. An empty page with a spinner that
    // stopped is indistinguishable from a hang, which is the one impression
    // this repo's interface rules exist to prevent.
    wc.on('did-fail-load', (_e, code, description, validatedURL, isMainFrame) => {
      if (!isMainFrame) return
      // -3 is ERR_ABORTED, which is what a user-cancelled or superseded
      // navigation reports. Surfacing it as a failure makes every redirect
      // look broken.
      if (code === -3) return
      this.loading = false
      this.error = `${description} (${code}) — ${validatedURL}`
      changed()
    })

    wc.on('render-process-gone', (_e, details) => {
      this.loading = false
      this.error = `the page process ended: ${details.reason}`
      changed()
    })

    // Popups open as ordinary tabs rather than as native windows: a dev
    // browser whose OAuth popup escapes into a chromeless window the operator
    // cannot screenshot or inspect has lost the thing it exists for.
    wc.setWindowOpenHandler(({ url }) => {
      this.events.onChanged()
      this.onPopup?.(url)
      return { action: 'deny' }
    })

    wc.on('devtools-opened', changed)
    wc.on('devtools-closed', changed)

    // The page preload talks back over the view's own ipc, so a message can
    // only have come from a frame in *this* tab — there is no tab id to spoof.
    // The payload is `{ ref, note, edits }` from the in-page comment form. An
    // older preload sends a bare `ElementRef`; tell them apart by the `ref`
    // field so a version skew degrades to a note-less pick rather than throwing.
    wc.ipc.on(RBPage.picked, (_e, payload: PickedFromPage | ElementRef) => {
      const picked = (payload as PickedFromPage).ref
        ? (payload as PickedFromPage)
        : { ref: payload as ElementRef }
      const vp = this.metrics?.viewport ?? { width: 0, height: 0 }
      this.events.onPicked(this.id, picked.ref, vp, picked.note, picked.edits)
    })
    wc.ipc.on(RBPage.cancelled, () => this.events.onPickCancelled(this.id))
    wc.ipc.on(RBPage.credentialRequest, (_e, p: { origin: string }) => {
      this.events.onCredentialRequest(this.id, p?.origin ?? '')
    })
    wc.ipc.on(RBPage.credentialSubmit, (_e, sub: CredentialSubmit) => {
      if (sub && typeof sub.origin === 'string' && typeof sub.password === 'string') {
        this.events.onCredentialSubmit(this.id, {
          origin: sub.origin,
          username: typeof sub.username === 'string' ? sub.username : '',
          password: sub.password,
        })
      }
    })
    wc.ipc.on(RBPage.metrics, (_e, m: { viewport: Viewport; scrollWidth: number; scrollHeight: number }) => {
      this.metrics = m
    })
    wc.ipc.on(RBPage.remeasure, (_e, reply: { requestId: number; element: ElementRef | null }) => {
      const settle = this.pendingRemeasure.get(reply.requestId)
      if (!settle) return
      this.pendingRemeasure.delete(reply.requestId)
      settle(reply.element)
    })
  }

  /** Set by the owner so a popup becomes a tab. */
  onPopup?: (url: string) => void

  /* ------------------------------------------------------------- navigation */

  async navigate(raw: string): Promise<void> {
    this.error = undefined
    await this.view.webContents.loadURL(normaliseUrl(raw)).catch((e: Error) => {
      // `loadURL` rejects on a failed load *as well as* firing did-fail-load.
      // Swallowing it here keeps one path to the message the operator sees.
      this.error ??= e.message
    })
  }

  back(): void { if (this.view.webContents.navigationHistory.canGoBack()) this.view.webContents.navigationHistory.goBack() }
  forward(): void { if (this.view.webContents.navigationHistory.canGoForward()) this.view.webContents.navigationHistory.goForward() }
  reload(hard: boolean): void { hard ? this.view.webContents.reloadIgnoringCache() : this.view.webContents.reload() }
  stop(): void { this.view.webContents.stop() }

  toggleDevtools(): void {
    const wc = this.view.webContents
    if (wc.isDevToolsOpened()) wc.closeDevTools()
    // Detached, always. Docked DevTools resizes the page view out from under
    // the device frame, so a 390px iPhone silently becomes 240px and every
    // measurement taken through it is wrong.
    else wc.openDevTools({ mode: 'detach' })
  }

  /* --------------------------------------------------------------- picking */

  setPickMode(active: boolean, multi: boolean, shoot: boolean): void {
    this.view.webContents.send(RBPage.setPickMode, { active, multi, shoot })
  }

  highlight(element: ElementRef | null): void {
    this.view.webContents.send(RBPage.highlight, element)
  }

  /** Hand a saved login to the page to fill. The page fills only empty fields
   *  on a form that exists, so this is safe to send speculatively. */
  fillCredentials(cred: FillCredentials): void {
    this.view.webContents.send(RBPage.fillCredentials, cred)
  }

  remeasure(element: ElementRef): Promise<ElementRef | null> {
    const requestId = ++this.remeasureSeq
    return new Promise((resolve) => {
      this.pendingRemeasure.set(requestId, resolve)
      this.view.webContents.send(RBPage.remeasure, { requestId, element })
      // A page that navigated away never answers. Without this the map grows
      // and the chrome's row spins for ever on a tab nobody is looking at.
      setTimeout(() => {
        if (!this.pendingRemeasure.delete(requestId)) return
        resolve(null)
      }, 2000)
    })
  }

  /* ------------------------------------------------------------- emulation */

  /**
   * Bounds and CDP metrics are set **together**, from one state.
   *
   * They are two halves of one fact and drift the moment they are set from two
   * places: the view would be 390px wide while the page still believed it was
   * 1200, so media queries and `innerWidth` disagreed with what was on screen.
   * The repo has the same lesson about xterm — never scale a terminal by
   * scaling its container, because it measures what it draws.
   */
  async applyEmulation(state: EmulationState): Promise<void> {
    this.emulation = state
    await this.layout()
  }

  setSlot(slot: Slot): void {
    this.slot = slot
    void this.layout()
  }

  /** The device's CSS size after rotation, or `null` for "fill the slot". */
  private deviceSize(): { width: number; height: number; scale: number; mobile: boolean; touch: boolean; ua?: string } | null {
    const { deviceId, rotated, customWidth, customHeight } = this.emulation
    if (deviceId) {
      const d = deviceById(deviceId)
      if (!d) return null
      const width = rotated ? d.height : d.width
      const height = rotated ? d.width : d.height
      return { width, height, scale: d.deviceScaleFactor, mobile: d.mobile, touch: d.touch, ua: d.userAgent }
    }
    if (customWidth && customHeight) {
      return { width: customWidth, height: customHeight, scale: 0, mobile: false, touch: false }
    }
    return null
  }

  private async layout(): Promise<void> {
    const slot = this.slot
    const device = this.deviceSize()

    if (!device) {
      this.view.setBounds(slot)
      await this.capture.setEmulation({
        colorScheme: this.emulation.colorScheme,
        reducedMotion: this.emulation.reducedMotion,
        forcedColors: this.emulation.forcedColors,
      })
      return
    }

    // `zoom: 0` means fit. Never zoom *up* to fill — a 390px phone blown up to
    // 900px looks like a tablet and invites exactly the wrong conclusion about
    // a layout bug.
    const fit = Math.min(1, slot.width / device.width, slot.height / device.height)
    const zoom = this.emulation.zoom > 0 ? this.emulation.zoom : fit

    const width = Math.round(device.width * zoom)
    const height = Math.round(device.height * zoom)

    this.view.setBounds({
      x: Math.round(slot.x + (slot.width - width) / 2),
      y: Math.round(slot.y + Math.max(0, (slot.height - height) / 2)),
      width,
      height,
    })

    await this.capture.setEmulation({
      width: device.width,
      height: device.height,
      deviceScaleFactor: device.scale || undefined,
      mobile: device.mobile,
      touch: device.touch,
      userAgent: device.ua,
      colorScheme: this.emulation.colorScheme,
      reducedMotion: this.emulation.reducedMotion,
      forcedColors: this.emulation.forcedColors,
      scale: zoom,
    })
  }

  /* ------------------------------------------------------------------ state */

  state(): TabState {
    const wc = this.view.webContents
    const cap = this.capture.state()
    return {
      id: this.id,
      // Filled by the owner, which is the only thing that knows. `Tab` has no
      // view of its siblings by design.
      active: false,
      url: this.url,
      title: this.title || this.url || 'New tab',
      loading: this.loading,
      canGoBack: wc.navigationHistory.canGoBack(),
      canGoForward: wc.navigationHistory.canGoForward(),
      favicon: this.favicon,
      error: this.error,
      emulation: this.emulation,
      devtoolsOpen: wc.isDevToolsOpened(),
      capturing: cap.console === 'cdp' && cap.network === 'cdp',
      sessionId: this.sessionId,
    }
  }

  dispose(): void {
    this.capture.dispose()
    // `close()` rather than only removing the view: a removed view keeps its
    // renderer process alive, so closing twenty tabs during a day's work
    // leaves twenty processes holding their pages' memory.
    this.view.webContents.close()
  }
}
