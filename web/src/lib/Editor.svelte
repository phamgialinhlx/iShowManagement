<script lang="ts">
  import { onMount } from 'svelte'
  import { basicSetup } from 'codemirror'
  import { EditorView } from '@codemirror/view'
  import { EditorState, type Extension } from '@codemirror/state'
  import { StreamLanguage } from '@codemirror/language'
  import { oneDark } from '@codemirror/theme-one-dark'
  import { rust } from '@codemirror/lang-rust'
  import { python } from '@codemirror/lang-python'
  import { javascript } from '@codemirror/lang-javascript'
  import { json } from '@codemirror/lang-json'
  import { html } from '@codemirror/lang-html'
  import { css } from '@codemirror/lang-css'
  import { markdown } from '@codemirror/lang-markdown'
  import { yaml } from '@codemirror/lang-yaml'
  import { sql } from '@codemirror/lang-sql'
  import { shell } from '@codemirror/legacy-modes/mode/shell'

  interface Props {
    text: string
    name: string
    onchange?: (value: string) => void
  }
  let { text, name, onchange }: Props = $props()

  let host: HTMLDivElement
  let view: EditorView | undefined

  // Pick a language extension by file extension. Unknown → no highlighting
  // (still fully editable). Returns an array so the caller can spread it.
  function langFor(name: string): Extension[] {
    const ext = name.split('.').pop()?.toLowerCase() ?? ''
    switch (ext) {
      case 'rs': return [rust()]
      case 'py': return [python()]
      case 'js': case 'mjs': case 'cjs': return [javascript()]
      case 'ts': return [javascript({ typescript: true })]
      case 'jsx': return [javascript({ jsx: true })]
      case 'tsx': return [javascript({ jsx: true, typescript: true })]
      case 'json': return [json()]
      case 'html': case 'htm': return [html()]
      case 'css': return [css()]
      case 'md': case 'markdown': return [markdown()]
      case 'yaml': case 'yml': return [yaml()]
      case 'sql': return [sql()]
      case 'sh': case 'bash': case 'zsh': case 'conf': case 'env': case 'toml':
        return [StreamLanguage.define(shell)]
      default: return []
    }
  }

  onMount(() => {
    view = new EditorView({
      state: EditorState.create({
        doc: text,
        extensions: [
          basicSetup,
          oneDark,
          EditorView.lineWrapping,
          ...langFor(name),
          EditorView.updateListener.of((u) => {
            if (u.docChanged && onchange) onchange(u.state.doc.toString())
          }),
        ],
      }),
      parent: host,
    })
    return () => view?.destroy()
  })
</script>

<div class="cm-host" bind:this={host}></div>

<style>
  .cm-host {
    height: 100%;
  }
  :global(.cm-host .cm-editor) {
    height: 100%;
  }
  :global(.cm-host .cm-scroller) {
    font: 12px/1.6 var(--font-mono);
  }
</style>
