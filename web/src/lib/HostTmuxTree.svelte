<script lang="ts">
  import { slide } from 'svelte/transition'
  import { getTmux, type TmuxSession } from './api'

  interface Props {
    id: string
    openNames: string[]
    activeName?: string
    onSelect: (name: string) => void
    onClose: (name: string) => void
  }
  let { id, openNames, activeName, onSelect, onClose }: Props = $props()

  let sessions = $state<TmuxSession[]>([])
  let reason = $state('')
  let loading = $state(false)
  let expanded = $state(true)

  async function reload() {
    loading = true
    reason = ''
    try {
      const t = await getTmux(id)
      reason = t.available ? '' : t.reason ?? 'unavailable'
      sessions = t.sessions ?? []
    } catch (e) {
      reason = String(e)
    } finally {
      loading = false
    }
  }

  // Discovered sessions, plus any we hold open the server hasn't listed yet.
  const rows = $derived.by(() => {
    const known = new Set(sessions.map((s) => s.name))
    const extra = openNames
      .filter((n) => !known.has(n))
      .map((name) => ({ name, windows: 0, attached: true, created: '' }) as TmuxSession)
    return [...sessions, ...extra]
  })

  $effect(() => {
    id
    reload()
  })
</script>

<div class="tree">
  <div class="thead">
    <button class="disc" onclick={() => (expanded = !expanded)}>
      <span class="chev" class:open={expanded}>▸</span>
      <span class="tlabel">tmux</span>
      {#if rows.length}<span class="tcount">{rows.length}</span>{/if}
    </button>
    <button
      class="trefresh"
      class:spin={loading}
      title="Refresh sessions"
      onclick={reload}
      aria-label="Refresh tmux sessions">⟳</button>
  </div>

  {#if expanded}
    <div class="leaves" transition:slide={{ duration: 160 }}>
      {#if loading && !rows.length}
        <div class="tnote">scanning…</div>
      {:else if reason}
        <div class="tnote">{reason}</div>
      {:else if rows.length === 0}
        <div class="tnote">no sessions</div>
      {:else}
        {#each rows as s, i (s.name)}
          {@const isOpen = openNames.includes(s.name)}
          <button
            class="leaf"
            class:active={activeName === s.name}
            class:open={isOpen}
            style="--i:{i}"
            onclick={() => onSelect(s.name)}
          >
            <span
              class="lg"
              class:on={isOpen}
              class:att={!isOpen && s.attached}
              title={isOpen ? 'Open here' : s.attached ? 'Attached elsewhere' : 'Detached'}
            ></span>
            <span class="lname mono">{s.name}</span>
            {#if s.windows}<span class="lw" title="windows">{s.windows}</span>{/if}
            {#if isOpen}
              <span
                class="x"
                role="button"
                tabindex="0"
                title="Close (detach)"
                onclick={(e) => {
                  e.stopPropagation()
                  onClose(s.name)
                }}
                onkeydown={(e) => e.key === 'Enter' && onClose(s.name)}
              >×</span>
            {/if}
          </button>
        {/each}
      {/if}
    </div>
  {/if}
</div>

<style>
  .tree {
    margin: 1px 0 4px 1.05rem;
  }
  .thead {
    display: flex;
    align-items: center;
    height: 22px;
  }
  .disc {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--ink-faint);
    font: inherit;
    padding: 0 0.2rem;
  }
  .disc:hover {
    color: var(--ink-dim);
  }
  .chev {
    font-size: 8px;
    line-height: 1;
    transition: transform 0.15s ease;
    color: var(--ink-faint);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .tlabel {
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }
  .tcount {
    font-size: 10px;
    font-family: var(--font-mono);
    color: var(--ink-faint);
  }
  .trefresh {
    margin-left: auto;
    width: 20px;
    height: 20px;
    border: none;
    background: none;
    color: var(--ink-faint);
    cursor: pointer;
    border-radius: 5px;
    font-size: 11px;
    opacity: 0;
    transition: opacity 0.12s;
  }
  .tree:hover .trefresh {
    opacity: 1;
  }
  .trefresh:hover {
    color: var(--ink);
    background: var(--surface-2);
  }
  .trefresh.spin {
    opacity: 1;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  /* A hairline guide runs down the left of the leaf list — the "tree" spine. */
  .leaves {
    margin-left: 0.32rem;
    padding-left: 0.5rem;
    border-left: 1px solid var(--line);
  }
  .leaf {
    position: relative;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    padding: 0.32rem 0.4rem 0.32rem 0.45rem;
    border-radius: 6px;
    cursor: pointer;
    animation: leafin 0.18s ease both;
    animation-delay: calc(var(--i) * 22ms);
  }
  @keyframes leafin {
    from {
      opacity: 0;
      transform: translateX(-3px);
    }
  }
  .leaf:hover {
    background: var(--surface);
  }
  .leaf.active {
    background: var(--surface-2);
  }
  .leaf.active::before {
    content: '';
    position: absolute;
    left: -0.5rem;
    top: 6px;
    bottom: 6px;
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
  }
  .lg {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex: none;
    border: 1.5px solid var(--ink-faint);
    box-sizing: border-box;
  }
  .lg.on {
    background: var(--run);
    border-color: var(--run);
    box-shadow: 0 0 0 2.5px rgba(119, 192, 145, 0.14);
  }
  .lg.att {
    background: var(--warn);
    border-color: var(--warn);
  }
  .lname {
    flex: 1;
    font-size: 12px;
    color: var(--ink-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .leaf.open .lname {
    color: var(--ink);
  }
  .leaf.active .lname {
    color: #fff;
  }
  .lw {
    font-size: 10px;
    font-family: var(--font-mono);
    color: var(--ink-faint);
    flex: none;
  }
  .x {
    color: var(--ink-faint);
    font-size: 12px;
    opacity: 0;
    transition: 0.12s;
    padding: 0 0.1rem;
    border-radius: 3px;
    flex: none;
  }
  .leaf:hover .x {
    opacity: 1;
  }
  .x:hover {
    color: var(--danger);
  }
  .tnote {
    padding: 0.3rem 0.45rem;
    color: var(--ink-faint);
    font-size: 11px;
  }
</style>
