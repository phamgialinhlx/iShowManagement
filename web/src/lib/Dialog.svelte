<script lang="ts">
  import { dialogState, settle } from './dialogs.svelte'

  let input = $state('')

  // Seed the field each time a prompt opens.
  $effect(() => {
    const c = dialogState.current
    if (c?.kind === 'prompt') input = c.value
  })

  function ok() {
    const c = dialogState.current
    if (!c) return
    settle(c.kind === 'prompt' ? input : true)
  }
  function cancel() {
    settle(dialogState.current?.kind === 'prompt' ? null : false)
  }
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      cancel()
    } else if (e.key === 'Enter') {
      e.preventDefault()
      ok()
    }
  }
</script>

<svelte:window onkeydown={dialogState.current ? onkey : undefined} />

{#if dialogState.current}
  {@const c = dialogState.current}
  <div
    class="backdrop"
    role="presentation"
    onclick={(e) => e.target === e.currentTarget && cancel()}
  >
    <div class="dlg" role="dialog" aria-modal="true" tabindex="-1">
      <p class="msg">{c.message}</p>
      {#if c.kind === 'prompt'}
        <!-- svelte-ignore a11y_autofocus -->
        <input class="in" type={c.password ? 'password' : 'text'} bind:value={input} autofocus />
      {/if}
      <div class="actions">
        {#if c.kind !== 'alert'}<button class="b" onclick={cancel}>Cancel</button>{/if}
        <button class="b ok" class:danger={c.danger} onclick={ok}>{c.okLabel}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .dlg {
    width: min(90vw, 380px);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 1.1rem 1.1rem 0.9rem;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
  }
  .msg {
    margin: 0 0 0.9rem;
    color: var(--ink);
    font-size: 13px;
    line-height: 1.5;
  }
  .in {
    width: 100%;
    box-sizing: border-box;
    margin-bottom: 0.9rem;
    padding: 0.5rem 0.6rem;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 6px;
    color: var(--ink);
    font: inherit;
  }
  .in:focus {
    outline: none;
    border-color: var(--accent);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .b {
    padding: 0.4rem 0.85rem;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--surface);
    color: var(--ink-dim);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .b:hover {
    color: var(--ink);
    background: var(--bg);
  }
  .b.ok {
    background: var(--accent-soft);
    border-color: transparent;
    color: var(--ink);
  }
  .b.ok:hover {
    background: var(--accent);
    color: #0b0c0e;
  }
  .b.ok.danger {
    background: rgba(211, 121, 111, 0.14);
    color: var(--danger);
  }
  .b.ok.danger:hover {
    background: var(--danger);
    color: #0b0c0e;
  }
</style>
