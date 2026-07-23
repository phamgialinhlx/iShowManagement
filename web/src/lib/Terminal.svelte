<script lang="ts">
  import { onMount, onDestroy } from 'svelte'
  import { Terminal } from '@xterm/xterm'
  import { FitAddon } from '@xterm/addon-fit'
  import '@xterm/xterm/css/xterm.css'

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
        lineHeight: 1.35,
        cursorBlink: true,
        theme: {
          background: '#0b0c0e',
          foreground: '#c4c6ca',
          cursor: '#8aa6c8',
          cursorAccent: '#0b0c0e',
          selectionBackground: 'rgba(138,166,200,0.25)',
          black: '#0b0c0e',
          brightBlack: '#5c5e64',
          red: '#d3796f',
          brightRed: '#d3796f',
          green: '#77c091',
          brightGreen: '#77c091',
          yellow: '#d6b36a',
          brightYellow: '#d6b36a',
          blue: '#8aa6c8',
          brightBlue: '#8aa6c8',
          magenta: '#b08ac8',
          brightMagenta: '#b08ac8',
          cyan: '#8ac8c4',
          brightCyan: '#8ac8c4',
          white: '#c4c6ca',
          brightWhite: '#e9eaec',
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
    }
  })

  onDestroy(() => {
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
  }
  :global(.term .xterm),
  :global(.term .xterm-viewport) {
    height: 100% !important;
  }
</style>
