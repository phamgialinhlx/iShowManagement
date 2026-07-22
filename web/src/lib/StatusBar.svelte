<script lang="ts">
  interface Props {
    sessions: number
    forwards: number
    proxies: number
    active?: { name: string; live: boolean }
    onShowTunnels: () => void
  }
  let { sessions, forwards, proxies, active, onShowTunnels }: Props = $props()
</script>

<footer class="statusbar">
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
    padding: 0 1.35rem;
    border-top: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink-faint);
    font-size: 11.5px;
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
