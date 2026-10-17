/**
 * The chrome's bridge. This is the *only* surface the browser UI has.
 *
 * `contextIsolation` is on and nothing here hands the renderer an `ipcRenderer`
 * or a module loader: every entry is a named function with a fixed channel, so
 * the UI cannot reach a channel that was not deliberately exposed. That matters
 * more than usual here — this app's whole job is rendering untrusted pages, and
 * a chrome XSS with a general IPC handle would be a local shell.
 */
import { contextBridge, ipcRenderer } from 'electron'
import { RB, RBEvent } from '../shared/contract'
import type {
  Annotation, Bookmark, CaptureState, ConsoleEntry, ControlState, CredentialMeta, CredentialOffer,
  DevicePreset, ElementEdit, ElementRef, EmulationState, NetworkEntry, Suggestion, TabState, Viewport,
} from '../shared/contract'

const on = <T>(channel: string, fn: (value: T) => void) => {
  const handler = (_e: unknown, value: T) => fn(value)
  ipcRenderer.on(channel, handler)
  return () => { ipcRenderer.off(channel, handler) }
}

const api = {
  /** The OS, so the renderer can inset its top bar past the macOS traffic
   *  lights (a `hiddenInset` titlebar overlays the window's top-left). */
  platform: process.platform,
  tabs: {
    list: (): Promise<TabState[]> => ipcRenderer.invoke(RB.tabList),
    open: (url?: string): Promise<TabState> => ipcRenderer.invoke(RB.tabNew, url),
    close: (id: string): Promise<void> => ipcRenderer.invoke(RB.tabClose, id),
    activate: (id: string): Promise<void> => ipcRenderer.invoke(RB.tabActivate, id),
    navigate: (id: string, url: string): Promise<void> => ipcRenderer.invoke(RB.tabNavigate, { id, url }),
    back: (id: string): Promise<void> => ipcRenderer.invoke(RB.tabBack, id),
    forward: (id: string): Promise<void> => ipcRenderer.invoke(RB.tabForward, id),
    reload: (id: string, hard?: boolean): Promise<void> => ipcRenderer.invoke(RB.tabReload, { id, hard }),
    stop: (id: string): Promise<void> => ipcRenderer.invoke(RB.tabStop, id),
    devtools: (id: string): Promise<void> => ipcRenderer.invoke(RB.tabDevtools, id),
    onChanged: (fn: (tabs: TabState[]) => void) => on<TabState[]>(RBEvent.tabs, fn),
  },

  /**
   * The chrome owns the layout and *tells* main where the page goes.
   *
   * The alternative — main assuming a toolbar height — puts the same number in
   * two places, and they drift the first time a row is added to the chrome.
   * Sent on every resize; main debounces before it moves the view.
   */
  setLayout: (rect: { x: number; y: number; width: number; height: number }): Promise<void> =>
    ipcRenderer.invoke(RB.layout, rect),

  emulation: {
    devices: (): Promise<DevicePreset[]> => ipcRenderer.invoke(RB.devices),
    set: (id: string, state: EmulationState): Promise<void> => ipcRenderer.invoke(RB.emulationSet, { id, state }),
  },

  pick: {
    start: (id: string, opts: { multi: boolean; shoot: boolean }): Promise<void> =>
      ipcRenderer.invoke(RB.pickStart, { id, ...opts }),
    stop: (id: string): Promise<void> => ipcRenderer.invoke(RB.pickStop, id),
    /** Re-draw the page-side highlight for an annotation being hovered in the
     *  list. `null` clears it. */
    highlight: (id: string, element: ElementRef | null): Promise<void> =>
      ipcRenderer.invoke(RB.highlight, { id, element }),
    /** Ask the page whether a stored selector still resolves, and where it is
     *  now. A note taken before a reload is worth keeping; a note whose element
     *  has *gone* is worth saying so about. */
    remeasure: (id: string, element: ElementRef): Promise<ElementRef | null> =>
      ipcRenderer.invoke(RB.remeasure, { id, element }),
    onPicked: (
      fn: (p: {
        tabId: string
        element: ElementRef
        /** What the operator typed into the in-page comment form. */
        note?: string
        /** Live style/text changes made in the form's edit mode. */
        edits?: ElementEdit[]
        screenshot?: string
        /** Where the same PNG was written. The prompt carries this, not the
         *  base64 — see `Annotation.screenshotPath`. */
        screenshotPath?: string
        viewport: Viewport
      }) => void,
    ) =>
      on<{
        tabId: string
        element: ElementRef
        note?: string
        edits?: ElementEdit[]
        screenshot?: string
        screenshotPath?: string
        viewport: Viewport
      }>(RBEvent.picked, fn),
    onCancelled: (fn: (p: { tabId: string }) => void) => on<{ tabId: string }>(RBEvent.pickCancelled, fn),
  },

  capture: {
    viewport: (id: string): Promise<string> => ipcRenderer.invoke(RB.captureViewport, id),
    fullPage: (id: string): Promise<string> => ipcRenderer.invoke(RB.captureFullPage, id),
    element: (id: string, element: ElementRef): Promise<string> =>
      ipcRenderer.invoke(RB.captureElement, { id, element }),
    region: (id: string, rect: { x: number; y: number; width: number; height: number }): Promise<string> =>
      ipcRenderer.invoke(RB.captureRegion, { id, rect }),
    onState: (fn: (s: CaptureState) => void) => on<CaptureState>(RBEvent.captureState, fn),
  },

  console: {
    get: (id: string): Promise<ConsoleEntry[]> => ipcRenderer.invoke(RB.consoleGet, id),
    clear: (id: string): Promise<void> => ipcRenderer.invoke(RB.consoleClear, id),
    onEntry: (fn: (e: ConsoleEntry & { tabId: string }) => void) =>
      on<ConsoleEntry & { tabId: string }>(RBEvent.consoleEntry, fn),
  },

  network: {
    get: (id: string): Promise<NetworkEntry[]> => ipcRenderer.invoke(RB.networkGet, id),
    clear: (id: string): Promise<void> => ipcRenderer.invoke(RB.networkClear, id),
    har: (id: string): Promise<string> => ipcRenderer.invoke(RB.networkHar, id),
    /** Bodies are fetched on demand, never streamed with the entry: they are
     *  large and frequently hold tokens, and a log that quietly accumulates
     *  every response body is a credential store nobody meant to build. */
    body: (id: string, requestId: string): Promise<{ request?: string; response?: string }> =>
      ipcRenderer.invoke(RB.networkBody, { id, requestId }),
    onEntry: (fn: (e: NetworkEntry & { tabId: string }) => void) =>
      on<NetworkEntry & { tabId: string }>(RBEvent.networkEntry, fn),
  },

  clipboard: {
    text: (value: string): Promise<void> => ipcRenderer.invoke(RB.clipboardText, value),
    /** Base64 PNG, no `data:` prefix. */
    image: (png: string): Promise<void> => ipcRenderer.invoke(RB.clipboardImage, png),
  },

  savePng: (png: string, suggested: string): Promise<string | null> =>
    ipcRenderer.invoke(RB.savePng, { png, suggested }),

  /**
   * Annotations are persisted by main, in the app's user-data directory.
   *
   * Not `localStorage`: a screenshot is a megabyte of base64 and that quota is
   * one shared bucket, so a morning's annotations would start silently failing
   * to save. The same mistake rmux's background picture avoided.
   */
  store: {
    get: (): Promise<Annotation[]> => ipcRenderer.invoke(RB.storeGet),
    set: (annotations: Annotation[]): Promise<void> => ipcRenderer.invoke(RB.storeSet, annotations),
  },

  /** Browsing data for the active tab's partition — cookies and cache persist
   *  like a normal browser, and this is the way to wipe them. `'cache'` clears
   *  only the HTTP cache (a hard reload that actually re-fetches); `'all'` also
   *  drops cookies, localStorage and the rest, which signs the site out. */
  data: {
    clear: (scope: 'cache' | 'all'): Promise<void> => ipcRenderer.invoke(RB.dataClear, scope),
  },

  /** The saved-password manager, and the answers to a save offer. `list` and
   *  `reveal` return metadata and, only on an explicit reveal, one plaintext —
   *  the renderer never holds the whole set of secrets. */
  passwords: {
    list: (): Promise<CredentialMeta[]> => ipcRenderer.invoke(RB.passwordsList),
    reveal: (origin: string, username: string): Promise<string | null> =>
      ipcRenderer.invoke(RB.passwordReveal, { origin, username }),
    remove: (origin: string, username: string): Promise<void> =>
      ipcRenderer.invoke(RB.passwordDelete, { origin, username }),
    /** Commit the login the given tab just submitted. */
    save: (tabId: string): Promise<{ ok: boolean; error?: string }> =>
      ipcRenderer.invoke(RB.passwordSaveOffer, { tabId }),
    /** Drop the offer. `never` also blocks future offers for that origin. */
    dismiss: (tabId: string, never = false): Promise<void> =>
      ipcRenderer.invoke(RB.passwordDismissOffer, { tabId, never }),
    onOffer: (fn: (o: CredentialOffer) => void) => on<CredentialOffer>(RBEvent.credentialOffer, fn),
  },

  /** Address-bar history suggestions and the bookmark set. */
  history: {
    suggest: (query: string, limit = 8): Promise<Suggestion[]> =>
      ipcRenderer.invoke(RB.historySuggest, { query, limit }),
    clear: (): Promise<void> => ipcRenderer.invoke(RB.historyClear),
  },
  bookmarks: {
    list: (): Promise<Bookmark[]> => ipcRenderer.invoke(RB.bookmarksList),
    add: (url: string, title?: string): Promise<void> => ipcRenderer.invoke(RB.bookmarkAdd, { url, title }),
    remove: (url: string): Promise<void> => ipcRenderer.invoke(RB.bookmarkRemove, { url }),
    has: (url: string): Promise<boolean> => ipcRenderer.invoke(RB.bookmarkHas, { url }),
  },

  control: {
    status: (): Promise<ControlState> => ipcRenderer.invoke(RB.controlStatus),
    connect: (): Promise<ControlState> => ipcRenderer.invoke(RB.controlConnect),
    activate: (sessionId: string): Promise<void> => ipcRenderer.invoke(RB.controlActivate, sessionId),
    proxy: (sessionId: string): Promise<{ port: number }> => ipcRenderer.invoke(RB.controlProxy, sessionId),
    report: (sessionId: string, annotations: Annotation[]): Promise<void> =>
      ipcRenderer.invoke(RB.controlReport, { sessionId, annotations }),
    onState: (fn: (s: ControlState) => void) => on<ControlState>(RBEvent.controlState, fn),
  },

  onOpenUrl: (fn: (p: { url: string; sessionId?: string }) => void) =>
    on<{ url: string; sessionId?: string }>(RBEvent.openUrl, fn),
  /** Menu accelerators arrive here rather than being bound in the renderer:
   *  the page view holds focus almost all the time, so a `keydown` listener in
   *  the chrome would never see them. */
  onShortcut: (fn: (name: string) => void) => on<string>(RBEvent.shortcut, fn),
}

export type RBrowseApi = typeof api

contextBridge.exposeInMainWorld('rbrowse', api)
