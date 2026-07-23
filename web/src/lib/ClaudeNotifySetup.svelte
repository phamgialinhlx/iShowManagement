<script lang="ts">
  import { fly } from 'svelte/transition'

  interface Props {
    hostName: string
    installing: boolean
    error?: string
    onInstall: () => void
    onDismiss: () => void
  }
  let { hostName, installing, error, onInstall, onDismiss }: Props = $props()

  let showDetail = $state(false)
</script>

<div class="card" transition:fly={{ y: 14, duration: 220 }}>
  <div class="head">
    <span class="bell" aria-hidden="true">
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
        <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
        <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
      </svg>
    </span>
    <div class="titles">
      <div class="t">Notify me about Claude</div>
      <div class="sub">on <span class="host">{hostName}</span></div>
    </div>
    <button class="dismiss" title="Not now" aria-label="Dismiss" onclick={onDismiss}>×</button>
  </div>

  <p class="lead">
    Get a macOS banner when Claude <b>finishes</b> or <b>needs you</b> — for tmux
    sessions you open here.
  </p>

  <button class="detail-toggle" onclick={() => (showDetail = !showDetail)}>
    <span class="chev" class:open={showDetail}>▸</span> What this installs
  </button>
  {#if showDetail}
    <ul class="detail">
      <li>a <span class="mono">Stop</span> + <span class="mono">Notification</span> hook in <span class="mono">~/.claude/settings.json</span></li>
      <li>a small <span class="mono">~/.claude/ism-notify.sh</span></li>
      <li class="muted">merged into your existing settings · fully reversible</li>
    </ul>
  {/if}

  {#if error}<div class="err">{error}</div>{/if}

  <div class="actions">
    <button class="ghost" onclick={onDismiss} disabled={installing}>Not now</button>
    <button class="go" onclick={onInstall} disabled={installing}>
      {#if installing}<span class="spin" aria-hidden="true"></span>Enabling…{:else}Enable{/if}
    </button>
  </div>
</div>

<style>
  .card {
    position: absolute;
    right: 18px;
    bottom: 18px;
    z-index: 30;
    width: 320px;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 12px;
    padding: 1rem 1.05rem 0.9rem;
    box-shadow: 0 20px 48px -16px #000, 0 0 0 1px rgba(255, 255, 255, 0.02) inset;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 0.6rem;
  }
  .bell {
    width: 30px;
    height: 30px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 8px;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .titles {
    flex: 1;
    min-width: 0;
  }
  .t {
    font-size: 13.5px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .sub {
    font-size: 11.5px;
    color: var(--ink-faint);
    margin-top: 1px;
  }
  .sub .host {
    color: var(--ink-dim);
    font-family: var(--font-mono);
  }
  .dismiss {
    flex: none;
    width: 22px;
    height: 22px;
    border: none;
    background: none;
    color: var(--ink-faint);
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
    border-radius: 5px;
    margin: -2px -4px 0 0;
  }
  .dismiss:hover {
    color: var(--ink);
    background: var(--surface);
  }
  .lead {
    margin: 0.7rem 0 0.15rem;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--ink-dim);
  }
  .lead b {
    color: var(--ink);
    font-weight: 600;
  }
  .detail-toggle {
    margin-top: 0.55rem;
    background: none;
    border: none;
    color: var(--ink-faint);
    font: inherit;
    font-size: 11.5px;
    cursor: pointer;
    padding: 0.15rem 0;
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
  }
  .detail-toggle:hover {
    color: var(--ink-dim);
  }
  .chev {
    font-size: 8px;
    transition: transform 0.15s ease;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .detail {
    list-style: none;
    margin: 0.4rem 0 0.2rem;
    padding: 0.55rem 0.7rem;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 8px;
    font-size: 11.5px;
    line-height: 1.7;
    color: var(--ink-dim);
  }
  .detail .mono {
    font-family: var(--font-mono);
    color: var(--ink);
    font-size: 11px;
  }
  .detail .muted {
    color: var(--ink-faint);
    border-top: 1px solid var(--line-2);
    margin-top: 0.35rem;
    padding-top: 0.35rem;
  }
  .err {
    margin-top: 0.6rem;
    font-size: 11.5px;
    color: var(--danger);
    word-break: break-word;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.9rem;
  }
  .ghost,
  .go {
    border-radius: 8px;
    padding: 0.45rem 0.85rem;
    font: inherit;
    font-size: 12.5px;
    font-weight: 600;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
  }
  .ghost {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
  }
  .ghost:hover:not(:disabled) {
    color: var(--ink);
    border-color: var(--ink-faint);
  }
  .go {
    background: var(--accent);
    border: 1px solid var(--accent);
    color: #2e3440;
  }
  .go:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  .go:disabled,
  .ghost:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .spin {
    width: 11px;
    height: 11px;
    border: 1.5px solid rgba(46, 52, 64, 0.35);
    border-top-color: #2e3440;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
