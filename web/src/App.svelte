<script lang="ts">
  import { onMount } from 'svelte'
  import Terminal from './lib/Terminal.svelte'
  import Managers from './lib/Managers.svelte'
  import Files from './lib/Files.svelte'
  import BrowserPanel from './lib/BrowserPanel.svelte'
  import StatusBar from './lib/StatusBar.svelte'
  import HostTmuxTree from './lib/HostTmuxTree.svelte'
  import Home from './lib/Home.svelte'
  import { isMac } from './lib/platform'
  import ClaudeNotifySetup from './lib/ClaudeNotifySetup.svelte'
  import Dialog from './lib/Dialog.svelte'
  import { confirmDialog, promptDialog } from './lib/dialogs.svelte'
  import {
    listServers,
    refreshServers,
    setPassword,
    clearPassword,
    touchServer,
    unforwardPort,
    stopProxy,
    getTunnels,
    setWatching,
    getNotifyStatus,
    installNotify,
    uninstallNotify,
    tmuxSelect,
    getFeatures,
    type Server,
    type Tunnels,
    type NotifyStatus,
    type NotifyEvent,
    type ClaudeInstance,
  } from './lib/api'

  type Kind = 'shell' | 'tmux' | 'docker-logs' | 'docker-exec' | 'browser'
  type Panel = 'overview' | 'docker' | 'ports' | 'processes' | 'files'
  const PANELS: Panel[] = ['overview', 'docker', 'ports', 'processes', 'files']

  interface Session {
    key: number
    hostId: string
    kind: Kind
    cid?: string
    session?: string
    title: string
  }
  type Active = { type: 'panel'; panel: Panel } | { type: 'session'; key: number }

  let servers = $state<Server[]>([])
  let error = $state('')

  let activeHostId = $state<string | undefined>()
  let sessions = $state<Session[]>([])
  let hostActive = $state<Record<string, Active>>({})
  let tunnels = $state<Tunnels>({ forwards: [], proxies: [] })
  let nextKey = 1

  // Per-terminal connection status, keyed by session.key.
  let termStatus = $state<Record<number, 'connecting' | 'connected' | 'closed'>>({})

  // True only while the app window is actually on screen AND focused. Tracked from
  // real focus/visibility events (a point-in-time hasFocus() can lag in WKWebView).
  // Drives suppression: if you're not seeing the app, you get the banner even when
  // the Claude session is the active tab.
  let appVisible = $state(true)

  // Claude notifications, per host. Status/badges drive UI; cursor is internal.
  let notifyStatus = $state<Record<string, NotifyStatus>>({})
  let notifyBadges = $state<Record<string, number>>({})
  let notifyDismissed = $state<Record<string, boolean>>({})
  let notifyInstalling = $state<Record<string, boolean>>({})
  let notifyError = $state<Record<string, string>>({})
  // Open notify WebSockets, keyed by host id. Core pushes Claude events over these;
  // onmessage fires even when the window is hidden (unlike a throttled poll timer).
  const notifyWs: Record<string, WebSocket> = {}

  const visible = $derived(servers.filter((s) => !s.hidden))
  const liveHostIds = $derived(new Set(sessions.map((s) => s.hostId)))
  // Hosts with a terminal that's actually connected → a live ControlMaster exists.
  // Notify status/polling is gated on this so it never opens a *second* concurrent
  // connection (which, on a cloudflared ProxyCommand host, gets closed as it loses
  // the ControlMaster race → "Connection closed by UNKNOWN port 65535").
  const connectedHostIds = $derived(
    new Set(
      sessions
        .filter((s) => s.kind !== 'browser' && termStatus[s.key] === 'connected')
        .map((s) => s.hostId),
    ),
  )
  // Most-recently-accessed first. `lastAccessed` is persisted server-side, so the
  // order survives restarts/reinstalls. Never-accessed hosts (null) keep their
  // original ssh-config order (stable sort, comparator returns 0).
  const byRecency = (a: Server, b: Server) => {
    const ta = a.lastAccessed
    const tb = b.lastAccessed
    if (ta == null && tb == null) return 0
    if (ta == null) return 1
    if (tb == null) return -1
    return tb - ta
  }
  // Live hosts in the order they went live — a newly-connected host is appended,
  // so connecting never reshuffles the hosts already up. Reconciled from the live
  // set: drop hosts that left, append hosts that just appeared (Set iterates in
  // session order).
  let liveOrder = $state<string[]>([])
  $effect(() => {
    const next = liveOrder.filter((id) => liveHostIds.has(id))
    for (const id of liveHostIds) if (!next.includes(id)) next.push(id)
    if (next.length !== liveOrder.length || next.some((id, i) => id !== liveOrder[i])) {
      liveOrder = next
    }
  })
  // Live hosts float to the top (in connection order); idle hosts below a separator.
  const liveList = $derived(
    liveOrder.map((id) => visible.find((s) => s.id === id)).filter((s): s is Server => !!s),
  )
  // Idle hosts: recently-used first (so a just-disconnected host stays near the top).
  const idleList = $derived(visible.filter((s) => !liveHostIds.has(s.id)).sort(byRecency))
  const sessionCounts = $derived(
    sessions.reduce<Record<string, number>>((m, s) => ((m[s.hostId] = (m[s.hostId] ?? 0) + 1), m), {}),
  )
  const activeHost = $derived(servers.find((s) => s.id === activeHostId))
  const activeState = $derived<Active | undefined>(activeHostId ? hostActive[activeHostId] : undefined)
  const hostSessions = $derived(sessions.filter((s) => s.hostId === activeHostId))
  // tmux session names currently open for the active host — feeds the sidebar
  // tree's "open here" state.
  const openTmuxNamesFor = (hostId: string) =>
    sessions
      .filter((s) => s.hostId === hostId && s.kind === 'tmux')
      .map((s) => s.session!)
      .filter(Boolean)
  // The tmux session showing right now (if a tmux tab is active) — highlighted in the tree.
  const activeTmuxName = $derived.by(() => {
    if (activeState?.type !== 'session') return undefined
    const s = sessions.find((x) => x.key === activeState.key)
    return s?.kind === 'tmux' ? s.session : undefined
  })
  // Terminals stay mounted across host switches; only the active one is shown.
  const terminalSessions = $derived(sessions.filter((s) => s.kind !== 'browser'))
  const isLive = $derived(!!activeHostId && liveHostIds.has(activeHostId))
  // Offer the setup card once a remote host is live, reachable, and lacks the hook
  // (until the user installs or dismisses it this session).
  const showNotifySetup = $derived(
    !!activeHost &&
      !activeHost.isLocal &&
      isLive &&
      notifyStatus[activeHost.id]?.reachable === true &&
      notifyStatus[activeHost.id]?.installed === false &&
      !notifyDismissed[activeHost.id],
  )

  async function load(fn: () => Promise<Server[]>) {
    try {
      servers = await fn()
      error = ''
    } catch (e) {
      error = String(e)
    }
  }

  async function refreshTunnels() {
    try {
      tunnels = await getTunnels()
    } catch {
      /* status bar is best-effort */
    }
  }

  // Desktop shell exposes a native in-tab browser; the plain-browser build falls
  // back to launching external Chrome.
  let embeddedBrowser = $state(false)

  onMount(() => {
    load(listServers)
    refreshTunnels()
    getFeatures().then((f) => (embeddedBrowser = f.embeddedBrowser)).catch(() => {})
    const t = setInterval(() => {
      refreshTunnels()
      // Learn install status for connected hosts (reuses the ControlMaster), then
      // reconcile the notify sockets. The sockets themselves push events; this timer
      // only opens/heals them, so background throttling can't drop notifications.
      for (const id of connectedHostIds) ensureNotifyStatus(id)
      reconcileNotifyWs()
    }, 4000)

    const updVisible = () => {
      appVisible = document.visibilityState === 'visible' && document.hasFocus()
      pushWatching() // report immediately on focus/blur (fires while JS is still live)
    }
    updVisible()
    window.addEventListener('focus', updVisible)
    window.addEventListener('blur', updVisible)
    document.addEventListener('visibilitychange', updVisible)

    // Fast heartbeat (< core's 5s TTL) so the watched-host report stays fresh
    // while the app is up; it naturally stops when the webview is suspended.
    const hb = setInterval(pushWatching, 2000)

    return () => {
      clearInterval(t)
      clearInterval(hb)
      window.removeEventListener('focus', updVisible)
      window.removeEventListener('blur', updVisible)
      document.removeEventListener('visibilitychange', updVisible)
      for (const id of Object.keys(notifyWs)) closeNotifyWs(id)
    }
  })

  // --- Claude notifications -------------------------------------------------

  async function ensureNotifyStatus(id: string) {
    if (id === '__local__' || notifyStatus[id]) return
    try {
      const st = await getNotifyStatus(id)
      notifyStatus = { ...notifyStatus, [id]: st }
      // Auto-update: the hook is wired but its script predates this app version
      // (e.g. lacks the pane-location column). Silently reinstall to refresh it —
      // install is idempotent, so this just rewrites the script and re-merges.
      if (st.installed && st.current === false) {
        installNotify(id)
          .then(() => (notifyStatus = { ...notifyStatus, [id]: { ...st, current: true } }))
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
  function reconcileNotifyWs() {
    const desired = new Set([...connectedHostIds].filter((id) => notifyStatus[id]?.installed))
    for (const id of desired) if (!notifyWs[id]) openNotifyWs(id)
    for (const id of Object.keys(notifyWs)) if (!desired.has(id)) closeNotifyWs(id)
  }

  function openNotifyWs(id: string) {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    const ws = new WebSocket(`${proto}://${location.host}/ws/notify?id=${encodeURIComponent(id)}`)
    notifyWs[id] = ws
    ws.onmessage = (e) => {
      try {
        fireNotify(id, JSON.parse(e.data) as NotifyEvent)
      } catch {
        /* ignore a malformed frame */
      }
    }
    ws.onclose = () => {
      if (notifyWs[id] === ws) delete notifyWs[id] // reconcile reopens if still wanted
    }
    ws.onerror = () => ws.close()
  }

  function closeNotifyWs(id: string) {
    const ws = notifyWs[id]
    delete notifyWs[id]
    ws?.close()
  }

  // The host whose live terminal is genuinely on screen: app focused AND visible
  // AND its active tab is a terminal (shell/tmux/docker) on that host. `null` for
  // any doubt (unfocused, hidden, minimized, another Space, a browser/dashboard
  // tab) — core then fires that host's banner.
  function watchedHost(): string | null {
    if (!appVisible || !activeHostId) return null
    if (activeState?.type !== 'session') return null
    const s = sessions.find((x) => x.key === activeState.key)
    return s && s.kind !== 'browser' ? activeHostId : null
  }

  // Heartbeat our watched host to core. When the app is backgrounded the webview
  // suspends and these stop, so core (never suspended) fires the banner itself.
  function pushWatching() {
    // Report which tmux sessions are open (as tabs) per host, so core scopes
    // Claude notifications to only the sessions the user has attached.
    const openTmux: Record<string, string[]> = {}
    for (const s of sessions) {
      if (s.kind === 'tmux' && s.session) (openTmux[s.hostId] ??= []).push(s.session)
    }
    setWatching(watchedHost(), openTmux).catch(() => {})
  }

  function fireNotify(id: string, _ev: NotifyEvent) {
    // Core raises the OS banner (works while backgrounded); the webview only
    // badges the sidebar, and only when you're not already looking at that host.
    if (activeHostId !== id) {
      notifyBadges = { ...notifyBadges, [id]: (notifyBadges[id] ?? 0) + 1 }
    }
  }

  async function installNotifyFor(id: string) {
    notifyInstalling = { ...notifyInstalling, [id]: true }
    notifyError = { ...notifyError, [id]: '' }
    try {
      await installNotify(id)
      notifyStatus = { ...notifyStatus, [id]: { reachable: true, installed: true } }
      reconcileNotifyWs() // open the stream now rather than waiting for the next tick
    } catch (e) {
      notifyError = { ...notifyError, [id]: String(e) }
    } finally {
      notifyInstalling = { ...notifyInstalling, [id]: false }
    }
  }

  async function uninstallNotifyFor(id: string) {
    if (!(await confirmDialog('Disable Claude notifications on this host? Removes the hook + helper script.', { okLabel: 'Disable', danger: true }))) return
    try {
      await uninstallNotify(id)
      notifyStatus = { ...notifyStatus, [id]: { reachable: true, installed: false } }
      notifyDismissed = { ...notifyDismissed, [id]: true } // don't immediately re-offer
    } catch (e) {
      notifyError = { ...notifyError, [id]: String(e) }
    }
  }

  function setActive(a: Active) {
    if (activeHostId) hostActive = { ...hostActive, [activeHostId]: a }
  }

  // Selecting a host connects it (opens a shell) on first visit; otherwise just shows it.
  function selectHost(s: Server) {
    activeHostId = s.id
    // Stamp recency (optimistic + persisted) so the order survives restarts.
    const now = Math.floor(Date.now() / 1000)
    servers = servers.map((x) => (x.id === s.id ? { ...x, lastAccessed: now } : x))
    touchServer(s.id).catch(() => {})
    // You're looking at this host now — clear its badge. (Notify status is fetched
    // by the poll loop once a terminal connects, so we don't open a second
    // connection here that would race the console on a ProxyCommand host.)
    if (notifyBadges[s.id]) notifyBadges = { ...notifyBadges, [s.id]: 0 }
    if (!sessions.some((x) => x.hostId === s.id)) {
      openShell(s.id)
    } else if (!hostActive[s.id]) {
      hostActive = { ...hostActive, [s.id]: { type: 'panel', panel: 'overview' } }
    }
  }

  function openShell(hostId: string) {
    const n = sessions.filter((s) => s.hostId === hostId && s.kind === 'shell').length
    const key = nextKey++
    sessions = [...sessions, { key, hostId, kind: 'shell', title: n === 0 ? 'shell' : `shell ${n + 1}` }]
    hostActive = { ...hostActive, [hostId]: { type: 'session', key } }
    addMenuOpen = false
  }

  // Attach a tmux session picked from the sidebar tree — opens (or re-focuses)
  // its persistent terminal tab.
  function selectTmux(hostId: string, session: string) {
    // Selecting from any connected host's tree focuses that host.
    activeHostId = hostId
    const existing = sessions.find(
      (s) => s.hostId === hostId && s.kind === 'tmux' && s.session === session,
    )
    if (existing) {
      setActive({ type: 'session', key: existing.key })
      return
    }
    const key = nextKey++
    sessions = [...sessions, { key, hostId, kind: 'tmux', session, title: session }]
    hostActive = { ...hostActive, [hostId]: { type: 'session', key } }
  }

  // Attach a specific Claude instance from the sidebar tree: focus its
  // window+pane on the host (so every client on the session follows), then open
  // or re-focus the session's terminal tab — landing right on that pane.
  async function attachClaude(hostId: string, session: string, inst: ClaudeInstance) {
    if (inst.window != null) {
      await tmuxSelect(hostId, { session, window: inst.window, paneId: inst.paneId }).catch(() => {})
    }
    selectTmux(hostId, session)
  }

  // Close (detach from) a tmux session's tab — the tmux session keeps running.
  function closeTmux(hostId: string, session: string) {
    const s = sessions.find((x) => x.hostId === hostId && x.kind === 'tmux' && x.session === session)
    if (s) closeSession(s.key)
  }

  function openDockerTerminal(kind: 'docker-logs' | 'docker-exec', cid: string, name: string) {
    if (!activeHostId) return
    const key = nextKey++
    const title = (kind === 'docker-logs' ? 'logs:' : 'sh:') + name
    sessions = [...sessions, { key, hostId: activeHostId, kind, cid, title }]
    hostActive = { ...hostActive, [activeHostId]: { type: 'session', key } }
  }

  function openBrowserTab(hostId: string) {
    const existing = sessions.find((s) => s.hostId === hostId && s.kind === 'browser')
    if (existing) {
      setActive({ type: 'session', key: existing.key })
      return
    }
    const key = nextKey++
    sessions = [...sessions, { key, hostId, kind: 'browser', title: 'Browser ↗' }]
    hostActive = { ...hostActive, [hostId]: { type: 'session', key } }
  }

  function closeSession(key: number, e?: MouseEvent) {
    e?.stopPropagation()
    const s = sessions.find((k) => k.key === key)
    if (!s) return
    sessions = sessions.filter((k) => k.key !== key)
    delete termStatus[key]
    const cur = hostActive[s.hostId]
    if (cur?.type === 'session' && cur.key === key) {
      const rest = sessions.filter((k) => k.hostId === s.hostId)
      hostActive = {
        ...hostActive,
        [s.hostId]: rest.length
          ? { type: 'session', key: rest[rest.length - 1].key }
          : { type: 'panel', panel: 'overview' },
      }
    }
  }

  async function disconnect() {
    const id = activeHostId
    if (!id) return
    sessions = sessions.filter((s) => s.hostId !== id)
    hostActive = { ...hostActive, [id]: { type: 'panel', panel: 'overview' } }
    // Tear down this host's tunnels too.
    stopProxy(id).catch(() => {})
    for (const f of tunnels.forwards.filter((f) => f.alias === id)) {
      unforwardPort(id, f.remotePort).catch(() => {})
    }
    setTimeout(refreshTunnels, 300)
  }

  function termProps(
    s: Session,
    local: boolean,
  ): { mode: string; alias?: string; cid?: string; session?: string } {
    if (s.kind === 'shell') {
      return local ? { mode: 'local' } : { mode: 'console', alias: s.hostId }
    }
    if (s.kind === 'tmux') {
      return { mode: 'tmux', alias: s.hostId, session: s.session }
    }
    return { mode: s.kind, alias: s.hostId, cid: s.cid }
  }

  // Sidebar / topbar host actions -------------------------------------------

  // Bumped per session to force a Terminal remount. The PTY receives the
  // password when ssh is spawned, so a session opened before one was stored sits
  // at its prompt forever — storing a password can only take effect on a fresh
  // connection. Cheap for tmux sessions: the remote tmux server keeps them
  // alive, so reattaching restores the panes.
  let termEpoch = $state<Record<number, number>>({})
  function reconnectHostTerminals(hostId: string) {
    for (const s of sessions) {
      if (s.hostId === hostId && s.kind !== 'browser') {
        termEpoch[s.key] = (termEpoch[s.key] ?? 0) + 1
      }
    }
  }

  async function managePassword() {
    const s = activeHost
    if (!s) return
    if (s.hasPassword) {
      if (await confirmDialog(`Clear stored password for ${s.name}?`, { okLabel: 'Clear', danger: true })) {
        await clearPassword(s.id)
        await load(listServers)
      }
      return
    }
    const pw = await promptDialog(`SSH password for ${s.name}`, { password: true })
    if (pw) {
      await setPassword(s.id, pw)
      await load(listServers)
      // Reconnect so the stored password is used straight away, instead of
      // leaving the user to type it into a shell already sitting at a prompt.
      reconnectHostTerminals(s.id)
    }
  }

  // Global tunnel kills (from Home / status bar) ----------------------------
  async function killForward(alias: string, remotePort: number) {
    await unforwardPort(alias, remotePort).catch(() => {})
    refreshTunnels()
  }
  async function killProxy(alias: string) {
    await stopProxy(alias).catch(() => {})
    refreshTunnels()
  }

  let addMenuOpen = $state(false)
  let menuPos = $state({ x: 0, y: 0 })
  let plusBtn: HTMLButtonElement | undefined

  // Sidebar collapse — ⌘B on macOS, Ctrl+B elsewhere (no Cmd key there).
  //
  // Ctrl+B is tmux's prefix, so on non-Mac the key is left alone while the
  // terminal has focus and forwarded to the remote tmux; intercepting it
  // globally would cost pane/window switching, which is the point of the app.
  // The shortcut still works anywhere else in the UI. macOS needs no such
  // carve-out, since tmux never sees Cmd.
  let sideCollapsed = $state(localStorage.getItem('sideCollapsed') === '1')
  function toggleSide() {
    sideCollapsed = !sideCollapsed
    localStorage.setItem('sideCollapsed', sideCollapsed ? '1' : '0')
  }
  function onGlobalKeydown(e: KeyboardEvent) {
    if (e.key !== 'b' || e.altKey) return
    const mod = isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey
    if (!mod) return
    // `.term` is our own container class, so this does not depend on xterm's
    // internal class names.
    if (!isMac && (e.target as HTMLElement | null)?.closest?.('.term')) return
    e.preventDefault()
    toggleSide()
  }

  function toggleAddMenu() {
    if (!addMenuOpen && plusBtn) {
      const r = plusBtn.getBoundingClientRect()
      menuPos = { x: r.left, y: r.bottom + 4 }
    }
    addMenuOpen = !addMenuOpen
  }

  function isActiveTerm(s: Session): boolean {
    return s.hostId === activeHostId && activeState?.type === 'session' && activeState.key === s.key
  }
</script>

<svelte:window onkeydowncapture={onGlobalKeydown} />

<div class="app" class:collapsed={sideCollapsed}>
  <!-- Sidebar -->
  <aside class="side">
    <div class="brand">
      <span class="mark"></span>
      <b>iShowManagement</b>
      <span class="spacer"></span>
      <button class="ico" title="Refresh from ~/.ssh/config" onclick={() => load(refreshServers)}>⟳</button>
    </div>

    {#if error}<div class="err">{error}</div>{/if}

    {#snippet serverRow(s: Server)}
      <li>
        <button class="srv" class:active={activeHostId === s.id} onclick={() => selectHost(s)}>
          <span class="r1">
            <span class="dot" class:on={liveHostIds.has(s.id)}></span>
            <span class="name">{s.name}</span>
            {#if s.hasPassword}<span class="lock" title="Password stored">🔒</span>{/if}
            {#if notifyBadges[s.id]}<span class="nbadge" title="Claude notifications">{notifyBadges[s.id]}</span>{/if}
            {#if sessionCounts[s.id]}<span class="count">{sessionCounts[s.id]}</span>{/if}
          </span>
        </button>
        {#if connectedHostIds.has(s.id)}
          <HostTmuxTree
            id={s.id}
            openNames={openTmuxNamesFor(s.id)}
            activeName={s.id === activeHostId ? activeTmuxName : undefined}
            onSelect={(name) => selectTmux(s.id, name)}
            onClose={(name) => closeTmux(s.id, name)}
            onAttachClaude={(name, inst) => attachClaude(s.id, name, inst)}
          />
        {/if}
      </li>
    {/snippet}

    <div class="glabel micro">Hosts</div>
    <ul class="servers">
      {#each liveList as s (s.id)}{@render serverRow(s)}{/each}
      {#if liveList.length && idleList.length}<li class="sep" aria-hidden="true"></li>{/if}
      {#each idleList as s (s.id)}{@render serverRow(s)}{/each}
    </ul>
  </aside>

  <!-- Main -->
  <section class="main">
    {#if activeHost}
      <div class="topbar">
        <div class="crumb">
          <span class="host">{activeHost.name}</span>
          <span class="addr mono">{activeHost.isLocal ? 'localhost' : `${activeHost.user ? activeHost.user + '@' : ''}${activeHost.host}${activeHost.port && activeHost.port !== 22 ? ':' + activeHost.port : ''}`}</span>
          <span class="pill" class:idle={!isLive}><span class="p"></span>{isLive ? 'connected' : 'idle'}</span>
        </div>
        <div class="top-actions">
          {#if !activeHost.isLocal}
            {@const on = notifyStatus[activeHost.id]?.installed === true}
            <button
              class="iconbtn bell"
              class:on
              title={on ? 'Claude notifications on — click to disable' : 'Enable Claude notifications'}
              aria-label="Claude notifications"
              onclick={() =>
                on
                  ? uninstallNotifyFor(activeHost.id)
                  : (notifyDismissed = { ...notifyDismissed, [activeHost.id]: false })}
            >
              <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
                <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
                <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
                {#if !on}<line x1="3" y1="3" x2="21" y2="21" stroke-width="1.4" />{/if}
              </svg>
              {#if on}<span class="on-dot" aria-hidden="true"></span>{/if}
            </button>
          {/if}
          <button class="btn" onclick={managePassword}>{activeHost.hasPassword ? 'Clear password' : 'Set password'}</button>
          {#if isLive}<button class="btn danger" onclick={disconnect}>Disconnect</button>{/if}
        </div>
      </div>

      <div class="tabbar">
        {#each PANELS as p}
          <button class="tab" class:active={activeState?.type === 'panel' && activeState.panel === p} onclick={() => setActive({ type: 'panel', panel: p })}>
            {p[0].toUpperCase() + p.slice(1)}
          </button>
        {/each}
        <span class="divider"></span>
        {#each hostSessions as s (s.key)}
          <button class="tab" class:active={activeState?.type === 'session' && activeState.key === s.key} class:browser={s.kind === 'browser'} onclick={() => setActive({ type: 'session', key: s.key })}>
            {#if s.kind !== 'browser' && termStatus[s.key]}<span class="tdot {termStatus[s.key]}"></span>{/if}
            {s.title}
            <span class="x" role="button" tabindex="0" title="Close" onclick={(e) => closeSession(s.key, e)} onkeydown={(e) => e.key === 'Enter' && closeSession(s.key)}>×</span>
          </button>
        {/each}
        <button class="tab plusbtn" bind:this={plusBtn} title="New session" onclick={toggleAddMenu}>＋</button>
      </div>

      {#if addMenuOpen}
        <div class="menu-backdrop" role="presentation" onclick={() => (addMenuOpen = false)}></div>
        <div class="menu" style:left="{menuPos.x}px" style:top="{menuPos.y}px">
          <button onclick={() => openShell(activeHost.id)}>New shell</button>
          {#if !activeHost.isLocal}<button onclick={() => { openBrowserTab(activeHost.id); addMenuOpen = false }}>Browser ↗</button>{/if}
        </div>
      {/if}
    {/if}

    <div class="stack">
      <!-- Persistent terminals: always mounted, shown only when active. -->
      {#each terminalSessions as s (s.key)}
        {@const p = termProps(s, servers.find((h) => h.id === s.hostId)?.isLocal ?? false)}
        <div class="layer term-layer" style:display={isActiveTerm(s) ? 'block' : 'none'}>
          <!-- Remounts on epoch bump, dropping the old socket and respawning ssh
               so a newly stored password takes effect. -->
          {#key termEpoch[s.key] ?? 0}
            <Terminal mode={p.mode} alias={p.alias} cid={p.cid} session={p.session} onStatus={(st) => (termStatus[s.key] = st)} />
          {/key}
        </div>
      {/each}

      {#if !activeHost}
        <div class="layer">
          <Home
            {servers}
            {tunnels}
            {sessionCounts}
            liveHostIds={liveHostIds}
            onConnect={selectHost}
            onKillForward={killForward}
            onKillProxy={killProxy}
          />
        </div>
      {:else if activeState?.type === 'panel'}
        <div class="layer">
          {#if activeState.panel === 'files'}
            <Files id={activeHost.id} />
          {:else}
            <Managers id={activeHost.id} view={activeState.panel} onTerminal={openDockerTerminal} onChanged={refreshTunnels} />
          {/if}
        </div>
      {:else if activeState?.type === 'session'}
        {@const cur = sessions.find((x) => x.key === activeState.key)}
        {#if cur?.kind === 'browser'}
          <div class="layer">
            <BrowserPanel id={activeHost.id} name={activeHost.name} embedded={embeddedBrowser} onChanged={refreshTunnels} />
          </div>
        {/if}
      {/if}

      {#if showNotifySetup && activeHost}
        <ClaudeNotifySetup
          hostName={activeHost.name}
          installing={!!notifyInstalling[activeHost.id]}
          error={notifyError[activeHost.id]}
          onInstall={() => installNotifyFor(activeHost.id)}
          onDismiss={() => (notifyDismissed = { ...notifyDismissed, [activeHost.id]: true })}
        />
      {/if}
    </div>
  </section>

  <StatusBar
    sessions={terminalSessions.length}
    forwards={tunnels.forwards.length}
    proxies={tunnels.proxies.length}
    active={activeHost ? { name: activeHost.name, live: isLive } : undefined}
    onShowTunnels={() => (activeHostId = undefined)}
    {sideCollapsed}
    onToggleSide={toggleSide}
  />
</div>

<Dialog />

<style>
  .app {
    display: grid;
    grid-template-columns: 264px 1fr;
    grid-template-rows: 1fr auto;
    height: 100vh;
  }
  .app.collapsed {
    grid-template-columns: 0 1fr;
  }
  /* Kept mounted (HostTmuxTree polls its own data); just not shown. */
  .app.collapsed .side {
    visibility: hidden;
    border-right: none;
  }
  /* Sidebar */
  .side {
    grid-row: 1 / 2;
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--line);
    overflow: hidden;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.65rem;
    padding: 1.15rem 1.15rem 1rem;
  }
  .mark {
    width: 22px;
    height: 22px;
    border: 1px solid var(--ink-faint);
    border-radius: 6px;
    display: grid;
    place-items: center;
    flex: none;
  }
  .mark::after {
    content: '';
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--ink);
  }
  .brand b {
    font-weight: 500;
    font-size: 14px;
    letter-spacing: -0.005em;
  }
  .spacer {
    margin-left: auto;
  }
  .ico {
    width: 26px;
    height: 26px;
    border: none;
    background: none;
    color: var(--ink-faint);
    cursor: pointer;
    border-radius: 6px;
    font-size: 15px;
  }
  .ico:hover {
    color: var(--ink);
    background: var(--surface-2);
  }
  .err {
    color: var(--danger);
    padding: 0.4rem 1.15rem;
    font-size: 12px;
  }
  .glabel {
    padding: 0.9rem 1.25rem 0.5rem;
  }
  .servers {
    list-style: none;
    margin: 0;
    padding: 0 0.6rem;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .srv {
    position: relative;
    display: block;
    width: 100%;
    text-align: left;
    cursor: pointer;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    padding: 0.6rem 0.6rem;
    border-radius: var(--radius);
    margin-bottom: 1px;
  }
  .srv:hover {
    background: var(--surface);
  }
  .srv.active {
    background: var(--surface-2);
  }
  .srv.active::before {
    content: '';
    position: absolute;
    left: 0;
    top: 10px;
    bottom: 10px;
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
  }
  .srv.dim {
    opacity: 0.5;
  }
  .r1 {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--ink-faint);
    flex: none;
  }
  .dot.on {
    background: var(--run);
  }
  .name {
    font-weight: 500;
    letter-spacing: -0.005em;
  }
  .srv.active .name {
    color: #e5e9f0;
  }
  .lock {
    font-size: 11px;
  }
  .count {
    margin-left: auto;
    font-size: 11px;
    color: var(--ink-faint);
    font-family: var(--font-mono);
  }
  .nbadge {
    margin-left: 0.2rem;
    min-width: 15px;
    height: 15px;
    padding: 0 4px;
    border-radius: 8px;
    background: var(--warn);
    color: #2e3440;
    font-size: 10px;
    font-weight: 700;
    font-family: var(--font-mono);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    line-height: 1;
    flex: none;
  }
  .sep {
    height: 1px;
    background: var(--line);
    margin: 0.5rem 0.6rem;
  }

  /* Main */
  .main {
    grid-row: 1 / 2;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .topbar {
    display: flex;
    align-items: center;
    gap: 0.9rem;
    padding: 0.9rem 1.35rem 0.8rem;
  }
  .crumb {
    display: flex;
    align-items: baseline;
    gap: 0.6rem;
    min-width: 0;
  }
  .crumb .host {
    font-weight: 500;
    font-size: 15px;
    letter-spacing: -0.01em;
  }
  .crumb .addr {
    color: var(--ink-faint);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    font-size: 11.5px;
    color: var(--ink-dim);
    flex: none;
  }
  .pill .p {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--run);
  }
  .pill.idle {
    color: var(--ink-faint);
  }
  .pill.idle .p {
    background: var(--ink-faint);
  }
  .top-actions {
    margin-left: auto;
    display: flex;
    gap: 0.4rem;
  }
  .btn {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 7px;
    padding: 0.4rem 0.75rem;
    cursor: pointer;
    font: inherit;
    font-size: 12.5px;
    font-weight: 500;
  }
  .btn:hover {
    color: var(--ink);
    border-color: var(--ink-faint);
  }
  .btn.danger:hover {
    color: var(--danger);
    border-color: rgba(191, 97, 106, 0.4);
  }
  .iconbtn {
    position: relative;
    width: 30px;
    height: 30px;
    display: inline-grid;
    place-items: center;
    border: 1px solid var(--line);
    border-radius: 7px;
    background: none;
    color: var(--ink-faint);
    cursor: pointer;
    transition: color 0.12s, border-color 0.12s;
  }
  /* Off: muted; hover invites turning it on. */
  .iconbtn:hover {
    color: var(--ink);
    border-color: var(--ink-faint);
  }
  /* On: accent with a live dot; hover signals disable. */
  .iconbtn.on {
    color: var(--accent);
  }
  .iconbtn.on:hover {
    color: var(--danger);
    border-color: rgba(191, 97, 106, 0.4);
  }
  .iconbtn .on-dot {
    position: absolute;
    top: 4px;
    right: 4px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--run);
    box-shadow: 0 0 0 2px var(--bg);
  }
  .iconbtn:hover .on-dot {
    background: var(--danger);
  }

  /* Segmented bar */
  .tabbar {
    display: flex;
    align-items: center;
    gap: 0.1rem;
    padding: 0 1.1rem;
    border-bottom: 1px solid var(--line);
    height: 42px;
    overflow-x: auto;
  }
  .tab {
    position: relative;
    padding: 0.5rem 0.7rem;
    cursor: pointer;
    color: var(--ink-faint);
    font-size: 12.5px;
    font-weight: 500;
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    height: 100%;
    border: none;
    background: none;
    border-bottom: 1.5px solid transparent;
    white-space: nowrap;
    font-family: inherit;
  }
  .tab:hover {
    color: var(--ink-dim);
  }
  .tab.active {
    color: var(--ink);
    border-bottom-color: var(--accent);
  }
  .tab.browser {
    color: var(--ink-dim);
  }
  .tab.browser.active {
    color: var(--ink);
  }
  .tab .x {
    color: var(--ink-faint);
    font-size: 13px;
    opacity: 0;
    transition: 0.12s;
    border-radius: 3px;
    padding: 0 0.15rem;
  }
  .tab:hover .x {
    opacity: 1;
  }
  .tab .x:hover {
    color: var(--danger);
  }
  .tdot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--ink-faint);
  }
  .tdot.connected {
    background: var(--run);
  }
  .tdot.connecting {
    background: var(--warn);
  }
  .tdot.closed {
    background: var(--danger);
  }
  .divider {
    width: 1px;
    height: 18px;
    background: var(--line);
    margin: 0 0.7rem;
    align-self: center;
    flex: none;
  }
  .plusbtn {
    color: var(--ink-faint);
    font-size: 15px;
    flex: none;
  }
  .plusbtn:hover {
    color: var(--ink);
  }
  .menu-backdrop {
    position: fixed;
    inset: 0;
    z-index: 40;
  }
  .menu {
    position: fixed;
    z-index: 41;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 4px;
    min-width: 140px;
    box-shadow: 0 12px 30px -12px #000;
  }
  .menu button {
    display: block;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    color: var(--ink-dim);
    font: inherit;
    font-size: 12.5px;
    padding: 0.4rem 0.6rem;
    border-radius: 6px;
    cursor: pointer;
  }
  .menu button:hover {
    background: var(--surface);
    color: var(--ink);
  }

  /* Content stack — layers share the same box; only the active one shows. */
  .stack {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  .layer {
    position: absolute;
    inset: 0;
    min-height: 0;
    overflow: hidden;
  }
  .term-layer {
    background: var(--bg);
  }
</style>
