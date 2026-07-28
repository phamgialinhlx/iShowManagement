<script lang="ts">
  import { untrack } from 'svelte'
  import {
    listFiles,
    viewFile,
    saveFile,
    downloadFileUrl,
    type Listing,
    type FileEntry,
    type FileView,
  } from './api'
  import Editor from './Editor.svelte'

  interface Props {
    id: string
  }
  let { id }: Props = $props()

  let listing = $state<Listing>()
  let preview = $state<FileView>()
  let error = $state('')
  let loading = $state(false)

  // --- autosave state for the editable text file currently open ---
  let currentPath = $state('') // path loaded into the editor
  let currentId = $state('') // host id of that file (so a host switch saves to the right host)
  let currentText = $state('') // latest editor text (what the next save writes)
  let dirty = $state(false) // unsaved edits exist
  let saving = $state(false) // a save is in flight
  let saveStatus = $state('') // '' | 'unsaved' | 'saving…' | 'saved HH:MM:SS' | '⚠ save failed: …'
  let saveError = $state('')
  let loadingFile = $state(false) // suppresses save during a programmatic load
  let debounceTimer: ReturnType<typeof setTimeout> | undefined
  let inFlight: Promise<void> | null = null

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
    // Flush the file we're leaving before the preview pane is cleared — covers
    // the "up" button and directory navigation, which both destroy the editor.
    await flushSave()
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
      return
    }
    // Flush the file we're leaving before switching.
    await flushSave()
    loadingFile = true
    loading = true
    error = ''
    try {
      preview = await viewFile(id, entry.path)
      if (preview?.type === 'text') {
        currentId = id
        currentPath = preview.path
        currentText = preview.text ?? ''
        dirty = false
        saving = false
        saveError = ''
        saveStatus = ''
      }
    } catch (e) {
      error = String(e)
    } finally {
      loading = false
      loadingFile = false
    }
  }

  // Editor reported a change → mark dirty and (re)start the debounce timer.
  function onEdit(value: string) {
    currentText = value
    scheduleSave()
  }

  function scheduleSave() {
    if (loadingFile) return // ignore the "edit" that is just us loading a file
    dirty = true
    saveError = ''
    saveStatus = 'unsaved'
    if (debounceTimer) clearTimeout(debounceTimer)
    debounceTimer = setTimeout(runSave, 800)
  }

  // Save once, using the current text. If a save is already in flight, bail:
  // when it finishes it will see `dirty` still true (currentText differs from
  // what it saved) and reschedule, so the latest edits are not lost.
  async function runSave() {
    if (debounceTimer) {
      clearTimeout(debounceTimer)
      debounceTimer = undefined
    }
    saveError = '' // clear so a retry (which calls runSave directly) doesn't
                   // briefly show the old error color / retry button mid-save
    if (inFlight || !currentPath) return
    saving = true
    saveStatus = 'saving…'
    const saveId = currentId
    const path = currentPath
    const text = currentText // capture exactly what we are saving
    inFlight = (async () => {
      try {
        await saveFile(saveId, path, text)
        saveError = ''
      } catch (e) {
        saveError = String(e)
      }
    })()
    await inFlight
    inFlight = null
    saving = false
    if (saveError) {
      saveStatus = `⚠ save failed: ${saveError}`
      // dirty stays true so the retry button / next edit can re-attempt
    } else {
      // Only clear dirty if the editor did not move on while we were saving.
      dirty = currentText !== text
      saveStatus = dirty ? 'unsaved' : `saved ${clock()}`
    }
    if (dirty && !saveError) scheduleSave() // edits landed during the save
  }

  // Force pending edits to disk before switching files / hosts. Bypasses the
  // debounce and drains until quiet or an error.
  async function flushSave() {
    if (debounceTimer) {
      clearTimeout(debounceTimer)
      debounceTimer = undefined
    }
    while (true) {
      while (inFlight) await inFlight
      if (!dirty || !currentPath || saveError) return
      void runSave() // sets inFlight synchronously
    }
  }

  function clock(): string {
    return new Date().toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    })
  }

  // (Re)load when the target server changes. `load` flushes the outgoing file
  // (saving to its original host via `currentId`) before listing the new one.
  // untrack keeps this effect dependent on `id` only, not the save state.
  $effect(() => {
    const _ = id
    untrack(() => {
      void load('')
    })
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
        {#if preview.type === 'text' && preview.editable}
          <span
            class="save-status"
            class:unsaved={dirty && !saving && !saveError}
            class:saving={saving}
            class:err={!!saveError}
          >
            {saveStatus}
            {#if saveError}
              <button class="retry" onclick={() => void runSave()}>retry</button>
            {/if}
          </span>
        {/if}
        <a class="dl" href={downloadFileUrl(id, preview.path)}>download</a>
      </div>
      <div class="pv-body">
        {#if preview.type === 'text'}
          {#if preview.editable}
            {#key preview.path}
              <!-- `?? ''` is a type narrowing fix, not a behavior change: the
                 backend always sets `text` for type === 'text' (FileView.text is
                 optional only because image/too_large/unsupported omit it; the
                 {#if type === 'text'} guard does not narrow the sibling field). -->
              <Editor text={preview.text ?? ''} name={preview.name} onchange={onEdit} />
            {/key}
          {:else}
            <pre>{preview.text}</pre>
            <div class="muted ro-note">not editable: not UTF-8 — download to edit externally.</div>
          {/if}
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
  .save-status {
    margin-left: 0.5rem;
    font-size: 11.5px;
    font-family: var(--font-mono);
    color: var(--ink-faint);
  }
  .save-status.unsaved {
    color: var(--accent);
  }
  .save-status.saving {
    color: var(--ink-dim);
  }
  .save-status.err {
    color: var(--danger);
  }
  .retry {
    margin-left: 0.4rem;
    background: none;
    border: 1px solid var(--danger);
    color: var(--danger);
    border-radius: 6px;
    padding: 0.05rem 0.4rem;
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .retry:hover {
    background: rgba(211, 121, 111, 0.12);
  }
  .ro-note {
    padding: 0.4rem 0.9rem 0;
    font-size: 12px;
  }
</style>
