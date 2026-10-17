import { useCallback, useEffect, useRef, useState } from 'react'

/* ------------------------------------------------------------------ layout */

/** What main is told. Integers: a fractional rect makes a native view land on a
 *  half pixel and the page's right column renders a hairline short. */
export interface Rect { x: number; y: number; width: number; height: number }

/**
 * Tell main where the page goes — **once the layout settles, not on every
 * observation.**
 *
 * `ResizeObserver` fires continuously while a window is dragged or the
 * inspector's grip is pulled, and every message main acts on repositions a
 * native view and forces the page to reflow. rmux learned this the expensive
 * way with terminals: a two-second drag told the far side about forty widths,
 * and the page redrew forty times. So observation stays immediate — the rect is
 * captured on every callback, so the *last* one is always the truth — while the
 * message is debounced.
 *
 * The first measurement is sent immediately, because until it arrives main has
 * no idea where the page belongs and the window shows nothing at all.
 */
const SETTLE_MS = 80

export function usePageSlot(
  ref: React.RefObject<HTMLElement | null>,
  onMeasure?: (rect: Rect) => void,
) {
  /* The callback is held in a ref so that a caller passing an inline arrow does
     not tear down and rebuild the `ResizeObserver` on every render — which would
     re-measure, re-send, and defeat the debounce entirely. */
  const report = useRef(onMeasure)
  report.current = onMeasure

  const timer = useRef<number | null>(null)
  /** The most recent measurement, whether or not it has been sent. */
  const last = useRef<Rect | null>(null)
  /** The most recent measurement that *was* sent, so an identical one is not. */
  const lastSent = useRef<Rect | null>(null)
  const sentOnce = useRef(false)

  const send = useCallback((rect: Rect) => {
    // Nothing moved — an identical rect is a message main would act on for no
    // reason, and these arrive in bursts.
    if (sentOnce.current && sameRect(rect, lastSent.current)) return
    lastSent.current = rect
    void window.rbrowse.setLayout(rect)
    sentOnce.current = true
  }, [])

  const measure = useCallback(() => {
    const el = ref.current
    if (!el) return
    const r = el.getBoundingClientRect()
    const rect: Rect = {
      x: Math.round(r.left),
      y: Math.round(r.top),
      width: Math.round(r.width),
      height: Math.round(r.height),
    }
    last.current = rect
    // The *observation* is immediate — the readout in the status strip is the
    // operator watching a drag, and a debounced number there reads as a frozen
    // UI. Only the message to main is deferred.
    report.current?.(rect)

    if (!sentOnce.current) { send(rect); return }

    if (timer.current !== null) window.clearTimeout(timer.current)
    timer.current = window.setTimeout(() => {
      timer.current = null
      if (last.current) send(last.current)
    }, SETTLE_MS)
  }, [ref, send])

  useEffect(() => {
    const el = ref.current
    if (!el) return
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    // A window resize can move the slot without changing its size (the chrome
    // above it is fixed-height), and `ResizeObserver` would say nothing.
    window.addEventListener('resize', measure)
    measure()
    return () => {
      ro.disconnect()
      window.removeEventListener('resize', measure)
      if (timer.current !== null) window.clearTimeout(timer.current)
    }
  }, [ref, measure])

  return measure
}

function sameRect(a: Rect, b: Rect | null): boolean {
  return !!b && a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height
}

/* ----------------------------------------------------------------- outcome */

export type OutcomeKind = 'busy' | 'ok' | 'error'
export interface Outcome { kind: OutcomeKind; text: string }

/**
 * The inline report every mutating control owes the operator.
 *
 * Disabled plus a progress label in flight, then the confirmation or the error.
 * **Errors persist until the next attempt**; successes clear after 2.5s — an
 * operation that can fail must never appear to have succeeded, and the
 * asymmetry is the whole point.
 */
export function useOutcome() {
  const [outcome, setOutcome] = useState<Outcome | null>(null)
  const timer = useRef<number | null>(null)

  const clearTimer = () => {
    if (timer.current !== null) window.clearTimeout(timer.current)
    timer.current = null
  }

  const run = useCallback(async (busy: string, fn: () => Promise<string | void>) => {
    clearTimer()
    setOutcome({ kind: 'busy', text: busy })
    try {
      const done = await fn()
      setOutcome({ kind: 'ok', text: done || 'done' })
      timer.current = window.setTimeout(() => setOutcome(null), 2500)
    } catch (e) {
      setOutcome({ kind: 'error', text: message(e) })
    }
  }, [])

  useEffect(() => clearTimer, [])

  return { outcome, run, busy: outcome?.kind === 'busy' }
}

/** An IPC rejection arrives as an `Error` whose message carries Electron's own
 *  "Error invoking remote method" prefix. Strip it: the operator needs the
 *  reason, and the prefix pushes it off the end of a 9px label. */
export function message(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e)
  return raw.replace(/^Error invoking remote method '[^']*':\s*/, '').slice(0, 160)
}

/* ------------------------------------------------------------ preferences */

/**
 * A small preference that survives a reload, kept in `localStorage`.
 *
 * Only *small closed* values live here — a panel width, which filter chip is
 * lit, whether screenshots go into the prompt. Annotations deliberately do not:
 * they carry base64 screenshots, and `localStorage` is a single shared quota, so
 * a morning's notes would start silently failing to save. Those go through
 * `rbrowse.store`, which main writes to the user-data directory.
 */
export function usePref<T>(key: string, fallback: T): [T, (v: T) => void] {
  const [value, setValue] = useState<T>(() => {
    try {
      const raw = window.localStorage.getItem(`rb.${key}`)
      return raw === null ? fallback : (JSON.parse(raw) as T)
    } catch {
      // A quota error, a private window, or a value written by an older build
      // that no longer parses. None of them are worth a broken panel.
      return fallback
    }
  })

  const set = useCallback((v: T) => {
    setValue(v)
    try {
      window.localStorage.setItem(`rb.${key}`, JSON.stringify(v))
    } catch { /* see above */ }
  }, [key])

  return [value, set]
}
