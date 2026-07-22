<script lang="ts">
  import type { Server, Tunnels } from './api'

  interface Props {
    servers: Server[]
    tunnels: Tunnels
    sessionCounts: Record<string, number>
    liveHostIds: Set<string>
    onConnect: (s: Server) => void
    onKillForward: (alias: string, remotePort: number) => void
    onKillProxy: (alias: string) => void
  }
  let { servers, tunnels, sessionCounts, liveHostIds, onConnect, onKillForward, onKillProxy }: Props = $props()

  const hour = new Date().getHours()
  const greeting = hour < 5 ? 'Still up.' : hour < 12 ? 'Good morning.' : hour < 18 ? 'Good afternoon.' : 'Good evening.'

  const visible = $derived(servers.filter((s) => !s.hidden))
  const liveCount = $derived(liveHostIds.size)
  const tunnelCount = $derived(tunnels.forwards.length + tunnels.proxies.length)
</script>

<div class="home">
  <h1>{greeting}</h1>
  <p class="sub">
    {visible.length} host{visible.length === 1 ? '' : 's'} in <span class="mono">~/.ssh/config</span>
    · {liveCount} live · {tunnelCount} active tunnel{tunnelCount === 1 ? '' : 's'}
  </p>

  <div class="micro lbl">Hosts</div>
  <div class="hgrid">
    {#each visible as s (s.id)}
      <button class="hcard" onclick={() => onConnect(s)}>
        <div class="r1">
          <span class="dot" class:on={liveHostIds.has(s.id)}></span>
          <span class="nm">{s.name}</span>
          {#if sessionCounts[s.id]}<span class="badge">{sessionCounts[s.id]} session{sessionCounts[s.id] === 1 ? '' : 's'}</span>{/if}
        </div>
        <div class="ad mono">{s.isLocal ? 'this machine' : `${s.user ? s.user + '@' : ''}${s.host}`}</div>
      </button>
    {/each}
  </div>

  {#if tunnelCount > 0}
    <div class="micro lbl tun-lbl">Active tunnels</div>
    {#each tunnels.forwards as f (f.alias + ':' + f.remotePort)}
      <div class="tun">
        <span class="mk fwd"></span>
        <span class="k mono">127.0.0.1:{f.localPort} → :{f.remotePort}</span>
        <span class="t">forward</span>
        <span class="host">· {f.alias}</span>
        <button class="kill" onclick={() => onKillForward(f.alias, f.remotePort)}>unforward</button>
      </div>
    {/each}
    {#each tunnels.proxies as p (p.alias)}
      <div class="tun">
        <span class="mk prox"></span>
        <span class="k mono">SOCKS5 127.0.0.1:{p.port}</span>
        <span class="t">proxy</span>
        <span class="host">· {p.alias}</span>
        <button class="kill" onclick={() => onKillProxy(p.alias)}>stop</button>
      </div>
    {/each}
  {/if}
</div>

<style>
  .home {
    height: 100%;
    overflow: auto;
    background: var(--bg);
  }
  h1 {
    max-width: 1000px;
    margin: 0 auto;
    padding: 3.5rem 2rem 0.3rem;
    font-size: 24px;
    font-weight: 300;
    letter-spacing: -0.02em;
  }
  .sub {
    max-width: 1000px;
    margin: 0 auto;
    padding: 0 2rem 2.4rem;
    color: var(--ink-faint);
    font-size: 13.5px;
  }
  .lbl {
    max-width: 1000px;
    margin: 0 auto 0.9rem;
    padding: 0 2rem;
  }
  .tun-lbl {
    margin-top: 2.6rem;
  }
  .hgrid {
    max-width: 1000px;
    margin: 0 auto;
    padding: 0 2rem;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
    gap: 1px;
    background: var(--line);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .hcard {
    background: var(--bg);
    padding: 1.1rem 1.15rem;
    cursor: pointer;
    border: none;
    text-align: left;
    color: inherit;
    font: inherit;
  }
  .hcard:hover {
    background: var(--surface);
  }
  .r1 {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    margin-bottom: 0.3rem;
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
  .nm {
    font-weight: 500;
  }
  .badge {
    margin-left: auto;
    font-size: 10.5px;
    color: var(--accent);
  }
  .ad {
    color: var(--ink-faint);
    font-size: 11.5px;
  }
  .tun {
    max-width: 1000px;
    margin: 0 auto 0.5rem;
    display: flex;
    align-items: center;
    gap: 0.8rem;
    padding: 0.7rem 0.9rem;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    font-size: 12.5px;
    width: calc(100% - 4rem);
  }
  .mk {
    width: 6px;
    height: 6px;
    flex: none;
  }
  .mk.fwd {
    border-radius: 2px;
    background: var(--accent);
  }
  .mk.prox {
    border-radius: 50%;
    border: 1px solid var(--ink-dim);
  }
  .k {
    color: var(--ink-dim);
  }
  .t {
    color: var(--ink-faint);
  }
  .host {
    color: var(--ink-dim);
  }
  .kill {
    margin-left: auto;
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 6px;
    padding: 0.18rem 0.5rem;
    font: inherit;
    font-size: 11.5px;
    cursor: pointer;
  }
  .kill:hover {
    color: var(--danger);
    border-color: rgba(211, 121, 111, 0.4);
  }
</style>
