<script lang="ts">
  import { openBrowser, stopProxy } from './api'

  interface Props {
    id: string
    name: string
    onChanged: () => void
  }
  let { id, name, onChanged }: Props = $props()

  let busy = $state(false)
  let error = $state('')
  let socksPort = $state<number | undefined>()

  async function launch() {
    busy = true
    error = ''
    try {
      const r = await openBrowser(id)
      socksPort = r.socksPort
      onChanged()
    } catch (e) {
      error = String(e)
    } finally {
      busy = false
    }
  }

  async function stop() {
    busy = true
    try {
      await stopProxy(id)
      socksPort = undefined
      onChanged()
    } catch (e) {
      error = String(e)
    } finally {
      busy = false
    }
  }
</script>

<div class="bp">
  <div class="inner">
    <div class="icon">↗</div>
    <h2>Server-side browser</h2>
    <p class="lead">
      Launches an external Chrome window routed through a SOCKS proxy on
      <span class="mono">{name}</span>, so the browser sees the network as the server does —
      including the server's own <span class="mono">127.0.0.1</span>.
    </p>

    {#if socksPort}
      <div class="status">
        <span class="ok"></span> Proxy live · <span class="mono">socks5://127.0.0.1:{socksPort}</span>
      </div>
      <div class="actions">
        <button class="btn primary" onclick={launch} disabled={busy}>Open browser window</button>
        <button class="btn" onclick={stop} disabled={busy}>Stop proxy</button>
      </div>
    {:else}
      <div class="actions">
        <button class="btn primary" onclick={launch} disabled={busy}>{busy ? 'Starting…' : 'Launch browser'}</button>
      </div>
      <p class="hint">Tip: if it fails to authenticate, open a shell to this host first.</p>
    {/if}

    {#if error}<p class="err">{error}</p>{/if}
  </div>
</div>

<style>
  .bp {
    height: 100%;
    overflow: auto;
    display: grid;
    place-items: center;
    background: var(--bg);
  }
  .inner {
    max-width: 460px;
    padding: 2rem;
    text-align: center;
  }
  .icon {
    width: 46px;
    height: 46px;
    margin: 0 auto 1rem;
    border: 1px solid var(--line);
    border-radius: 12px;
    display: grid;
    place-items: center;
    font-size: 20px;
    color: var(--accent);
  }
  h2 {
    margin: 0 0 0.6rem;
    font-size: 17px;
    font-weight: 500;
  }
  .lead {
    color: var(--ink-dim);
    font-size: 13px;
    line-height: 1.6;
    margin: 0 0 1.4rem;
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 12.5px;
    color: var(--ink-dim);
    margin-bottom: 1.2rem;
  }
  .ok {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--run);
  }
  .actions {
    display: flex;
    gap: 0.5rem;
    justify-content: center;
  }
  .btn {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 8px;
    padding: 0.5rem 0.9rem;
    cursor: pointer;
    font: inherit;
    font-size: 12.5px;
    font-weight: 500;
  }
  .btn:hover:not(:disabled) {
    color: var(--ink);
    border-color: var(--ink-faint);
  }
  .btn.primary {
    background: var(--accent-soft);
    border-color: rgba(136, 192, 208, 0.35);
    color: var(--ink);
  }
  .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .hint {
    color: var(--ink-faint);
    font-size: 11.5px;
    margin: 1rem 0 0;
  }
  .err {
    color: var(--danger);
    font-size: 12px;
    margin-top: 1rem;
  }
</style>
