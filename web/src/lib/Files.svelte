<script lang="ts">
  import {
    listFiles,
    viewFile,
    downloadFileUrl,
    type Listing,
    type FileEntry,
    type FileView,
  } from './api'

  interface Props {
    id: string
  }
  let { id }: Props = $props()

  let listing = $state<Listing>()
  let preview = $state<FileView>()
  let error = $state('')
  let loading = $state(false)

  function fmtSize(n: number): string {
    if (!n) return ''
    const u = ['B', 'K', 'M', 'G']
    let i = 0
    while (n >= 1024 && i < u.length - 1) {
      n /= 1024
      i++
    }
    return `${i === 0 ? n : n.toFixed(1)}${u[i]}`
  }

  async function load(path = '') {
    loading = true
    error = ''
    preview = undefined
    try {
      listing = await listFiles(id, path)
    } catch (e) {
      error = String(e)
    } finally {
      loading = false
    }
  }

  async function open(entry: FileEntry) {
    if (entry.type === 'dir') {
      await load(entry.path)
    } else {
      loading = true
      error = ''
      try {
        preview = await viewFile(id, entry.path)
      } catch (e) {
        error = String(e)
      } finally {
        loading = false
      }
    }
  }

  // (Re)load when the target server changes.
  $effect(() => {
    id
    load('')
  })
</script>

<div class="files">
  <div class="left">
    <div class="crumbs">
      {#if listing?.parent}
        <button class="up" onclick={() => load(listing!.parent!)}>↑ up</button>
      {/if}
      <span class="cwd" title={listing?.path}>{listing?.path ?? '…'}</span>
    </div>
    {#if error}<div class="err">{error}</div>{/if}
    <ul class="entries">
      {#each listing?.entries ?? [] as e (e.path)}
        <li>
          <button class="entry" class:sel={preview?.path === e.path} onclick={() => open(e)}>
            <span class="ico">{e.type === 'dir' ? '📁' : e.type === 'link' ? '🔗' : '📄'}</span>
            <span class="fname">{e.name}</span>
            <span class="fsize">{e.type === 'file' ? fmtSize(e.size) : ''}</span>
          </button>
        </li>
      {/each}
      {#if listing && listing.entries.length === 0}
        <li class="muted empty">empty directory</li>
      {/if}
    </ul>
  </div>

  <div class="right">
    {#if preview}
      <div class="pv-head">
        <span class="pv-name">{preview.name}</span>
        <span class="muted">{preview.mime} · {fmtSize(preview.size)}</span>
        <a class="dl" href={downloadFileUrl(id, preview.path)}>download</a>
      </div>
      <div class="pv-body">
        {#if preview.type === 'text'}
          <pre>{preview.text}</pre>
        {:else if preview.type === 'image'}
          <img src={preview.dataUrl} alt={preview.name} />
        {:else if preview.type === 'too_large'}
          <div class="muted pad">Too large to preview ({fmtSize(preview.size)}). Use download.</div>
        {:else}
          <div class="muted pad">No preview for this file type. Use download.</div>
        {/if}
      </div>
    {:else}
      <div class="muted pad">Select a file to preview.</div>
    {/if}
  </div>
</div>

<style>
  .files {
    display: grid;
    grid-template-columns: 320px 1fr;
    height: 100%;
    min-height: 0;
    background: var(--bg);
  }
  .left {
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.55rem 0.75rem;
    border-bottom: 1px solid var(--line);
  }
  .up {
    background: none;
    border: 1px solid var(--line);
    color: var(--ink-dim);
    border-radius: 6px;
    padding: 0.12rem 0.45rem;
    cursor: pointer;
    font: inherit;
    font-size: 12px;
  }
  .up:hover { color: var(--ink); border-color: var(--ink-faint); }
  .cwd {
    color: var(--ink-faint);
    font-size: 11.5px;
    font-family: var(--font-mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
  }
  .entries {
    list-style: none;
    margin: 0;
    padding: 0.3rem;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .entry {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    padding: 0.4rem 0.5rem;
    border-radius: 6px;
    cursor: pointer;
  }
  .entry:hover { background: var(--surface); }
  .entry.sel { background: var(--surface-2); }
  .fname { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fsize { color: var(--ink-faint); font-size: 11px; font-family: var(--font-mono); }
  .empty { padding: 0.5rem; }
  .right {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .pv-head {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.55rem 0.85rem;
    border-bottom: 1px solid var(--line);
  }
  .pv-name { font-weight: 500; }
  .dl {
    margin-left: auto;
    color: var(--accent);
    text-decoration: none;
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 0.18rem 0.6rem;
    font-size: 12px;
  }
  .dl:hover { border-color: var(--ink-faint); }
  .pv-body {
    overflow: auto;
    flex: 1;
    min-height: 0;
    padding: 0.9rem;
  }
  .pv-body pre {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    font: 12px/1.6 var(--font-mono);
    color: var(--ink);
  }
  .pv-body img {
    max-width: 100%;
    image-rendering: pixelated;
    background: #fff;
  }
  .muted { color: var(--ink-faint); }
  .err { color: var(--danger); padding: 0.5rem 0.7rem; font-size: 12px; }
  .pad { padding: 1rem; }
</style>
