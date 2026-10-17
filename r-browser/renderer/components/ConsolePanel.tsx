import { useMemo, useState } from 'react'
import type { CaptureState, ConsoleEntry } from '@shared/contract'
import { Empty, OutcomeLabel, Seg } from './Bits'
import { clock } from '../lib/format'
import { useOutcome, usePref } from '../lib/hooks'

type Filter = 'all' | 'error' | 'warn' | 'log'

/** `contract.ts` keeps `level` a free string on purpose — browsers add levels
 *  and an unknown one must not drop the entry. So the *bucket* is derived here,
 *  and anything unrecognised falls into `log` rather than disappearing. */
export function bucket(level: string): 'error' | 'warn' | 'log' {
  const l = level.toLowerCase()
  if (l.startsWith('err') || l === 'severe' || l === 'assert') return 'error'
  if (l.startsWith('warn')) return 'warn'
  return 'log'
}

export function ConsolePanel({
  entries, capture, onClear, onCopy,
}: {
  entries: ConsoleEntry[]
  capture: CaptureState | null
  onClear: () => Promise<void>
  onCopy: (text: string) => Promise<void>
}) {
  const [filter, setFilter] = usePref<Filter>('console.filter', 'all')
  const [needle, setNeedle] = useState('')
  const [open, setOpen] = useState<Record<string, boolean>>({})
  const { outcome, run } = useOutcome()

  const shown = useMemo(() => {
    const n = needle.trim().toLowerCase()
    return entries.filter(e => {
      if (filter !== 'all' && bucket(e.level) !== filter) return false
      if (!n) return true
      return e.text.toLowerCase().includes(n) || (e.url ?? '').toLowerCase().includes(n)
    })
  }, [entries, filter, needle])

  const counts = useMemo(() => {
    let error = 0, warn = 0
    for (const e of entries) {
      const b = bucket(e.level)
      if (b === 'error') error++
      else if (b === 'warn') warn++
    }
    return { error, warn }
  }, [entries])

  return (
    <div className="inspector-body">
      <div className="panel-bar">
        <Seg
          value={filter}
          options={[
            { value: 'all', label: `all ${entries.length}` },
            { value: 'error', label: `err ${counts.error}` },
            { value: 'warn', label: `warn ${counts.warn}` },
            { value: 'log', label: 'log' },
          ]}
          onChange={setFilter}
        />
        <input
          className="field"
          style={{ flex: '1 1 90px', minWidth: 70 }}
          placeholder="filter"
          value={needle}
          onChange={e => setNeedle(e.target.value)}
        />
        <button
          type="button"
          className="chip"
          onClick={() => void run('clearing', async () => { await onClear(); return 'cleared' })}
        >
          clear
        </button>
        <button
          type="button"
          className="chip"
          onClick={() => void run('copying', async () => {
            await onCopy(asText(shown))
            return `copied · ${shown.length} lines`
          })}
        >
          copy
        </button>
        <OutcomeLabel outcome={outcome} />
      </div>

      {/*
        Capture state, said out loud.
        Chromium allows one debugger client per target and the DevTools window is
        one, so opening the inspector detaches our CDP session. A log that went
        quiet without explanation reads as "the log stopped working"; this line
        is the difference between a degraded feature and a broken app.
      */}
      {capture && capture.console !== 'cdp' && (
        <div className="pad">
          <span className="micro busy">
            {capture.console === 'fallback'
              ? 'devtools has the debugger — stacks and network are paused'
              : 'console capture is off'}
          </span>
          {capture.reason && <div className="micro faint">{capture.reason}</div>}
        </div>
      )}

      <div className="scroll-list">
        {shown.length === 0 && (
          <Empty
            phase={entries.length ? 'nothing matches this filter' : 'no console output yet'}
            detail={entries.length ? undefined : 'lines appear here as the page runs'}
          />
        )}
        {shown.map(e => {
          const b = bucket(e.level)
          const expanded = open[e.id] === true
          return (
            <div key={e.id}>
              <div
                className="log-row"
                data-level={b}
                onClick={() => e.stack && setOpen(o => ({ ...o, [e.id]: !expanded }))}
                style={{ cursor: e.stack ? 'pointer' : 'default' }}
              >
                <span className="micro faint data" style={{ flex: '0 0 auto' }}>{clock(e.at)}</span>
                <span className="log-text selectable">{e.text}</span>
                {e.stack && <span className="micro faint">{expanded ? '−' : '+'}</span>}
              </div>
              {expanded && e.stack && <div className="log-stack selectable">{e.stack}</div>}
            </div>
          )
        })}
      </div>
    </div>
  )
}

/** The clipboard form. Stacks are included — a stack is most of why an error
 *  line is worth copying at all. */
export function asText(entries: ConsoleEntry[]): string {
  return entries
    .map(e => {
      const where = e.url ? ` (${e.url}${e.line ? `:${e.line}` : ''})` : ''
      const head = `[${e.level}] ${e.text}${where}`
      return e.stack ? `${head}\n${e.stack}` : head
    })
    .join('\n')
}
