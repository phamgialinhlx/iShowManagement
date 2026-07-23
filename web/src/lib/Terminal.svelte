<script lang="ts">
  import { onMount, onDestroy } from 'svelte'
  import { Terminal } from '@xterm/xterm'
  import { FitAddon } from '@xterm/addon-fit'
  import '@xterm/xterm/css/xterm.css'
  import { pasteImage } from './api'
  import { alertDialog } from './dialogs.svelte'

  interface Props {
    mode: string
    alias?: string
    session?: string
    cid?: string
    onStatus?: (s: 'connecting' | 'connected' | 'closed') => void
  }
  let { mode, alias, session, cid, onStatus }: Props = $props()

  let host: HTMLDivElement
  let term: Terminal | undefined
  let socket: WebSocket | undefined
  let ro: ResizeObserver | undefined

  // Pasting an image uploads it to the remote host's /tmp and types the path,
  // so CLIs like Claude Code can read it (see plans/2026-07-23-image-paste.md).
  // console/tmux only; elsewhere image pastes stay no-ops as before. Capture
  // phase so this runs before xterm's own paste handler on the hidden textarea.
  function onPaste(ev: ClipboardEvent) {
    const item = Array.from(ev.clipboardData?.items ?? []).find(
      (i) => i.kind === 'file' && i.type.startsWith('image/'),
    )
    if (!item || !alias) return // text paste: let xterm handle it
    ev.preventDefault()
    ev.stopImmediatePropagation()
    const blob = item.getAsFile()
    if (!blob) return
    pasteImage(alias, blob)
      .then((path) => term?.paste(path + ' '))
      .catch((e) => alertDialog(`Image paste failed: ${e.message}`))
  }
  const interceptImagePaste = () => mode === 'console' || mode === 'tmux'

  onMount(() => {
    let cancelled = false
    // Lilex is a bundled webfont (unlike the old SF Mono, which was a local
    // face): make sure it's loaded before xterm measures glyph widths, or a
    // cold load measures the fallback and the grid renders at the wrong size.
    document.fonts.load('15px Lilex').then(setup, setup)
    return () => {
      cancelled = true
    }

    function setup() {
      if (cancelled) return
      term = new Terminal({
        fontFamily: "'Lilex', 'SF Mono', SFMono-Regular, ui-monospace, Menlo, monospace",
        fontSize: 15,
        lineHeight: 1.0,
        cursorBlink: true,
        /* The official Nord terminal scheme (nordtheme.com). */
        theme: {
          background: '#2e3440',
          foreground: '#d8dee9',
          cursor: '#d8dee9',
          cursorAccent: '#2e3440',
          selectionBackground: 'rgba(76,86,106,0.55)',
          black: '#3b4252',
          brightBlack: '#4c566a',
          red: '#bf616a',
          brightRed: '#bf616a',
          green: '#a3be8c',
          brightGreen: '#a3be8c',
          yellow: '#ebcb8b',
          brightYellow: '#ebcb8b',
          blue: '#81a1c1',
          brightBlue: '#81a1c1',
          magenta: '#b48ead',
          brightMagenta: '#b48ead',
          cyan: '#88c0d0',
          brightCyan: '#8fbcbb',
          white: '#e5e9f0',
          brightWhite: '#eceff4',
        },
      })
      const fit = new FitAddon()
      term.loadAddon(fit)
      term.open(host)
      fit.fit()

      const sendResize = () => {
        if (socket?.readyState === WebSocket.OPEN && term) {
          socket.send(JSON.stringify({ t: 'r', cols: term.cols, rows: term.rows }))
        }
      }

      const params = new URLSearchParams({ mode })
      if (alias) params.set('alias', alias)
      if (session) params.set('session', session)
      if (cid) params.set('cid', cid)

      onStatus?.('connecting')
      const proto = location.protocol === 'https:' ? 'wss' : 'ws'
      socket = new WebSocket(`${proto}://${location.host}/ws?${params}`)
      socket.binaryType = 'arraybuffer'

      socket.onopen = () => {
        onStatus?.('connected')
        sendResize()
        term!.focus()
      }
      socket.onmessage = (ev) => {
        if (ev.data instanceof ArrayBuffer) term!.write(new Uint8Array(ev.data))
        else term!.write(ev.data)
      }
      socket.onclose = () => {
        onStatus?.('closed')
        term!.write('\r\n\x1b[90m[disconnected]\x1b[0m\r\n')
      }

      term.onData((data) => {
        if (socket?.readyState === WebSocket.OPEN) {
          socket.send(new TextEncoder().encode(data))
        }
      })
      term.onResize(sendResize)

      ro = new ResizeObserver(() => fit.fit())
      ro.observe(host)

      if (interceptImagePaste()) host.addEventListener('paste', onPaste, true)
    }
  })

  onDestroy(() => {
    host?.removeEventListener('paste', onPaste, true)
    ro?.disconnect()
    socket?.close()
    term?.dispose()
  })
</script>

<div class="term" bind:this={host}></div>

<style>
  .term {
    height: 100%;
    padding: 0.5rem;
    box-sizing: border-box;
    /* Match the xterm theme background so the padding ring doesn't show a
       mismatched app background around the terminal. */
    background: #2e3440; /* nord0 */
  }
  :global(.term .xterm),
  :global(.term .xterm-viewport) {
    height: 100% !important;
  }
</style>
