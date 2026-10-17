/**
 * R BROWSER — main process.
 *
 * The development browser for rmux. It exists because rmux's own window
 * cannot be one: rmux is a single WKWebView, and a per-session proxy would
 * proxy rmux's own interface. `crates/rmux-control` was written for this app
 * before it existed, which is why the protocol already calls its client
 * `rbrowse`.
 *
 * What lives here and nowhere else:
 *
 *   - the window, and the fact that the page is a **sibling** of the chrome
 *     rather than a child of it;
 *   - tab lifecycle and the Chromium partition each tab belongs to;
 *   - the accelerators, because the page view holds keyboard focus almost all
 *     the time and a `keydown` listener in the chrome would never fire;
 *   - the one connection to rmux.
 */
import {
  app, BaseWindow, WebContentsView, ipcMain, session as electronSession,
  clipboard, ClipboardItem, dialog, Menu, shell, type Session,
} from 'electron'
import { join } from 'node:path'
import { randomUUID } from 'node:crypto'
import { writeFile, mkdir } from 'node:fs/promises'
import { RB, RBEvent } from '../shared/contract'
import type {
  Annotation, CaptureState, ConsoleEntry, ControlState, CredentialOffer, CredentialSubmit,
  ElementEdit, ElementRef, EmulationState, NetworkEntry, PageMetrics, TabState,
} from '../shared/contract'
import { Tab, type Slot } from './tab'
import { DEVICES } from './devices'
import { Store } from './store'
import { Passwords } from './passwords'
import { History, Bookmarks } from './history'
import { PROBE_EXPRESSION, CLEAR_EXPRESSION, toSourceHint } from './runtime-probe'
import { Control } from './control'

const DEV_URL = process.env.RB_DEV_URL

/** Where the page goes until the chrome has measured itself. Zero-height, not
 *  zero-everything: a view with no bounds at all flashes at 0,0 full size for
 *  one frame on every launch, which reads as the page loading in the wrong
 *  place. */
const NO_SLOT: Slot = { x: 0, y: 0, width: 0, height: 0 }

class Browser {
  private window!: BaseWindow
  private chrome!: WebContentsView
  private readonly tabs = new Map<string, Tab>()
  private order: string[] = []
  private activeId: string | null = null
  private slot: Slot = NO_SLOT
  private readonly store = new Store()
  private readonly passwords = new Passwords()
  private readonly history = new History()
  private readonly bookmarks = new Bookmarks()
  /** A submitted login awaiting the operator's Save, held per tab so its
   *  plaintext never travels to the renderer — Save just names the tab. */
  private readonly pendingCredential = new Map<string, CredentialSubmit>()
  private control!: Control

  /** One Chromium partition per rmux session, so a SOCKS proxy opened for one
   *  session's target applies to that session's tabs and nothing else. This is
   *  the capability rmux's single webview does not have, and the entire reason
   *  a separate browser is worth building. */
  private readonly partitions = new Map<string, Session>()

  async start(): Promise<void> {
    this.window = new BaseWindow({
      width: 1500,
      height: 980,
      minWidth: 900,
      minHeight: 600,
      title: 'R Browser',
      backgroundColor: '#060606',
      titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
      trafficLightPosition: { x: 14, y: 14 },
    })

    this.chrome = new WebContentsView({
      webPreferences: {
        preload: join(__dirname, 'preload-chrome.cjs'),
        contextIsolation: true,
        nodeIntegration: false,
        // The chrome is our own code, but it renders page-derived strings —
        // URLs, console text, element text. Sandboxed anyway: a chrome XSS
        // with a Node handle would be a local shell, and the whole app is
        // pointed at pages nobody here wrote.
        sandbox: true,
      },
    })

    this.window.contentView.addChildView(this.chrome)
    this.layoutChrome()
    this.window.on('resize', () => this.layoutChrome())

    if (DEV_URL) await this.chrome.webContents.loadURL(DEV_URL)
    else await this.chrome.webContents.loadFile(join(__dirname, '../renderer/index.html'))

    this.control = new Control(
      (s) => this.send(RBEvent.controlState, s),
      ({ url, sessionId, focus }) => {
        void this.openTab(url, sessionId)
        if (focus) this.window.focus()
      },
    )
    // rmux being closed is the ordinary case, not an error: this app must work
    // on its own. `connect` resolves with `connected:false` rather than
    // throwing, and retries on a slow timer.
    void this.control.connect()

    this.buildMenu()
    this.registerIpc()

    await this.openTab(process.env.RB_START_URL ?? 'about:blank')
  }

  /* ---------------------------------------------------------------- layout */

  /** The chrome always fills the window; the *page* is positioned inside the
   *  hole the chrome reports. Two different jobs, deliberately not one. */
  private layoutChrome(): void {
    const { width, height } = this.window.getContentBounds()
    this.chrome.setBounds({ x: 0, y: 0, width, height })
  }

  private setSlot(slot: Slot): void {
    this.slot = slot
    const tab = this.active()
    tab?.setSlot(slot)
  }

  /* ------------------------------------------------------------------ tabs */

  private active(): Tab | null {
    return this.activeId ? this.tabs.get(this.activeId) ?? null : null
  }

  private partitionFor(sessionId?: string): Session {
    const key = sessionId ?? 'default'
    const existing = this.partitions.get(key)
    if (existing) return existing
    // `persist:` so a login survives a restart. A development browser that
    // forgets the staging site's session on every launch is one nobody uses.
    const ses = electronSession.fromPartition(`persist:rb-${key}`)
    this.partitions.set(key, ses)
    return ses
  }

  private async openTab(url?: string, sessionId?: string): Promise<TabState> {
    const tab = new Tab(this.partitionFor(sessionId), {
      onChanged: () => this.pushTabs(),
      onConsole: (tabId, entry) => this.send<ConsoleEntry & { tabId: string }>(RBEvent.consoleEntry, { ...entry, tabId }),
      onNetwork: (tabId, entry) => this.send<NetworkEntry & { tabId: string }>(RBEvent.networkEntry, { ...entry, tabId }),
      onCaptureState: (state) => this.send<CaptureState>(RBEvent.captureState, state),
      onPicked: (tabId, element, _vp, note, edits) => void this.onPicked(tabId, element, note, edits),
      onPickCancelled: (tabId) => this.send(RBEvent.pickCancelled, { tabId }),
      onCredentialRequest: (tabId, origin) => void this.onCredentialRequest(tabId, origin),
      onCredentialSubmit: (tabId, sub) => void this.onCredentialSubmit(tabId, sub),
      onVisit: (url, title) => void this.history.visit(url, title),
      onTitle: (url, title) => void this.history.retitle(url, title),
    })
    tab.sessionId = sessionId
    tab.onPopup = (popupUrl) => void this.openTab(popupUrl, sessionId)

    this.tabs.set(tab.id, tab)
    this.order.push(tab.id)
    this.activate(tab.id)
    if (url) await tab.navigate(url)
    this.pushTabs()
    return this.states().find((t) => t.id === tab.id) ?? tab.state()
  }

  private activate(id: string): void {
    const tab = this.tabs.get(id)
    if (!tab) return

    // Remove the outgoing view rather than leaving it behind the incoming one.
    // Stacked views still composite, and on a page that animates that is a
    // hidden tab burning GPU for the whole session.
    const current = this.active()
    if (current && current.id !== id) this.window.contentView.removeChildView(current.view)

    this.activeId = id
    this.window.contentView.addChildView(tab.view)
    tab.setSlot(this.slot)
    this.pushTabs()
  }

  private closeTab(id: string): void {
    const tab = this.tabs.get(id)
    if (!tab) return

    const index = this.order.indexOf(id)
    this.order = this.order.filter((t) => t !== id)
    this.tabs.delete(id)

    if (this.activeId === id) {
      this.activeId = null
      this.window.contentView.removeChildView(tab.view)
      // Land on the neighbour, the way every browser does. Landing on the
      // first tab instead means closing tab 9 of 10 throws the operator back
      // to the start of the strip.
      const next = this.order[Math.min(index, this.order.length - 1)]
      if (next) this.activate(next)
    } else {
      this.window.contentView.removeChildView(tab.view)
    }

    tab.dispose()
    this.pushTabs()
    if (!this.order.length) void this.openTab('about:blank')
  }

  /**
   * One place decides what a tab looks like on the wire.
   *
   * `Tab.state()` cannot fill `active` — a tab has no view of its siblings, by
   * design — so it is stamped here. Built in one function because the list is
   * also answered synchronously by `tabList`, and two constructions of the same
   * record is how one of them ends up without the flag.
   */
  private states(): TabState[] {
    return this.order
      .map((id) => this.tabs.get(id))
      .filter((t): t is Tab => !!t)
      .map((t) => ({ ...t.state(), active: t.id === this.activeId }))
  }

  private pushTabs(): void {
    this.send(RBEvent.tabs, this.states())
  }

  private send<T>(channel: string, value: T): void {
    if (this.chrome?.webContents.isDestroyed()) return
    this.chrome.webContents.send(channel, value)
  }

  /* --------------------------------------------------------------- picking */

  /**
   * A pick is completed *here*, not in the page.
   *
   * The screenshot has to come from CDP — the page cannot shoot itself — and
   * the viewport has to be the emulated one rather than whatever
   * `window.innerWidth` says. Doing it in main also means the chrome receives
   * one complete event rather than assembling three.
   */
  private async onPicked(
    tabId: string,
    element: ElementRef,
    note?: string,
    edits?: ElementEdit[],
  ): Promise<void> {
    const tab = this.tabs.get(tabId)
    if (!tab) return

    let screenshot: string | undefined
    try {
      // Pad the clip a little: an element shot cropped exactly to its border
      // box loses the very thing most complaints are about — its spacing
      // against its neighbours.
      const pad = 8
      screenshot = await tab.capture.captureRect({
        x: Math.max(0, element.rect.x - pad),
        y: Math.max(0, element.rect.y - pad),
        width: element.rect.width + pad * 2,
        height: element.rect.height + pad * 2,
      })
    } catch {
      // A shot that failed must not lose the pick. The note and the selector
      // are the payload; the image is a convenience.
      screenshot = undefined
    }

    // Ask the page's own world what component this is. Only reached here —
    // the preload is an isolated world and cannot see a framework expando at
    // all. Merged rather than overwriting: a build-time `data-*` attribute is
    // authoritative about the *file*, while the runtime probe is usually the
    // only thing that knows the *component name*, and each fills what the
    // other lacks.
    const runtime = toSourceHint(await tab.capture.evaluate<unknown>(PROBE_EXPRESSION))
    await tab.capture.evaluate<boolean>(CLEAR_EXPRESSION)
    if (runtime) {
      const stamped = element.source
      element.source = stamped
        ? {
            ...stamped,
            component: stamped.component ?? runtime.component,
            owners: stamped.owners ?? runtime.owners,
            // Both `via` strings are kept, because the reader is being asked to
            // judge how much to trust the file — and "two mechanisms agreed" is
            // a different claim from either one alone.
            via: `${stamped.via} + ${runtime.via}`,
          }
        : runtime
    }

    const metrics = tab.metrics as PageMetrics | null
    this.send(RBEvent.picked, {
      tabId,
      element,
      note,
      edits,
      screenshot,
      screenshotPath: screenshot ? await this.writeShot(screenshot) : undefined,
      viewport: metrics?.viewport ?? { width: 0, height: 0 },
    })
  }

  /* -------------------------------------------------------- credentials */

  /** The page asked for any saved login for its origin — fill the most recent
   *  match. Nothing is sent when there is none, so an ordinary page is untouched. */
  private async onCredentialRequest(tabId: string, origin: string): Promise<void> {
    const tab = this.tabs.get(tabId)
    if (!tab || !origin) return
    const matches = await this.passwords.forOrigin(origin)
    if (!matches.length) return
    const best = matches[0]
    tab.fillCredentials({ username: best.username, password: best.password })
  }

  /** A login was submitted. Decide whether it is worth offering to save, hold
   *  the plaintext against the tab, and ask the renderer to raise the bar. The
   *  password itself never leaves main — the offer carries only the address. */
  private async onCredentialSubmit(tabId: string, sub: CredentialSubmit): Promise<void> {
    const kind = await this.passwords.offerKind(sub)
    if (!kind) return
    this.pendingCredential.set(tabId, sub)
    this.send<CredentialOffer>(RBEvent.credentialOffer, {
      tabId,
      origin: sub.origin,
      username: sub.username,
      kind,
    })
  }

  /**
   * Put the PNG on disk and return where.
   *
   * The prompt carries this path rather than the image, because the agent
   * reading it runs in a terminal: it can open a file, and it cannot reach a
   * clipboard. Written at pick time rather than when a prompt is built — by
   * then the operator may have copied the note hours later, and a temp file
   * created on demand is one more thing to fail at exactly the wrong moment.
   *
   * A failed write must not lose the pick, so this never throws; the note
   * simply has no path and the prompt says nothing about an image.
   */
  private async writeShot(png: string): Promise<string | undefined> {
    try {
      const dir = join(app.getPath('userData'), 'shots')
      await mkdir(dir, { recursive: true })
      // Date-stamped and random: the operator ends up reading these paths in a
      // prompt, and `shot-2026-09-21T14-02-08-a3f1.png` says which pick it was.
      const stamp = new Date().toISOString().replace(/[:.]/g, '-')
      const path = join(dir, `shot-${stamp}-${randomUUID().slice(0, 4)}.png`)
      await writeFile(path, Buffer.from(png, 'base64'))
      return path
    } catch {
      return undefined
    }
  }

  /* ------------------------------------------------------------------- ipc */

  private tabFrom(id: string): Tab {
    const tab = this.tabs.get(id)
    // Throwing across `invoke` rejects the renderer's promise with this
    // message, which the chrome shows inline. Returning `undefined` instead
    // would make a stale tab id look like a button that does nothing.
    if (!tab) throw new Error(`no such tab: ${id}`)
    return tab
  }

  private registerIpc(): void {
    const h = <T>(channel: string, fn: (...args: never[]) => T | Promise<T>) =>
      ipcMain.handle(channel, (_e, ...args) => (fn as (...a: unknown[]) => T | Promise<T>)(...args))

    h(RB.tabList, () => this.states())
    h(RB.tabNew, (url?: string) => this.openTab(url))
    h(RB.tabClose, (id: string) => this.closeTab(id))
    h(RB.tabActivate, (id: string) => this.activate(id))
    h(RB.tabNavigate, ({ id, url }: { id: string; url: string }) => this.tabFrom(id).navigate(url))
    h(RB.tabBack, (id: string) => this.tabFrom(id).back())
    h(RB.tabForward, (id: string) => this.tabFrom(id).forward())
    h(RB.tabReload, ({ id, hard }: { id: string; hard?: boolean }) => this.tabFrom(id).reload(!!hard))
    h(RB.tabStop, (id: string) => this.tabFrom(id).stop())
    h(RB.tabDevtools, (id: string) => this.tabFrom(id).toggleDevtools())

    h(RB.layout, (slot: Slot) => this.setSlot(slot))
    h(RB.devices, () => DEVICES)
    h(RB.emulationSet, ({ id, state }: { id: string; state: EmulationState }) =>
      this.tabFrom(id).applyEmulation(state))

    h(RB.pickStart, ({ id, multi, shoot }: { id: string; multi: boolean; shoot: boolean }) =>
      this.tabFrom(id).setPickMode(true, multi, shoot))
    h(RB.pickStop, (id: string) => this.tabFrom(id).setPickMode(false, false, false))
    h(RB.highlight, ({ id, element }: { id: string; element: ElementRef | null }) =>
      this.tabFrom(id).highlight(element))
    h(RB.remeasure, ({ id, element }: { id: string; element: ElementRef }) =>
      this.tabFrom(id).remeasure(element))

    h(RB.captureViewport, (id: string) => this.tabFrom(id).capture.captureViewport())
    h(RB.captureFullPage, (id: string) => this.tabFrom(id).capture.captureFullPage())
    h(RB.captureElement, ({ id, element }: { id: string; element: ElementRef }) =>
      this.tabFrom(id).capture.captureRect(element.rect))
    h(RB.captureRegion, ({ id, rect }: { id: string; rect: Slot }) =>
      this.tabFrom(id).capture.captureRect(rect))

    h(RB.consoleGet, (id: string) => this.tabFrom(id).capture.consoleEntries())
    h(RB.consoleClear, (id: string) => this.tabFrom(id).capture.clearConsole())
    h(RB.networkGet, (id: string) => this.tabFrom(id).capture.networkEntries())
    h(RB.networkClear, (id: string) => this.tabFrom(id).capture.clearNetwork())
    h(RB.networkHar, (id: string) => this.tabFrom(id).capture.har())
    h(RB.networkBody, ({ id, requestId }: { id: string; requestId: string }) =>
      this.tabFrom(id).capture.body(requestId))

    h(RB.clipboardText, (value: string) => clipboard.writeText(value))
    // Electron 44 replaced the synchronous `writeImage` with the W3C-shaped
    // `write([ClipboardItem])`. Worth a comment because the old call still
    // reads correctly and is simply gone: the failure on an older-typed build
    // is a compile error, but on a mixed install it is a clipboard that
    // silently receives nothing.
    h(RB.clipboardImage, (png: string) =>
      clipboard.write([
        new ClipboardItem({ 'image/png': new Blob([Buffer.from(png, 'base64')], { type: 'image/png' }) }),
      ]))

    h(RB.savePng, async ({ png, suggested }: { png: string; suggested: string }) => {
      const { canceled, filePath } = await dialog.showSaveDialog(this.window, {
        defaultPath: suggested,
        filters: [{ name: 'PNG', extensions: ['png'] }],
      })
      if (canceled || !filePath) return null
      await writeFile(filePath, Buffer.from(png, 'base64'))
      return filePath
    })

    h(RB.storeGet, () => this.store.get())
    h(RB.storeSet, (annotations: Annotation[]) => this.store.set(annotations))

    // Saved-password manager. `list`/`delete` need no plaintext; `reveal` hands
    // one password back on an explicit click. Save/dismiss act on the pending
    // offer held in main, so the secret is committed without ever round-tripping
    // through the renderer.
    h(RB.passwordsList, () => this.passwords.list())
    h(RB.passwordReveal, (p: { origin: string; username: string }) =>
      this.passwords.reveal(p.origin, p.username))
    h(RB.passwordDelete, (p: { origin: string; username: string }) =>
      this.passwords.remove(p.origin, p.username))
    h(RB.passwordSaveOffer, async (p: { tabId: string }) => {
      const pending = this.pendingCredential.get(p.tabId)
      if (!pending) return { ok: false, error: 'nothing to save' }
      this.pendingCredential.delete(p.tabId)
      try {
        await this.passwords.save(pending)
        return { ok: true }
      } catch (e) {
        return { ok: false, error: (e as Error).message }
      }
    })
    h(RB.passwordDismissOffer, async (p: { tabId: string; never?: boolean }) => {
      const pending = this.pendingCredential.get(p.tabId)
      this.pendingCredential.delete(p.tabId)
      if (p.never && pending) await this.passwords.block(pending.origin)
    })

    // Address-bar suggestions: bookmarks first, then history, de-duplicated by
    // URL (a bookmarked page is also in history, and showing it twice is noise).
    h(RB.historySuggest, async (p: { query: string; limit?: number }) => {
      const limit = p.limit ?? 8
      const marks = await this.bookmarks.suggest(p.query, limit)
      const seen = new Set(marks.map((m) => m.url))
      const hist = (await this.history.suggest(p.query, limit + marks.length))
        .filter((h2) => !seen.has(h2.url))
      return [...marks, ...hist].slice(0, limit)
    })
    h(RB.historyClear, () => this.history.clear())
    h(RB.bookmarksList, () => this.bookmarks.list())
    h(RB.bookmarkAdd, (p: { url: string; title?: string }) => this.bookmarks.add(p.url, p.title))
    h(RB.bookmarkRemove, (p: { url: string }) => this.bookmarks.remove(p.url))
    h(RB.bookmarkHas, (p: { url: string }) => this.bookmarks.has(p.url))

    // Wipe the active tab's browsing data. Its partition is shared by every tab
    // on the same rmux session, so clearing it here clears it for all of them —
    // which is what "clear the cache for this site" means to someone who opened
    // it in three tabs. `'all'` also drops cookies and storage, so it is the
    // explicit sign-out, kept a separate choice from a plain cache clear.
    h(RB.dataClear, async (scope: 'cache' | 'all') => {
      const ses = this.active()?.view.webContents.session
      if (!ses) return
      if (scope === 'all') await ses.clearStorageData()
      await ses.clearCache()
    })

    h(RB.controlStatus, (): ControlState => this.control.state())
    h(RB.controlConnect, () => this.control.connect())
    h(RB.controlActivate, (sessionId: string) => this.control.activate(sessionId))
    h(RB.controlProxy, async (sessionId: string) => {
      const port = await this.control.openProxy(sessionId)
      // `socks5://` and not an http proxy: SOCKS5 in Chromium resolves DNS at
      // the far end, so an internal hostname resolves on the *server*. That is
      // the half that actually removes port forwarding — a `-L` tunnel carries
      // one port the operator had to know in advance.
      await this.partitionFor(sessionId).setProxy({ proxyRules: `socks5://127.0.0.1:${port}` })
      return { port }
    })
    h(RB.controlReport, async ({ sessionId, annotations }: { sessionId: string; annotations: Annotation[] }) => {
      // Filed one at a time so a single oversized screenshot cannot fail the
      // batch — the operator would have no way to tell which note was lost.
      for (const a of annotations) await this.control.reportSelection(sessionId, a)
    })
  }

  /* ----------------------------------------------------------------- menu */

  /**
   * Accelerators live in the application menu, not in the chrome's DOM.
   *
   * The page view holds focus for almost the whole session, so a `keydown`
   * handler in the renderer would fire only in the rare moments the operator
   * had clicked the toolbar. The menu catches the chord wherever focus is and
   * forwards a *name* — the chrome decides what it means, so a rebind is a
   * renderer change rather than a two-file one.
   */
  private buildMenu(): void {
    const shortcut = (name: string) => () => this.send(RBEvent.shortcut, name)
    const mod = process.platform === 'darwin' ? 'Cmd' : 'Ctrl'

    const template: Electron.MenuItemConstructorOptions[] = [
      ...(process.platform === 'darwin'
        ? [{ role: 'appMenu' as const }]
        : []),
      {
        label: 'File',
        submenu: [
          { label: 'New Tab', accelerator: `${mod}+T`, click: () => void this.openTab('about:blank') },
          { label: 'Close Tab', accelerator: `${mod}+W`, click: () => this.activeId && this.closeTab(this.activeId) },
          { type: 'separator' },
          { label: 'Save Screenshot…', accelerator: `${mod}+S`, click: shortcut('shot-save') },
          ...(process.platform === 'darwin' ? [] : [{ role: 'quit' as const }]),
        ],
      },
      // Edit is not decoration: without the standard roles, Cmd-C and Cmd-V do
      // not work in the address bar or the note field. Electron does not
      // install them for you, and the repo has already paid for a missing
      // clipboard path once.
      { role: 'editMenu' },
      {
        label: 'View',
        submenu: [
          { label: 'Reload', accelerator: `${mod}+R`, click: () => this.active()?.reload(false) },
          { label: 'Hard Reload', accelerator: `${mod}+Shift+R`, click: () => this.active()?.reload(true) },
          { label: 'Toggle DevTools', accelerator: `${mod}+Alt+I`, click: () => this.active()?.toggleDevtools() },
          { type: 'separator' },
          { label: 'Back', accelerator: `${mod}+Left`, click: () => this.active()?.back() },
          { label: 'Forward', accelerator: `${mod}+Right`, click: () => this.active()?.forward() },
          { type: 'separator' },
          { label: 'Toggle Inspector Panel', accelerator: `${mod}+B`, click: shortcut('toggle-panel') },
          { label: 'Focus Address Bar', accelerator: `${mod}+L`, click: shortcut('focus-address') },
        ],
      },
      {
        label: 'Capture',
        submenu: [
          { label: 'Pick Element', accelerator: `${mod}+Shift+C`, click: shortcut('pick') },
          { label: 'Pick Several', accelerator: `${mod}+Shift+M`, click: shortcut('pick-multi') },
          { type: 'separator' },
          { label: 'Shoot Viewport', accelerator: `${mod}+Shift+4`, click: shortcut('shot-viewport') },
          { label: 'Shoot Full Page', accelerator: `${mod}+Shift+5`, click: shortcut('shot-full') },
          { type: 'separator' },
          { label: 'Copy Prompt from Selection', accelerator: `${mod}+Shift+P`, click: shortcut('copy-prompt') },
          { label: 'Copy Console', accelerator: `${mod}+Shift+J`, click: shortcut('copy-console') },
          { label: 'Copy Network as HAR', accelerator: `${mod}+Shift+H`, click: shortcut('copy-har') },
        ],
      },
      {
        role: 'help',
        submenu: [
          { label: 'rmux control protocol', click: () => void shell.openExternal('https://github.com/') },
        ],
      },
    ]

    Menu.setApplicationMenu(Menu.buildFromTemplate(template))
  }
}

/* ------------------------------------------------------------------ launch */

// One instance. Two R Browsers would each hold their own rmux control
// connection and their own annotation store, and the second one's writes would
// silently overwrite the first's — the store is a single file.
if (!app.requestSingleInstanceLock()) {
  app.quit()
} else {
  const browser = new Browser()

  app.whenReady().then(() => {
    void browser.start()
    app.on('activate', () => { if (!BaseWindow.getAllWindows().length) void browser.start() })
  })

  app.on('window-all-closed', () => { if (process.platform !== 'darwin') app.quit() })

  // A panic must say why it panicked — the same rule rmux's `logs.rs` enforces.
  // A GUI process has nowhere for stderr to go, so an unhandled rejection that
  // only prints is an event nobody ever sees.
  process.on('uncaughtException', (err) => {
    console.error('[r-browser] uncaught exception on', process.title, err)
  })
  process.on('unhandledRejection', (reason) => {
    console.error('[r-browser] unhandled rejection:', reason)
  })
}
