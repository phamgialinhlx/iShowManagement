// Claude-notifications subsystem, extracted from App.svelte. Owns per-host status,
// badges, the install/uninstall flow, and the live notify WebSockets. Everything it
// needs from the rest of the app (which host is active, which are connected, whether
// the window is on screen) is passed in via `Deps` accessors, so this module imports
// no other store and has no circular dependency.

import {
  getNotifyStatus,
  installNotify,
  uninstallNotify,
  setWatching,
  type NotifyStatus,
  type NotifyEvent,
  type Server,
} from '../api'
import { confirmDialog } from '../dialogs.svelte'

interface Deps {
  connectedHostIds: () => Set<string>
  activeHostId: () => string | undefined
  activeHost: () => Server | undefined
  isLive: () => boolean
  // True only while the app window is actually on screen AND focused.
  appVisible: () => boolean
  // True when the active tab is a live terminal (shell/tmux/docker), not a browser.
  activeIsTerminal: () => boolean
  // tmux session names open (as tabs) per host — reported so core scopes banners.
  openTmuxByHost: () => Record<string, string[]>
}

export class NotifyStore {
  // Status/badges drive UI; dismissed/installing/error back the setup card + bell.
  status = $state<Record<string, NotifyStatus>>({})
  badges = $state<Record<string, number>>({})
  installing = $state<Record<string, boolean>>({})
  error = $state<Record<string, string>>({})
  #dismissed = $state<Record<string, boolean>>({})
  // Open notify WebSockets, keyed by host id. Core pushes Claude events over these;
  // onmessage fires even when the window is hidden (unlike a throttled poll timer).
  #ws: Record<string, WebSocket> = {}

  constructor(private d: Deps) {}

  // Offer the setup card once a remote host is live, reachable, and lacks the hook
  // (until the user installs or dismisses it this session).
  showSetup = $derived.by(() => {
    const h = this.d.activeHost()
    return (
      !!h &&
      !h.isLocal &&
      this.d.isLive() &&
      this.status[h.id]?.reachable === true &&
      this.status[h.id]?.installed === false &&
      !this.#dismissed[h.id]
    )
  })

  // Called every poll tick: learn install status for connected hosts (reuses the
  // ControlMaster), then reconcile the sockets. The sockets themselves push events;
  // this only opens/heals them, so background throttling can't drop notifications.
  tick() {
    for (const id of this.d.connectedHostIds()) this.#ensureStatus(id)
    this.#reconcileWs()
  }

  async install(id: string) {
    this.installing[id] = true
    this.error[id] = ''
    try {
      await installNotify(id)
      this.status[id] = { reachable: true, installed: true }
      this.#reconcileWs() // open the stream now rather than waiting for the next tick
    } catch (e) {
      this.error[id] = String(e)
    } finally {
      this.installing[id] = false
    }
  }

  async uninstall(id: string) {
    if (!(await confirmDialog('Disable Claude notifications on this host? Removes the hook + helper script.', { okLabel: 'Disable', danger: true }))) return
    try {
      await uninstallNotify(id)
      this.status[id] = { reachable: true, installed: false }
      this.#dismissed[id] = true // don't immediately re-offer
    } catch (e) {
      this.error[id] = String(e)
    }
  }

  // Bell toggle when the hook is off: re-offer the setup card.
  reoffer(id: string) {
    this.#dismissed[id] = false
  }

  dismiss(id: string) {
    this.#dismissed[id] = true
  }

  // You're looking at this host now — clear its badge.
  clearBadge(id: string) {
    if (this.badges[id]) this.badges[id] = 0
  }

  // Heartbeat our watched host to core. When the app is backgrounded the webview
  // suspends and these stop, so core (never suspended) fires the banner itself.
  pushWatching() {
    setWatching(this.#watchedHost(), this.d.openTmuxByHost()).catch(() => {})
  }

  teardown() {
    for (const id of Object.keys(this.#ws)) this.#closeWs(id)
  }

  async #ensureStatus(id: string) {
    if (id === '__local__' || this.status[id]) return
    try {
      const st = await getNotifyStatus(id)
      this.status[id] = st
      // Auto-update: the hook is wired but its script predates this app version
      // (e.g. lacks the pane-location column). Silently reinstall to refresh it —
      // install is idempotent, so this just rewrites the script and re-merges.
      if (st.installed && st.current === false) {
        installNotify(id)
          .then(() => (this.status[id] = { ...st, current: true }))
          .catch(() => {
            /* leave it stale; the user can reinstall manually */
          })
      }
    } catch {
      /* transient — retry next tick */
    }
  }

  // Keep a live notify WebSocket open for every connected host that has the hook
  // installed; close the rest. Called every tick, so a dropped socket reopens.
  #reconcileWs() {
    const desired = new Set([...this.d.connectedHostIds()].filter((id) => this.status[id]?.installed))
    for (const id of desired) if (!this.#ws[id]) this.#openWs(id)
    for (const id of Object.keys(this.#ws)) if (!desired.has(id)) this.#closeWs(id)
  }

  #openWs(id: string) {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    const ws = new WebSocket(`${proto}://${location.host}/ws/notify?id=${encodeURIComponent(id)}`)
    this.#ws[id] = ws
    ws.onmessage = (e) => {
      try {
        this.#fireNotify(id, JSON.parse(e.data) as NotifyEvent)
      } catch {
        /* ignore a malformed frame */
      }
    }
    ws.onclose = () => {
      if (this.#ws[id] === ws) delete this.#ws[id] // reconcile reopens if still wanted
    }
    ws.onerror = () => ws.close()
  }

  #closeWs(id: string) {
    const ws = this.#ws[id]
    delete this.#ws[id]
    ws?.close()
  }

  #fireNotify(id: string, _ev: NotifyEvent) {
    // Core raises the OS banner (works while backgrounded); the webview only
    // badges the sidebar, and only when you're not already looking at that host.
    if (this.d.activeHostId() !== id) {
      this.badges[id] = (this.badges[id] ?? 0) + 1
    }
  }

  // The host whose live terminal is genuinely on screen: app focused AND visible
  // AND its active tab is a terminal (shell/tmux/docker) on that host. `null` for
  // any doubt (unfocused, hidden, minimized, another Space, a browser/dashboard
  // tab) — core then fires that host's banner.
  #watchedHost(): string | null {
    if (!this.d.appVisible() || !this.d.activeHostId()) return null
    return this.d.activeIsTerminal() ? this.d.activeHostId()! : null
  }
}
