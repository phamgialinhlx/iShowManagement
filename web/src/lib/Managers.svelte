<script lang="ts">
  import {
    getOverview,
    getDocker,
    getDockerStats,
    dockerAction,
    getPorts,
    getProcesses,
    killPid,
    forwardPort,
    unforwardPort,
    localPortFree,
    getTunnels,
    type Overview,
    type Container,
    type Stat,
    type PortRow,
    type Proc,
    type Forward,
  } from './api'
  import { confirmDialog } from './dialogs.svelte'

  interface Props {
    id: string
    view: 'overview' | 'docker' | 'ports' | 'processes'
    onTerminal: (kind: 'docker-logs' | 'docker-exec', cid: string, name: string) => void
    onChanged?: () => void
  }
  let { id, view, onTerminal, onChanged }: Props = $props()

  const isLocal = $derived(id === '__local__')

  let loading = $state(false)
  let error = $state('')
  let overview = $state<Overview>()
  let containers = $state<Container[]>([])
  let stats = $state<Record<string, Stat>>({})
  let dockerReason = $state('')
  let ports = $state<PortRow[]>([])
  let portsReason = $state('')
  let procs = $state<Proc[]>([])

  // Manual-forward form (Ports tab). Ports are number|null so an empty field is null.
  let fwds = $state<Forward[]>([]) // this server's active forwards
  let fRemote = $state<number | null>(null)
  let fLocal = $state<number | null>(null)
  let fTarget = $state('127.0.0.1')
  let fBusy = $state(false)
  let fErr = $state('')
  let localEdited = $state(false) // user took over the local field → stop auto-fill
  let localHint = $state('')
  let suggestTimer: ReturnType<typeof setTimeout> | undefined

  const validPort = (n: number | null): n is number => n !== null && Number.isInteger(n) && n >= 1 && n <= 65535
  const formValid = $derived(validPort(fRemote) && validPort(fLocal) && fTarget.trim().length > 0)

  function fmtBytes(n: number): string {
    if (!n) return '—'
    const u = ['B', 'K', 'M', 'G', 'T']
    let i = 0
    while (n >= 1024 && i < u.length - 1) {
      n /= 1024
      i++
    }
    return `${n.toFixed(1)}${u[i]}`
  }

  async function reload() {
    loading = true
    error = ''
    try {
      if (view === 'overview') {
        overview = await getOverview(id)
      } else if (view === 'docker') {
        const d = await getDocker(id)
        dockerReason = d.available ? '' : d.reason ?? 'unavailable'
        containers = d.containers ?? []
        if (d.available) {
          const s = await getDockerStats(id)
          stats = Object.fromEntries(s.stats.map((x) => [x.name, x]))
        }
      } else if (view === 'ports') {
        const p = await getPorts(id)
        portsReason = p.available ? '' : p.reason ?? 'unavailable'
        ports = p.ports ?? []
        fwds = isLocal ? [] : (await getTunnels()).forwards.filter((f) => f.alias === id)
      } else if (view === 'processes') {
        procs = (await getProcesses(id)).processes
      }
    } catch (e) {
      error = String(e)
    } finally {
      loading = false
    }
  }

  // Reload whenever the target server or the selected view changes.
  $effect(() => {
    id
    view
    reload()
  })

  async function doDocker(cid: string, action: 'start' | 'stop' | 'restart' | 'rm') {
    if (action === 'rm' && !(await confirmDialog(`Remove container ${cid}?`, { okLabel: 'Remove', danger: true }))) return
    try {
      await dockerAction(id, cid, action)
      await reload()
    } catch (e) {
      error = String(e)
    }
  }

  async function doKill(pid: number, force = false) {
    if (!(await confirmDialog(`Send ${force ? 'SIGKILL' : 'SIGTERM'} to pid ${pid}?`, { okLabel: 'Send', danger: true }))) return
    try {
      await killPid(id, pid, force)
      await reload()
    } catch (e) {
      error = String(e)
    }
  }

  async function doForward(port: number) {
    try {
      await forwardPort(id, port)
      await reload()
      onChanged?.()
    } catch (e) {
      error = String(e)
    }
  }

  async function doUnforward(port: number) {
    try {
      await unforwardPort(id, port)
      await reload()
      onChanged?.()
    } catch (e) {
      error = String(e)
    }
  }

  // Typing a remote port mirrors it to local, then a debounced probe bumps the
  // suggestion to an offset if that local port is busy. Backs off once the user
  // edits local themselves.
  function onRemoteInput() {
    fErr = ''
    localHint = ''
    clearTimeout(suggestTimer)
    if (!localEdited) fLocal = fRemote
    if (localEdited || !validPort(fRemote)) return
    const rp = fRemote
    suggestTimer = setTimeout(async () => {
      const free = await localPortFree(rp).catch(() => true)
      if (localEdited || fRemote !== rp) return // user changed things meanwhile
      if (!free) {
        fLocal = 20000 + (rp % 10000)
        localHint = `:${rp} busy — suggesting :${fLocal}`
      }
    }, 300)
  }

  function onLocalInput() {
    localEdited = true
    localHint = ''
    fErr = ''
  }

  async function doManualForward() {
    if (!formValid || fBusy) return
    fBusy = true
    fErr = ''
    try {
      await forwardPort(id, fRemote!, { local: fLocal!, target: fTarget.trim() })
      fRemote = null
      fLocal = null
      fTarget = '127.0.0.1'
      localEdited = false
      localHint = ''
      await reload()
      onChanged?.()
    } catch (e) {
      fErr = e instanceof Error ? e.message : String(e)
    } finally {
      fBusy = false
    }
  }
</script>

<div class="panel">
  <div class="toolbar">
    <button class="refresh" onclick={reload}>⟳ refresh</button>
    {#if loading}<span class="muted">loading…</span>{/if}
    {#if error}<span class="err">{error}</span>{/if}
  </div>

  {#if view === 'overview' && overview}
    <div class="kv">
      <div><span class="k">host</span>{overview.host || '—'}</div>
      <div><span class="k">os</span>{overview.os || '—'}</div>
      <div><span class="k">uptime</span>{overview.uptime || '—'}</div>
      <div><span class="k">load</span>{overview.load || '—'}</div>
      <div>
        <span class="k">memory</span>
        {overview.mem ? `${fmtBytes(overview.mem.total - overview.mem.available)} / ${fmtBytes(overview.mem.total)} used` : '—'}
      </div>
      <div>
        <span class="k">disk /</span>
        {overview.disk ? `${fmtBytes(overview.disk.total - overview.disk.available)} / ${fmtBytes(overview.disk.total)} used` : '—'}
      </div>
    </div>
  {/if}

  {#if view === 'docker'}
    {#if dockerReason}
      <div class="muted pad">{dockerReason}</div>
    {:else}
      <table>
        <thead><tr><th>name</th><th>image</th><th>state</th><th>cpu</th><th>mem</th><th>status</th><th></th></tr></thead>
        <tbody>
          {#each containers as c (c.id)}
            <tr>
              <td class="mono">{c.name}</td>
              <td class="muted">{c.image}</td>
              <td><span class="state {c.state}">{c.state}</span></td>
              <td>{stats[c.name]?.cpu ?? '—'}</td>
              <td class="muted">{stats[c.name]?.mem ?? '—'}</td>
              <td class="muted">{c.status}</td>
              <td class="row-actions">
                {#if c.state === 'running'}
                  <button onclick={() => doDocker(c.id, 'stop')}>stop</button>
                  <button onclick={() => doDocker(c.id, 'restart')}>restart</button>
                  <button onclick={() => onTerminal('docker-logs', c.id, c.name)}>logs</button>
                  <button onclick={() => onTerminal('docker-exec', c.id, c.name)}>shell</button>
                {:else}
                  <button onclick={() => doDocker(c.id, 'start')}>start</button>
                {/if}
                <button class="danger" onclick={() => doDocker(c.id, 'rm')}>rm</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}

  {#if view === 'ports'}
    {#if !isLocal}
      <div class="fwd-form">
        <div class="fwd-row">
          <span class="fwd-lbl">Forward</span>
          <input class="port" type="number" min="1" max="65535" placeholder="server port"
                 bind:value={fRemote} oninput={onRemoteInput} />
          <span class="sep mono">→ 127.0.0.1:</span>
          <input class="port" type="number" min="1" max="65535" placeholder="local"
                 bind:value={fLocal} oninput={onLocalInput} />
          <span class="sep">via</span>
          <input class="host mono" type="text" placeholder="127.0.0.1" bind:value={fTarget} />
          <button class="go" disabled={!formValid || fBusy} onclick={doManualForward}>
            {fBusy ? 'Connecting…' : 'Forward'}
          </button>
        </div>
        {#if localHint}<div class="hint">{localHint}</div>{/if}
        {#if fErr}<div class="hint err">{fErr}</div>{/if}
        {#if fwds.length}
          <div class="fwd-list">
            {#each fwds as f (f.remotePort)}
              <div class="fwd-item">
                <span class="mono">127.0.0.1:{f.localPort} → :{f.remotePort}</span>
                <button onclick={() => doUnforward(f.remotePort)}>unforward</button>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
    {#if portsReason}
      <div class="muted pad">{portsReason}</div>
    {:else}
      <table>
        <thead><tr><th>proto</th><th>address</th><th>port</th><th>pid</th><th>process</th><th></th></tr></thead>
        <tbody>
          {#each ports as p (p.proto + p.addr + p.port)}
            <tr>
              <td class="muted">{p.proto}</td>
              <td class="mono">{p.addr}</td>
              <td class="mono">{p.port}</td>
              <td class="muted">{p.pid ?? '—'}</td>
              <td>{p.process ?? '—'}</td>
              <td class="row-actions">
                {#if !isLocal}
                  {#if p.forwardedTo}
                    <span class="fwd">→ 127.0.0.1:{p.forwardedTo}</span>
                    <button onclick={() => doUnforward(p.port)}>unforward</button>
                  {:else}
                    <button onclick={() => doForward(p.port)}>forward</button>
                  {/if}
                {/if}
                {#if p.pid}<button class="danger" onclick={() => doKill(p.pid!)}>kill</button>{/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}

  {#if view === 'processes'}
    <table>
      <thead><tr><th>pid</th><th>user</th><th>cpu%</th><th>mem%</th><th>time</th><th>command</th><th></th></tr></thead>
      <tbody>
        {#each procs as p (p.pid)}
          <tr>
            <td class="mono">{p.pid}</td>
            <td class="muted">{p.user}</td>
            <td>{p.cpu}</td>
            <td class="muted">{p.mem}</td>
            <td class="muted">{p.time}</td>
            <td class="mono cmd">{p.cmd}</td>
            <td class="row-actions"><button class="danger" onclick={() => doKill(p.pid)}>kill</button></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .panel {
    height: 100%;
    overflow: auto;
    padding: 1.25rem 1.35rem;
    box-sizing: border-box;
    background: var(--bg);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 1rem;
  }
  .refresh {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 7px;
    padding: 0.3rem 0.7rem;
    cursor: pointer;
    font: inherit;
    font-size: 12.5px;
    font-weight: 500;
  }
  .refresh:hover { color: var(--ink); border-color: var(--ink-faint); }
  .muted { color: var(--ink-faint); }
  .err { color: var(--danger); }
  .pad { padding: 1rem 0; }
  .kv {
    display: grid;
    gap: 1px;
    max-width: 560px;
    background: var(--line);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .kv > div {
    display: flex;
    gap: 0.75rem;
    padding: 0.65rem 0.85rem;
    background: var(--bg);
  }
  .kv .k {
    color: var(--ink-faint);
    width: 90px;
    flex: none;
    font-size: 12px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12.5px;
  }
  th {
    text-align: left;
    color: var(--ink-faint);
    font-weight: 500;
    font-size: 10.5px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    padding: 0.55rem 0.6rem;
    border-bottom: 1px solid var(--line);
    position: sticky;
    top: 0;
    background: var(--bg);
  }
  td {
    padding: 0.6rem 0.6rem;
    border-bottom: 1px solid var(--line-2);
    vertical-align: middle;
  }
  tbody tr:hover { background: var(--surface); }
  .mono { font-family: var(--font-mono); color: var(--ink); }
  .cmd { max-width: 420px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fwd { color: var(--run); font-size: 11px; margin-right: 0.25rem; }
  .state.running { color: var(--run); }
  .state.exited, .state.created, .state.dead { color: var(--ink-faint); }
  .row-actions { white-space: nowrap; text-align: right; }
  .row-actions button {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 6px;
    padding: 0.18rem 0.5rem;
    margin-left: 0.3rem;
    cursor: pointer;
    font-size: 11.5px;
    font-family: inherit;
    font-weight: 500;
  }
  .row-actions button:hover { color: var(--ink); border-color: var(--ink-faint); }
  .row-actions .danger:hover { color: var(--danger); border-color: rgba(191,97,106,0.4); }

  .fwd-form {
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 0.7rem 0.8rem;
    margin-bottom: 1rem;
    background: var(--surface);
  }
  .fwd-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .fwd-lbl {
    color: var(--ink-faint);
    font-size: 10.5px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }
  .fwd-row .sep { color: var(--ink-faint); font-size: 12.5px; }
  .fwd-row input {
    background: var(--bg);
    border: 1px solid var(--line);
    color: var(--ink);
    border-radius: 6px;
    padding: 0.28rem 0.5rem;
    font: inherit;
    font-size: 12.5px;
  }
  .fwd-row input:focus { outline: none; border-color: var(--ink-faint); }
  .fwd-row .port { width: 6.5rem; }
  .fwd-row .host { width: 8rem; }
  .fwd-row .go {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 6px;
    padding: 0.28rem 0.7rem;
    cursor: pointer;
    font: inherit;
    font-size: 12px;
    font-weight: 500;
  }
  .fwd-row .go:hover:not(:disabled) { color: var(--ink); border-color: var(--ink-faint); }
  .fwd-row .go:disabled { opacity: 0.45; cursor: default; }
  .hint { font-size: 11.5px; color: var(--ink-faint); margin-top: 0.5rem; }
  .hint.err { color: var(--danger); }
  .fwd-list {
    margin-top: 0.6rem;
    padding-top: 0.6rem;
    border-top: 1px solid var(--line-2);
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .fwd-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    font-size: 12px;
  }
  .fwd-item span { color: var(--run); }
  .fwd-item button {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 6px;
    padding: 0.15rem 0.5rem;
    cursor: pointer;
    font: inherit;
    font-size: 11.5px;
    font-weight: 500;
  }
  .fwd-item button:hover { color: var(--ink); border-color: var(--ink-faint); }
</style>
