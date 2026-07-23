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

  const badgeLabel = (s: ClaudeInstance['state']) =>
    s === 'working' ? 'working' : s === 'needs' ? 'needs you' : s === 'done' ? 'done' : 'running'

  // Claude's standard context window (the auto-compact threshold for Opus/Sonnet).
  const CTX_WINDOW = 200_000
  const ctxPct = (t: number) => Math.min(100, Math.round((t / CTX_WINDOW) * 100))
  // Shown as the token count in thousands, 2 decimals — e.g. 106265 → "106.27k".
  const ctxLabel = (t: number) => `${(t / 1000).toFixed(2)}k`

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
                  <button
                    class="cnode"
                    title={`Attach to pane ${inst.paneId ?? ''} (win ${inst.window ?? 0}·pane ${inst.pane ?? 0})`}
                    onclick={() => onAttachClaude(s.name, inst)}
                  >
                    <span class="cbody">
                      <span class="cl1">
                        <span class="cdot {inst.state}" aria-hidden="true"></span>
                        <span class="cdir">{inst.project ?? 'claude'}</span>
                        <span class="cbadge {inst.state}">{badgeLabel(inst.state)}</span>
                      </span>
                      <span class="cloc mono">
                        <span>win {inst.window ?? 0} · pane {inst.pane ?? 0}</span>
                        {#if inst.contextTokens != null}
                          {@const pct = ctxPct(inst.contextTokens)}
                          <span
                            class="cctx"
                            class:mid={pct >= 50 && pct < 80}
                            class:hi={pct >= 80}
                            title={`Context window: ${ctxLabel(inst.contextTokens)} / 200k tokens (${pct}%)`}
                          >{ctxLabel(inst.contextTokens)}</span>
                        {/if}
                      </span>
                      {#if inst.summary || inst.message}
                        <span class="csum">{inst.message ?? inst.summary}</span>
                      {/if}
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
  /* Status dot — the at-a-glance colour. `working` breathes so a live Claude
     reads as live; the rest are steady. */
  .cdot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex: none;
    background: var(--ink-faint);
  }
  .cdot.working {
    background: var(--run);
    animation: cpulse 1.4s ease-in-out infinite;
  }
  .cdot.needs {
    background: var(--warn);
  }
  .cdot.done {
    background: var(--run);
  }
  @keyframes cpulse {
    0%,
    100% {
      box-shadow: 0 0 0 0 rgba(163, 190, 140, 0.45);
      opacity: 1;
    }
    50% {
      box-shadow: 0 0 0 3px rgba(163, 190, 140, 0);
      opacity: 0.55;
    }
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
  /* Context-window fullness, pinned to the bottom-right of the row. Tints amber
     past half-full and red past 80% so a nearly-compacting session stands out. */
  .cctx {
    flex: none;
    color: var(--ink-dim);
    font-variant-numeric: tabular-nums;
  }
  .cctx.mid {
    color: var(--warn);
  }
  .cctx.hi {
    color: var(--danger);
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
  .cbadge.working {
    color: var(--run);
    background: rgba(163, 190, 140, 0.14);
  }
  .cbadge.needs {
    color: var(--warn);
    background: rgba(235, 203, 139, 0.14);
  }
  .cbadge.done {
    color: var(--run);
    background: rgba(163, 190, 140, 0.14);
  }
  .cbadge.unknown {
    color: var(--ink-faint);
    background: var(--surface-2);
  }
  .csum {
    font-size: 11px;
    color: var(--ink-dim);
    margin-top: 0.2rem;
    line-height: 1.35;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tnote {
    padding: 0.3rem 0.45rem;
    color: var(--ink-faint);
    font-size: 11px;
  }
</style>
