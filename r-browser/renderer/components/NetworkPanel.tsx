import { useMemo, useState } from 'react'
import type { NetworkEntry } from '@shared/contract'
import { Empty, OutcomeLabel, Seg } from './Bits'
import { bytes, millis } from '../lib/format'
import { pathOf } from '../lib/url'
import { useOutcome, usePref } from '../lib/hooks'

type Filter = 'all' | 'xhr' | 'doc' | 'js' | 'css' | 'img' | 'failed'

/** CDP's `resourceType` is a display name (`XHR`, `Fetch`, `Stylesheet`), and
 *  the set grows. Unknown types fall through to `all` only — never into a
 *  category they do not belong to, which would make a filter lie. */
function matches(filter: Filter, e: NetworkEntry): boolean {
  const t = (e.type ?? '').toLowerCase()
  switch (filter) {
    case 'all': return true
    case 'failed': return e.failed === true || (e.status !== undefined && e.status >= 400)
    case 'xhr': return t === 'xhr' || t === 'fetch'
    case 'doc': return t === 'document'
    case 'js': return t === 'script'
    case 'css': return t === 'stylesheet'
    case 'img': return t === 'image' || t === 'media' || t === 'font'
  }
}

export function NetworkPanel({
  entries, onClear, onHar, onBody, onCopy,
}: {
  entries: NetworkEntry[]
  onClear: () => Promise<void>
  onHar: () => Promise<string>
  onBody: (requestId: string) => Promise<{ request?: string; response?: string }>
  onCopy: (text: string) => Promise<void>
}) {
  const [filter, setFilter] = usePref<Filter>('network.filter', 'all')
  const [needle, setNeedle] = useState('')
  const [openId, setOpenId] = useState<string | null>(null)
  const [bodies, setBodies] = useState<Record<string, { request?: string; response?: string }>>({})
  const { outcome, run } = useOutcome()
  const body = useOutcome()

  const shown = useMemo(() => {
    const n = needle.trim().toLowerCase()
    return entries.filter(e => matches(filter, e) && (!n || e.url.toLowerCase().includes(n)))
  }, [entries, filter, needle])

  return (
    <div className="inspector-body">
      <div className="panel-bar">
        <Seg
          value={filter}
          options={[
            { value: 'all', label: `all ${entries.length}` },
            { value: 'xhr', label: 'xhr' },
            { value: 'doc', label: 'doc' },
            { value: 'js', label: 'js' },
            { value: 'css', label: 'css' },
            { value: 'img', label: 'img' },
            { value: 'failed', label: 'failed' },
          ]}
          onChange={setFilter}
        />
        <input
          className="field"
          style={{ flex: '1 1 90px', minWidth: 70 }}
          placeholder="filter url"
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
          onClick={() => void run('building har', async () => {
            const har = await onHar()
            await onCopy(har)
            return `copied · ${bytes(har.length)}`
          })}
        >
          copy har
        </button>
        <OutcomeLabel outcome={outcome} />
      </div>

      <div className="scroll-list">
        {shown.length === 0 && (
          <Empty
            phase={entries.length ? 'nothing matches this filter' : 'no requests yet'}
            detail={entries.length ? undefined : 'requests appear here as the page loads'}
          />
        )}
        {shown.map(e => {
          const bad = e.failed === true || (e.status !== undefined && e.status >= 400)
          const warn = !bad && e.status !== undefined && e.status >= 300
          const expanded = openId === e.id
          return (
            <div key={e.id}>
              <button
                type="button"
                className="net-row"
                data-bad={bad ? 'true' : undefined}
                data-warn={warn ? 'true' : undefined}
                onClick={() => setOpenId(expanded ? null : e.id)}
              >
                <span>{e.method}</span>
                <span>{e.failed ? 'err' : (e.status ?? '—')}</span>
                <span className="truncate">{(e.type ?? '—').toLowerCase().slice(0, 4)}</span>
                <span className="data">{millis(e.durationMs)}</span>
                <span className="data">{e.fromCache ? 'cache' : bytes(e.size)}</span>
                {/* Truncated from the left, so the path — the part that
                    identifies the request — survives. */}
                <span className="truncate-left">{pathOf(e.url)}</span>
              </button>

              {expanded && (
                <div className="net-detail">
                  <div style={{ wordBreak: 'break-all', marginBottom: 6 }}>{e.url}</div>
                  {e.errorText && <div className="alert" style={{ marginBottom: 6 }}>{e.errorText}</div>}
                  <Headers title="request headers" value={e.requestHeaders} />
                  <Headers title="response headers" value={e.responseHeaders} />
                  <div className="row" style={{ marginTop: 6 }}>
                    {/* Bodies are fetched on demand and never streamed with the
                        entry: they are large and frequently hold tokens, and a
                        log that quietly accumulates every response body is a
                        credential store nobody meant to build. */}
                    <button
                      type="button"
                      className="chip"
                      disabled={body.busy}
                      onClick={() => void body.run('fetching', async () => {
                        const got = await onBody(e.id)
                        setBodies(b => ({ ...b, [e.id]: got }))
                        return got.response || got.request ? 'loaded' : 'no body'
                      })}
                    >
                      body
                    </button>
                    <OutcomeLabel outcome={body.outcome} />
                  </div>
                  {bodies[e.id]?.request && (
                    <>
                      <div className="micro" style={{ marginTop: 6 }}>request body</div>
                      <pre>{bodies[e.id]?.request}</pre>
                    </>
                  )}
                  {bodies[e.id]?.response && (
                    <>
                      <div className="micro" style={{ marginTop: 6 }}>response body</div>
                      <pre>{bodies[e.id]?.response}</pre>
                    </>
                  )}
                </div>
              )}
            </div>
          )
        })}
      </div>
    </div>
  )
}

function Headers({ title, value }: { title: string; value?: Record<string, string> }) {
  const keys = value ? Object.keys(value) : []
  if (keys.length === 0) return null
  return (
    <div style={{ marginTop: 4 }}>
      <div className="micro">{title}</div>
      {keys.map(k => (
        <div key={k} style={{ wordBreak: 'break-all' }}>
          <span className="faint">{k}: </span>{value?.[k]}
        </div>
      ))}
    </div>
  )
}

/** The clipboard form for failed requests. A HAR is the right artefact for a
 *  whole session; a list of failures is the right one for a bug report, and
 *  pasting 4 MB of HAR to say "these three 500'd" buries it. */
export function failedAsText(entries: NetworkEntry[]): string {
  return entries
    .filter(e => e.failed === true || (e.status !== undefined && e.status >= 400))
    .map(e => `${e.method} ${e.status ?? 'ERR'} ${e.url}${e.errorText ? ` — ${e.errorText}` : ''}`)
    .join('\n')
}
