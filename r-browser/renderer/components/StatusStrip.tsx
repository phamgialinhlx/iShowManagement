import { useEffect, useRef, useState } from 'react'
import type { CaptureState, ControlState, TabState } from '@shared/contract'
import { OutcomeLabel } from './Bits'
import { IconSend } from './Icons'
import { useOutcome } from '../lib/hooks'

/**
 * The bottom strip: rmux, capture, viewport.
 *
 * All three are answers to "is what I am looking at trustworthy right now" —
 * whether notes can leave the app, whether the console is really listening, and
 * what size the page believes it is. None of them is an alarm, so none of them
 * is red.
 */
export function StatusStrip({
  control, capture, tab, slot, selectedCount, onConnect, onReport, onActivate,
}: {
  control: ControlState
  capture: CaptureState | null
  tab: TabState | null
  slot: { width: number; height: number }
  selectedCount: number
  onConnect: () => Promise<void>
  onReport: (sessionId: string) => Promise<void>
  onActivate: (sessionId: string) => Promise<void>
}) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement | null>(null)
  const { outcome, run, busy } = useOutcome()
  const [target, setTarget] = useState<string | null>(control.activeSessionId ?? null)

  useEffect(() => {
    if (control.activeSessionId) setTarget(control.activeSessionId)
  }, [control.activeSessionId])

  useEffect(() => {
    if (!open) return
    const away = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', away)
    return () => document.removeEventListener('mousedown', away)
  }, [open])

  return (
    <div className="status">
      <div style={{ position: 'relative' }} ref={box}>
        <button
          type="button"
          className="chip"
          aria-pressed={open}
          onClick={() => setOpen(v => !v)}
          title="rmux"
        >
          <span className="dot" data-on={control.connected ? 'true' : undefined} />
          rmux {control.connected ? `· ${control.sessions.length}` : '· off'}
        </button>

        {open && (
          <div className="popover">
            {!control.connected ? (
              <>
                {/*
                  rmux being closed degrades one panel, never the app — so this
                  says what still works rather than presenting a failure. There
                  is nothing else here on purpose: a session list, a target
                  picker and a SEND button that could not work would all be
                  controls that cannot work.
                */}
                <span className="micro">rmux not running — notes stay local</span>
                {control.error && <span className="micro faint">{control.error}</span>}
                <div className="row">
                  <button
                    type="button"
                    className="chip"
                    disabled={busy}
                    onClick={() => void run('connecting', async () => {
                      await onConnect()
                      return 'asked'
                    })}
                  >
                    retry
                  </button>
                  <OutcomeLabel outcome={outcome} />
                </div>
              </>
            ) : (
              <>
                <span className="micro">
                  {control.app ?? 'rmux'}{control.version ? ` · v${control.version}` : ''}
                </span>
                {control.sessions.length === 0 && (
                  <span className="micro faint">no sessions open in rmux</span>
                )}
                {control.sessions.map(s => (
                  <button
                    key={s.id}
                    type="button"
                    className="session-row"
                    aria-pressed={target === s.id}
                    onClick={() => {
                      setTarget(s.id)
                      void onActivate(s.id)
                    }}
                  >
                    <span className="col" style={{ gap: 2, minWidth: 0 }}>
                      <span className="truncate" style={{ fontSize: 11 }}>{s.name}</span>
                      <span className="micro faint truncate-left">{s.host ? `${s.host}:` : ''}{s.folder}</span>
                    </span>
                  </button>
                ))}

                <div className="row">
                  <button
                    type="button"
                    className="btn btn-primary"
                    disabled={busy || !target || selectedCount === 0}
                    onClick={() => void run('sending', async () => {
                      if (!target) return
                      await onReport(target)
                      return `filed · ${selectedCount} notes`
                    })}
                  >
                    <IconSend size={11} /> send to session
                  </button>
                  <OutcomeLabel outcome={outcome} />
                </div>
                {/*
                  Said plainly, because it is the difference between a tool and a
                  prompt-injection path: a report describes a page rmux did not
                  write, so it is *typed into the composer* and left for the
                  operator. Nothing here submits anything to an agent.
                */}
                <span className="micro faint">
                  typed into the session composer for you to send — never submitted
                </span>
              </>
            )}
          </div>
        )}
      </div>

      {/* Capture, stated rather than implied. `fallback` is amber: it is a
          degraded state the operator should know about, not one to act on. */}
      <span className={capture && capture.console !== 'cdp' ? 'micro busy' : 'micro'}>
        capture {capture ? `${capture.console} · net ${capture.network}` : '—'}
      </span>

      <div className="status-spacer" />

      <span className="micro data">
        {tab ? `${slot.width} × ${slot.height}` : '—'}
      </span>
    </div>
  )
}
