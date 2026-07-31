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
  // Sessions already auto-expanded once, so a later manual collapse sticks across
  // the 6s refresh instead of springing back open.
  let autoExpanded = new Set<string>()

  async function reload() {
    loading = true
    reason = ''
    try {
      // Sequential, NOT Promise.all: the first ssh call establishes the shared
      // ControlMaster; firing both at once makes their dials race to create the
      // socket, and the loser fails — which is why the session leaves would show
      // (getTmux won) but the Claude inventory came back empty (it lost the race).
      const t = await getTmux(id)
      const c = await getClaudeInventory(id).catch(() => null)
      reason = t.available ? '' : t.reason ?? 'unavailable'
      sessions = t.sessions ?? []
      const map: Record<string, ClaudeInstance[]> = {}
      for (const s of c?.sessions ?? []) map[s.name] = s.claude
      claudeBySession = map
      // Auto-expand each opened session that has Claude — once — so its panes
      // show without hunting for the tiny caret. A later manual collapse sticks.
      for (const [name, insts] of Object.entries(map)) {
        if (openNames.includes(name) && insts.length && !autoExpanded.has(name)) {
          openSessions.add(name)
          autoExpanded.add(name)
        }
      }
      openSessions = new Set(openSessions)
      // Seed sessions we've never seen as "caught up", and keep the active tab
      // acknowledged so a Claude that finishes while you're watching it never
      // flips unread. Watermark is the host-clock statusUpdatedAt (skew-free).
      let dirty = false
      for (const [name, insts] of Object.entries(map)) {
        if (!(name in seen)) {
          seen[name] = watermark(insts)
          dirty = true
        }
      }
      if (activeName && map[activeName]) {
        const w = watermark(map[activeName])
        if ((seen[activeName] ?? -1) < w) {
          seen[activeName] = w
          dirty = true
        }
      }
      if (dirty) {
        seen = { ...seen }
        persistSeen()
      }
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

  // READ/UNREAD is attention state the app owns. To stay correct across host↔client
  // clock skew, the watermark is Claude's own `statusUpdatedAt` (host clock on both
  // sides): a session is "read" up to the newest status change we've acknowledged.
  // Seeded to the current watermark on first sight so startup isn't a wall of
  // unread, and persisted per host. WORKING/WAITING come straight from the status.
  let seen = $state<Record<string, number>>({})
  const viewKey = () => `ism:claudeSeen:${id}`
  function persistSeen() {
    try {
      localStorage.setItem(viewKey(), JSON.stringify(seen))
    } catch {}
  }
  const watermark = (insts: ClaudeInstance[]) =>
    insts.reduce((m, i) => Math.max(m, i.statusUpdatedAt ?? 0), 0)
  function markSeen(name: string) {
    const insts = claudeBySession[name]
    if (!insts) return
    const w = watermark(insts)
    if ((seen[name] ?? -1) < w) {
      seen[name] = w
      seen = { ...seen }
      persistSeen()
    }
  }
  function display(inst: ClaudeInstance, session: string): 'working' | 'waiting' | 'unread' | 'read' {
    if (inst.status === 'working') return 'working'
    if (inst.status === 'waiting') return 'waiting'
    return (inst.statusUpdatedAt ?? 0) > (seen[session] ?? 0) ? 'unread' : 'read'
  }

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

  // Load persisted read-state when the host changes.
  $effect(() => {
    try {
      seen = JSON.parse(localStorage.getItem(viewKey()) ?? '{}')
    } catch {
      seen = {}
    }
  })

  // The active session is on screen → acknowledge it the moment it's switched to.
  $effect(() => {
    if (activeName) markSeen(activeName)
  })
</script>

{#snippet statusIcon(d: 'working' | 'waiting' | 'unread' | 'read')}
  {#if d === 'working'}
    <!-- lucide: loader -->
    <svg class="ci working" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M12 2v4" /><path d="m16.2 7.8 2.9-2.9" /><path d="M18 12h4" /><path d="m16.2 16.2 2.9 2.9" /><path d="M12 18v4" /><path d="m4.9 19.1 2.9-2.9" /><path d="M2 12h4" /><path d="m4.9 4.9 2.9 2.9" />
    </svg>
  {:else if d === 'waiting'}
    <!-- lucide: circle-alert -->
    <svg class="ci waiting" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <circle cx="12" cy="12" r="10" /><line x1="12" x2="12" y1="8" y2="12" /><line x1="12" x2="12.01" y1="16" y2="16" />
    </svg>
  {:else if d === 'unread'}
    <!-- lucide: circle-check -->
    <svg class="ci unread" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <circle cx="12" cy="12" r="10" /><path d="m9 12 2 2 4-4" />
    </svg>
  {:else}
    <!-- lucide: circle-check, negative (filled disc, knocked-out check) -->
    <svg class="ci read" viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true">
      <circle cx="12" cy="12" r="10" /><path d="m9 12 2 2 4-4" fill="none" stroke="var(--bg)" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  {/if}
{/snippet}

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
          <!-- Only surface Claude for sessions the user has opened here. On a
               shared host, someone else's session (attached by them, not opened
               in this app) stays a plain leaf with no rollup/dropdown. -->
          {@const claude = isOpen ? claudeBySession[s.name] : undefined}
          {@const dropped = openSessions.has(s.name)}
          <div class="snode" style="--i:{i}">
            <!-- A div, not a <button>: WebKit/WKWebView routes clicks on
                 interactive descendants of a real <button> to the button itself,
                 so a nested caret/close would fire onSelect instead of its own
                 handler (the bug where clicking the caret attached pham instead
                 of expanding it). role+tabindex+keydown keep it accessible. -->
            <div
              class="leaf"
              role="button"
              tabindex="0"
              class:active={activeName === s.name}
              class:open={isOpen}
              onclick={() => onSelect(s.name)}
              onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && onSelect(s.name)}
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
            </div>

            {#if claude?.length && dropped}
              <div class="kids" transition:slide={{ duration: 140 }}>
                {#each claude as inst (inst.paneId ?? `${inst.window}.${inst.pane}`)}
                  {@const d = display(inst, s.name)}
                  <button
                    class="cnode"
                    title={`${d} · pane ${inst.paneId ?? ''} (win ${inst.window ?? 0}·pane ${inst.pane ?? 0})`}
                    onclick={() => onAttachClaude(s.name, inst)}
                  >
                    <span class="cbody">
                      <span class="cl1">
                        <span class="cdir">{inst.project ?? 'claude'}</span>
                      </span>
                      <span class="cloc mono">
                        <span>win {inst.window ?? 0} · pane {inst.pane ?? 0}</span>
                        {#if d === 'waiting' && inst.waitingFor}
                          <span class="cwait">{inst.waitingFor}</span>
                        {/if}
                      </span>
                    </span>
                    {@render statusIcon(d)}
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
    box-shadow: 0 0 0 2.5px rgba(163, 190, 140, 0.14);
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
    color: #e5e9f0;
  }
  /* Rollup: how many Claude instances + their worst state, on a collapsed row. */
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
    align-items: center;
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
  .cbody {
    min-width: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
  }
  .cl1 {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  /* Status icon — the at-a-glance state. `working` spins (lucide loader); `read`
     is a quiet filled check; the rest are outlined and coloured. */
  .ci {
    width: 14px;
    height: 14px;
    flex: none;
  }
  .ci.working {
    color: var(--accent);
    animation: spin 0.9s linear infinite;
  }
  .ci.waiting {
    color: var(--warn);
  }
  .ci.unread {
    color: var(--run);
  }
  .ci.read {
    color: var(--ink-faint);
  }
  /* Project folder — the primary label ("which project is this Claude in"). */
  .cdir {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    font-weight: 500;
    letter-spacing: -0.005em;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cloc {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
    font-size: 10.5px;
    color: var(--ink-faint);
    margin-top: 0.1rem;
  }
  /* When waiting, Claude's `waitingFor` reason sits at the row's right. */
  .cwait {
    flex: none;
    color: var(--warn);
    font-variant-numeric: tabular-nums;
  }
  .tnote {
    padding: 0.3rem 0.45rem;
    color: var(--ink-faint);
    font-size: 11px;
  }
</style>
