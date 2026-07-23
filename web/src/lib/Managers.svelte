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
    type Overview,
    type Container,
    type Stat,
    type PortRow,
    type Proc,
  } from './api'
  import { confirmDialog, alertDialog } from './dialogs.svelte'

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
      const r = await forwardPort(id, port)
      await reload()
      onChanged?.()
      await alertDialog(`Forwarded remote :${port} → http://127.0.0.1:${r.localPort}`)
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
</style>
