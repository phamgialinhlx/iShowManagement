/** Small formatters. Every one of these renders into a `.data` element, so they
 *  all return fixed-ish widths — a column of numbers that changes width as it
 *  updates cannot be compared at a glance. */

export function bytes(n: number | undefined): string {
  if (n === undefined || !Number.isFinite(n)) return '—'
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} kB`
  return `${(n / (1024 * 1024)).toFixed(1)} MB`
}

export function millis(n: number | undefined): string {
  if (n === undefined || !Number.isFinite(n)) return '—'
  if (n < 1000) return `${Math.round(n)} ms`
  return `${(n / 1000).toFixed(2)} s`
}

/** Wall clock, not an age. An age has to be recomputed on a timer to stay true,
 *  and a console line's value is *when it happened* relative to the other lines
 *  — which a fixed stamp gives for free and a ticking one obscures. */
export function clock(at: number): string {
  const d = new Date(at)
  const p = (v: number, w = 2) => String(v).padStart(w, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`
}
