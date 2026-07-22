<script lang="ts">
  import { slide } from 'svelte/transition'
  import { getTmux, getClaudeInventory, type TmuxSession, type ClaudeInstance } from './api'

  interface Props {
    id: string
    openNames: string[]
    activeName?: string
    onSelect: (name: string) => void
    onClose: (name: string) => void
    onAttachClaude: (session: string, inst: ClaudeInstance) => void
  }
  let { id, openNames, activeName, onSelect, onClose, onAttachClaude }: Props = $props()

  let sessions = $state<TmuxSession[]>([])
  let claudeBySession = $state<Record<string, ClaudeInstance[]>>({})
  let reason = $state('')
  let loading = $state(false)
  let expanded = $state(true)
  // Which sessions have their Claude children dropped down.
  let openSessions = $state<Set<string>>(new Set())

  async function reload() {
    loading = true
    reason = ''
    try {
      const [t, c] = await Promise.all([getTmux(id), getClaudeInventory(id).catch(() => null)])
      reason = t.available ? '' : t.reason ?? 'unavailable'
      sessions = t.sessions ?? []
      const map: Record<string, ClaudeInstance[]> = {}
      for (const s of c?.sessions ?? []) map[s.name] = s.claude
      claudeBySession = map
      // Auto-expand an *attached* session the moment one of its Claudes needs you.
      const attached = new Set((t.sessions ?? []).filter((s) => s.attached).map((s) => s.name))
      for (const [name, insts] of Object.entries(map)) {
        if (attached.has(name) && insts.some((i) => i.state === 'needs')) openSessions.add(name)
      }
      openSessions = new Set(openSessions)
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

  function toggleSession(name: string, e: Event) {
    e.stopPropagation()
    if (openSessions.has(name)) openSessions.delete(name)
    else openSessions.add(name)
    openSessions = new Set(openSessions)
  }

  // Worst Claude state in a session, for the collapsed-row rollup marker.
  function rollup(insts: ClaudeInstance[] | undefined): 'needs' | 'done' | 'active' | undefined {
    if (!insts?.length) return undefined
    if (insts.some((i) => i.state === 'needs')) return 'needs'
    if (insts.some((i) => i.state === 'done')) return 'done'
    return 'active'
  }

  const badgeLabel = (s: ClaudeInstance['state']) =>
    s === 'needs' ? 'needs you' : s === 'done' ? 'done' : 'active'

  $effect(() => {
    id
    reload()
  })

  // Refresh the Claude picture periodically while connected — cheap, and keeps
  // states live without threading the notify WebSocket through this component.
  $effect(() => {
    if (!expanded) return
    const t = setInterval(reload, 6000)
    return () => clearInterval(t)
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
          <!-- Only surface Claude for attached tmux sessions; a detached session
               (nobody in it) stays a plain leaf with no rollup/dropdown. -->
          {@const claude = s.attached ? claudeBySession[s.name] : undefined}
          {@const roll = rollup(claude)}
          {@const dropped = openSessions.has(s.name)}
          <div class="snode" style="--i:{i}">
            <button
              class="leaf"
              class:active={activeName === s.name}
              class:open={isOpen}
              onclick={() => onSelect(s.name)}
            >
              {#if claude?.length}
                <span
                  class="scaret"
                  class:open={dropped}
                  role="button"
                  tabindex="0"
                  title={dropped ? 'Collapse' : 'Show Claude sessions'}
                  onclick={(e) => toggleSession(s.name, e)}
                  onkeydown={(e) => e.key === 'Enter' && toggleSession(s.name, e)}
                >▸</span>
              {:else}
                <span class="scaret-spacer"></span>
              {/if}
              <span
                class="lg"
                class:on={isOpen}
                class:att={!isOpen && s.attached}
                title={isOpen ? 'Open here' : s.attached ? 'Attached elsewhere' : 'Detached'}
              ></span>
              <span class="lname mono">{s.name}</span>
              {#if claude?.length}
                <span class="rollup" title="Claude sessions in this tmux session">
                  <span class="cmk {roll}"></span>{claude.length}
                </span>
              {/if}
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

            {#if claude?.length && dropped}
              <div class="kids" transition:slide={{ duration: 140 }}>
                {#each claude as inst (inst.paneId ?? `${inst.window}.${inst.pane}`)}
                  <button class="cnode" title="Attach to this pane" onclick={() => onAttachClaude(s.name, inst)}>
                    <span class="cst {inst.state}"></span>
                    <span class="cbody">
                      <span class="cl1">
                        <span class="cwhere mono"
                          >{inst.window != null ? `win ${inst.window}` : 'claude'}{inst.windowName
                            ? ` · ${inst.windowName}`
                            : ''}{inst.paneId ? ` · ${inst.paneId}` : ''}</span
                        >
                        <span class="cbadge {inst.state}">{badgeLabel(inst.state)}</span>
                      </span>
                      {#if inst.summary || inst.message}
                        <span class="csum">{inst.message ?? inst.summary}</span>
                      {/if}
                      {#if inst.project}<span class="cmeta mono">{inst.project}</span>{/if}
                    </span>
                  </button>
                {/each}
              </div>
            {/if}
          </div>
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
  .snode {
    animation: leafin 0.18s ease both;
    animation-delay: calc(var(--i) * 22ms);
  }
  .leaf {
    position: relative;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    padding: 0.32rem 0.4rem 0.32rem 0.3rem;
    border-radius: 6px;
    cursor: pointer;
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
  /* Disclosure caret for sessions that hold Claude instances. */
  .scaret {
    width: 12px;
    flex: none;
    text-align: center;
    font-size: 8px;
    color: var(--ink-faint);
    transition: transform 0.15s ease;
    border-radius: 3px;
  }
  .scaret.open {
    transform: rotate(90deg);
  }
  .scaret:hover {
    color: var(--ink);
  }
  .scaret-spacer {
    width: 12px;
    flex: none;
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
  /* Rollup: how many Claude instances + their worst state, on a collapsed row. */
  .rollup {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    flex: none;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--ink-faint);
  }
  .cmk {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: none;
  }
  .cmk.needs {
    background: var(--warn);
    box-shadow: 0 0 0 2.5px rgba(214, 179, 106, 0.16);
  }
  .cmk.done {
    background: var(--accent);
  }
  .cmk.active {
    background: var(--run);
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

  /* Claude children — nested under a session, their own hairline spine. */
  .kids {
    margin: 0.1rem 0 0.35rem 1rem;
    padding-left: 0.55rem;
    border-left: 1px solid var(--line-2);
  }
  .cnode {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    padding: 0.4rem 0.45rem;
    border-radius: 7px;
    cursor: pointer;
    margin-bottom: 1px;
  }
  .cnode:hover {
    background: var(--surface);
  }
  .cst {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex: none;
    margin-top: 4px;
  }
  .cst.needs {
    background: var(--warn);
    box-shadow: 0 0 0 3px rgba(214, 179, 106, 0.14);
  }
  .cst.done {
    background: var(--accent);
  }
  .cst.active {
    background: var(--run);
    box-shadow: 0 0 0 3px rgba(119, 192, 145, 0.12);
  }
  .cbody {
    min-width: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
  }
  .cl1 {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .cwhere {
    font-size: 11.5px;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cbadge {
    font-size: 8.5px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    padding: 0.05rem 0.3rem;
    border-radius: 3px;
    flex: none;
  }
  .cbadge.needs {
    color: var(--warn);
    background: rgba(214, 179, 106, 0.14);
  }
  .cbadge.done {
    color: var(--accent);
    background: var(--accent-soft);
  }
  .cbadge.active {
    color: var(--run);
    background: rgba(119, 192, 145, 0.12);
  }
  .csum {
    font-size: 11px;
    color: var(--ink-dim);
    margin-top: 0.15rem;
    line-height: 1.35;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cmeta {
    font-size: 10px;
    color: var(--ink-faint);
    margin-top: 0.2rem;
  }
  .tnote {
    padding: 0.3rem 0.45rem;
    color: var(--ink-faint);
    font-size: 11px;
  }
</style>
