<script lang="ts">
  interface Props {
    sessions: number
    forwards: number
    proxies: number
    active?: { name: string; live: boolean }
    onShowTunnels: () => void
    sideCollapsed: boolean
    onToggleSide: () => void
  }
  import { toggleSideLabel } from './platform'
  let { sessions, forwards, proxies, active, onShowTunnels, sideCollapsed, onToggleSide }: Props = $props()
</script>

<footer class="statusbar">
  <button
    class="dock"
    class:on={!sideCollapsed}
    title={`${sideCollapsed ? 'Show' : 'Hide'} sidebar (${toggleSideLabel})`}
    aria-label="Toggle sidebar"
    onclick={onToggleSide}
  >
    <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round">
      <rect x="1.6" y="2.6" width="12.8" height="10.8" rx="1.6" />
      <line x1="6" y1="2.6" x2="6" y2="13.4" />
    </svg>
  </button>
  <span class="item"><span class="g"></span>{sessions} session{sessions === 1 ? '' : 's'}</span>
  <button class="item" onclick={onShowTunnels}><span class="b"></span>{forwards} forward{forwards === 1 ? '' : 's'}</button>
  <button class="item" onclick={onShowTunnels}><span class="r"></span>{proxies} prox{proxies === 1 ? 'y' : 'ies'}</button>
  <span class="right mono">{active ? `${active.name} · ${active.live ? 'connected' : 'idle'}` : 'no host selected'}</span>
</footer>

<style>
  .statusbar {
    grid-row: 2 / 3;
    grid-column: 1 / 3;
    display: flex;
    align-items: center;
    gap: 1.4rem;
    height: 30px;
    /* No left padding: the sidebar-toggle dock sits flush in the corner. */
    padding: 0 1.35rem 0 0;
    border-top: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink-faint);
    font-size: 11.5px;
  }
  .dock {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 100%;
    border: none;
    border-right: 1px solid var(--line);
    border-radius: 0;
    background: none;
    color: var(--ink-faint);
    cursor: pointer;
    padding: 0;
    flex: none;
  }
  .dock:hover {
    color: var(--ink);
    background: var(--surface-2);
  }
  .dock.on {
    color: var(--accent);
  }
  .item {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    font-size: 11.5px;
    padding: 0;
    cursor: default;
  }
  button.item {
    cursor: pointer;
  }
  button.item:hover {
    color: var(--ink-dim);
  }
  .g {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--run);
  }
  .b {
    width: 6px;
    height: 6px;
    border-radius: 2px;
    background: var(--accent);
  }
  .r {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    border: 1px solid var(--ink-dim);
  }
  .right {
    margin-left: auto;
  }
</style>
