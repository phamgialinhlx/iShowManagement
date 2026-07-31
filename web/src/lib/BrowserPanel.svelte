<script lang="ts">
  import { onMount, onDestroy } from 'svelte'
  import { openBrowser, stopProxy, embedBrowser, browserControl, type Rect } from './api'

  interface Props {
    id: string
    name: string
    embedded: boolean
    onChanged: () => void
  }
  let { id, name, embedded, onChanged }: Props = $props()

  // -- External-Chrome fallback (plain-browser build) ---------------------
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

  // -- Embedded in-tab browser (desktop shell) ----------------------------
  let region = $state<HTMLDivElement>()
  let address = $state('')
  let embedError = $state('')
  let ro: ResizeObserver | undefined

  function rect(): Rect {
    const r = region!.getBoundingClientRect()
    return { x: r.left, y: r.top, w: r.width, h: r.height }
  }

  // Bare input → a URL. Loopback / private hosts default to http (that's where
  // `ssh -L` forwards land); everything else to https.
  function normalize(input: string): string {
    const s = input.trim()
    if (!s) return 'about:blank'
    if (/^[a-z][a-z0-9+.-]*:\/\//i.test(s)) return s
    if (/^(localhost|127\.|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(s)) return 'http://' + s
    return 'https://' + s
  }

  async function go() {
    const url = normalize(address)
    address = url === 'about:blank' ? '' : url
    try {
      await browserControl({ action: 'navigate', url })
    } catch (e) {
      embedError = String(e)
    }
  }

  const nav = (action: 'back' | 'forward' | 'reload') => browserControl({ action }).catch(() => {})

  // Coalesce resize bursts into one bounds update per frame.
  let raf = 0
  function syncBounds() {
    if (raf) return
    raf = requestAnimationFrame(() => {
      raf = 0
      if (region) browserControl({ action: 'bounds', rect: rect() }).catch(() => {})
    })
  }

  onMount(() => {
    if (!embedded || !region) return
    // Defer one frame so flex layout is settled before the first measurement.
    requestAnimationFrame(() => {
      if (!region) return
      embedBrowser(id, 'about:blank', rect())
        .then(() => onChanged())
        .catch((e) => (embedError = String(e)))
      ro = new ResizeObserver(syncBounds)
      ro.observe(region)
    })
  })

  onDestroy(() => {
    if (!embedded) return
    ro?.disconnect()
    if (raf) cancelAnimationFrame(raf)
    browserControl({ action: 'close' }).catch(() => {})
  })
</script>

{#if embedded}
  <div class="eb">
    <div class="bar">
      <button class="nav" title="Back" onclick={() => nav('back')}>‹</button>
      <button class="nav" title="Forward" onclick={() => nav('forward')}>›</button>
      <button class="nav" title="Reload" onclick={() => nav('reload')}>⟳</button>
      <form
        class="addr"
        onsubmit={(e) => {
          e.preventDefault()
          go()
        }}
      >
        <input
          type="text"
          spellcheck="false"
          autocapitalize="off"
          placeholder="Enter a URL — routed through {name}"
          bind:value={address}
        />
      </form>
    </div>
    {#if embedError}<div class="err">{embedError}</div>{/if}
    <!-- The native child webview is positioned over this region by the shell. -->
    <div class="region" bind:this={region}></div>
  </div>
{:else}
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
{/if}

<style>
  /* Embedded in-tab browser */
  .eb {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0.4rem 0.5rem;
    border-bottom: 1px solid var(--line);
    flex: 0 0 auto;
  }
  .nav {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 6px;
    width: 26px;
    height: 26px;
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
  }
  .nav:hover {
    color: var(--ink);
    border-color: var(--ink-faint);
  }
  .addr {
    flex: 1;
    display: flex;
  }
  .addr input {
    flex: 1;
    background: var(--bg-soft, rgba(127, 127, 127, 0.08));
    border: 1px solid var(--line);
    color: var(--ink);
    border-radius: 6px;
    padding: 0.35rem 0.6rem;
    font: inherit;
    font-size: 12.5px;
    outline: none;
  }
  .addr input:focus {
    border-color: var(--ink-faint);
  }
  .region {
    flex: 1;
    min-height: 0;
  }
  .eb .err {
    color: var(--danger);
    font-size: 12px;
    padding: 0.3rem 0.6rem;
  }

  /* External-Chrome fallback */
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
  .bp .err {
    color: var(--danger);
    font-size: 12px;
    margin-top: 1rem;
  }
</style>
