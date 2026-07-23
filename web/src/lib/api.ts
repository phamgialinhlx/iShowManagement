// Typed client for the iShowManagement REST API.

export interface Server {
  id: string
  name: string
  host: string
  user: string
  port: number
  proxyJump: string | null
  hidden: boolean
  hasPassword: boolean
  socksIndex: number
  isLocal: boolean
  lastAccessed: number | null
}

function ok(r: Response): Response {
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`)
  return r
}

const json = { 'content-type': 'application/json' }
const enc = encodeURIComponent

export const listServers = (): Promise<Server[]> =>
  fetch('/api/servers').then(ok).then((r) => r.json())

export const refreshServers = (): Promise<Server[]> =>
  fetch('/api/servers/refresh', { method: 'POST' }).then(ok).then((r) => r.json())

export const setHidden = (id: string, hidden: boolean): Promise<Response> =>
  fetch(`/api/servers/${enc(id)}/hidden`, {
    method: 'POST',
    headers: json,
    body: JSON.stringify({ hidden }),
  }).then(ok)

export const setPassword = (id: string, password: string): Promise<Response> =>
  fetch(`/api/servers/${enc(id)}/password`, {
    method: 'PUT',
    headers: json,
    body: JSON.stringify({ password }),
  }).then(ok)

export const clearPassword = (id: string): Promise<Response> =>
  fetch(`/api/servers/${enc(id)}/password`, { method: 'DELETE' }).then(ok)

export const touchServer = (id: string): Promise<Response> =>
  fetch(`/api/servers/${enc(id)}/touch`, { method: 'POST' }).then(ok)

// -- Managers -------------------------------------------------------------

export interface Overview {
  host: string
  os: string
  uptime: string
  load: string
  mem: { total: number; available: number } | null
  disk: { total: number; available: number } | null
}
export interface Container {
  id: string
  name: string
  image: string
  state: string
  status: string
  ports: string
}
export interface Stat {
  name: string
  cpu: string
  mem: string
}
export interface PortRow {
  proto: string
  addr: string
  port: number
  pid: number | null
  process: string | null
  forwardedTo?: number
}
export interface Proc {
  pid: number
  user: string
  cpu: number
  mem: number
  time: string
  cmd: string
}

const base = (id: string) => `/api/servers/${enc(id)}`

export const getOverview = (id: string): Promise<Overview> =>
  fetch(`${base(id)}/overview`).then(ok).then((r) => r.json())

export const getDocker = (
  id: string,
): Promise<{ available: boolean; reason?: string; containers?: Container[] }> =>
  fetch(`${base(id)}/docker`).then(ok).then((r) => r.json())

export const getDockerStats = (id: string): Promise<{ stats: Stat[] }> =>
  fetch(`${base(id)}/docker/stats`).then(ok).then((r) => r.json())

export interface TmuxSession {
  name: string
  windows: number
  attached: boolean
  created: string
}

export const getTmux = (
  id: string,
): Promise<{ available: boolean; reason?: string; sessions?: TmuxSession[] }> =>
  fetch(`${base(id)}/tmux`).then(ok).then((r) => r.json())

// A Claude instance living in a specific tmux pane, from the notify log
// (see notify::claude_inventory). `state` is derived from the last hook event.
export interface ClaudeInstance {
  paneId?: string
  window?: number
  windowName?: string
  pane?: number
  // State = kind of the most recent hook event:
  //   'working' = generating now (last event a UserPromptSubmit)
  //   'needs'   = blocked on a permission prompt
  //   'done'    = finished its turn (a Stop or idle prompt)
  //   'unknown' = process alive but no events (e.g. started before the hook)
  state: 'working' | 'needs' | 'done' | 'unknown'
  kind?: string
  notificationType?: string
  message?: string
  summary?: string
  project?: string
  // Context-window tokens on the last turn (input + cache). Absent when the
  // pane was found by command scan only (no hook event to read the transcript).
  contextTokens?: number
}
export interface ClaudeSession {
  name: string
  claude: ClaudeInstance[]
}

export const getClaudeInventory = (
  id: string,
): Promise<{ available: boolean; reason?: string; sessions?: ClaudeSession[] }> =>
  fetch(`${base(id)}/tmux/claude`).then(ok).then((r) => r.json())

// Focus a window (+ pane) in a session before attaching — lands the terminal on
// a specific Claude instance. Backend validates the pane id (`%<n>`).
export const tmuxSelect = (
  id: string,
  sel: { session: string; window: number; paneId?: string },
): Promise<Response> =>
  fetch(`${base(id)}/tmux/select`, {
    method: 'POST',
    headers: json,
    body: JSON.stringify({ session: sel.session, window: sel.window, pane_id: sel.paneId ?? null }),
  }).then(ok)

export const dockerAction = (
  id: string,
  cid: string,
  action: 'start' | 'stop' | 'restart' | 'rm',
): Promise<Response> => fetch(`${base(id)}/docker/${enc(cid)}/${action}`, { method: 'POST' }).then(ok)

export const getPorts = (
  id: string,
): Promise<{ available: boolean; reason?: string; ports?: PortRow[] }> =>
  fetch(`${base(id)}/ports`).then(ok).then((r) => r.json())

export const getProcesses = (id: string): Promise<{ processes: Proc[] }> =>
  fetch(`${base(id)}/processes`).then(ok).then((r) => r.json())

export const killPid = (id: string, pid: number, force = false): Promise<Response> =>
  fetch(`${base(id)}/kill`, {
    method: 'POST',
    headers: json,
    body: JSON.stringify({ pid, force }),
  }).then(ok)

// -- Files ----------------------------------------------------------------

export interface FileEntry {
  name: string
  path: string
  type: 'dir' | 'file' | 'link'
  size: number
  mtime: number
}
export interface Listing {
  path: string
  parent: string | null
  entries: FileEntry[]
}
export interface FileView {
  type: 'text' | 'image' | 'too_large' | 'unsupported'
  name: string
  path: string
  size: number
  mime: string
  text?: string
  dataUrl?: string
  limit?: number
  editable?: boolean
}

export const listFiles = (id: string, path = ''): Promise<Listing> =>
  fetch(`${base(id)}/files?path=${enc(path)}`).then(ok).then((r) => r.json())

export const viewFile = (id: string, path: string): Promise<FileView> =>
  fetch(`${base(id)}/files/view?path=${enc(path)}`).then(ok).then((r) => r.json())

export const downloadFileUrl = (id: string, path: string): string =>
  `${base(id)}/files/download?path=${enc(path)}`

export const saveFile = (id: string, path: string, content: string): Promise<Response> =>
  fetch(`${base(id)}/files/save?path=${enc(path)}`, {
    method: 'POST',
    headers: { 'content-type': 'text/plain; charset=utf-8' },
    body: content,
  }).then(ok)

// -- Forward + Browser ----------------------------------------------------

export const forwardPort = (id: string, port: number): Promise<{ ok: boolean; localPort: number }> =>
  fetch(`${base(id)}/ports/${port}/forward`, { method: 'POST' }).then(ok).then((r) => r.json())

export const unforwardPort = (id: string, port: number): Promise<Response> =>
  fetch(`${base(id)}/ports/${port}/forward`, { method: 'DELETE' }).then(ok)

export const openBrowser = (
  id: string,
  url?: string,
): Promise<{ socksPort: number; launched: boolean }> =>
  fetch(`${base(id)}/browser`, { method: 'POST', headers: json, body: JSON.stringify({ url }) })
    .then(ok)
    .then((r) => r.json())

export const stopProxy = (id: string): Promise<Response> =>
  fetch(`${base(id)}/proxy`, { method: 'DELETE' }).then(ok)

// -- Tunnels (global) -----------------------------------------------------

export interface Forward {
  alias: string
  remotePort: number
  localPort: number
}
export interface Proxy {
  alias: string
  port: number
}
export interface Tunnels {
  forwards: Forward[]
  proxies: Proxy[]
}

export const getTunnels = (): Promise<Tunnels> =>
  fetch('/api/tunnels').then(ok).then((r) => r.json())

// -- Claude notifications -------------------------------------------------

export const notify = (title: string, body: string, subtitle?: string): Promise<Response> =>
  fetch('/api/notify', {
    method: 'POST',
    headers: json,
    body: JSON.stringify({ title, body, subtitle }),
  }).then(ok)

// Heartbeat telling core which host's terminal is on screen (null when
// backgrounded / not a terminal). Core fires Claude banners itself and uses this
// to suppress the one you're watching — so delivery survives webview suspension.
export const setWatching = (
  activeHost: string | null,
  openTmux: Record<string, string[]> = {},
): Promise<Response> =>
  fetch('/api/watching', {
    method: 'POST',
    headers: json,
    body: JSON.stringify({ activeHost, openTmux }),
  }).then(ok)

export interface NotifyStatus {
  reachable: boolean
  installed: boolean
  /// False when the installed helper script predates the app's current version —
  /// the app then silently reinstalls to update it.
  current?: boolean
  reason?: string
}
export interface NotifyEvent {
  kind: 'stop' | 'notification'
  tmux?: string
  notificationType?: string
  message?: string
  project?: string
  summary?: string
}

export const getNotifyStatus = (id: string): Promise<NotifyStatus> =>
  fetch(`${base(id)}/claude-notify`).then(ok).then((r) => r.json())

export const installNotify = (id: string): Promise<Response> =>
  fetch(`${base(id)}/claude-notify`, { method: 'POST' }).then(ok)

export const uninstallNotify = (id: string): Promise<Response> =>
  fetch(`${base(id)}/claude-notify`, { method: 'DELETE' }).then(ok)

// Live events arrive over the /ws/notify WebSocket (see App.svelte), not this
// HTTP endpoint — core pushes them so delivery survives window-hidden throttling.
