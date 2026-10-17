/**
 * The three controls that would otherwise be re-invented in five panels.
 *
 * rmux learned this with `PanelLoader`: a component that exists but is awkward
 * to reuse gets copied instead, and by the time anyone notices there are seven
 * vocabularies for "loading". So the shared pieces live here and are small
 * enough to reach for.
 */
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { IconChevron } from './Icons'
import type { Outcome } from '../lib/hooks'

/* ----------------------------------------------------------------- outcome */

/** The inline report beside a mutating control. Absent when there is nothing to
 *  say — an empty element would reserve space that jumps when it fills. */
export function OutcomeLabel({ outcome }: { outcome: Outcome | null }) {
  if (!outcome) return null
  return <span className="outcome" data-kind={outcome.kind}>{outcome.text}</span>
}

/* --------------------------------------------------------------- segmented */

/**
 * A segmented control, for a *small closed* choice.
 *
 * Never a `<select>` for these: a dropdown hides the options **and** the current
 * value behind a click, and state must be readable without clicking. The
 * selected chip carries an underline as well as a tone, because at 9px a
 * tone-only difference is not a state anyone can see.
 */
export function Seg<T extends string>({
  value, options, onChange, title,
}: {
  value: T
  options: ReadonlyArray<{ value: T; label: string }>
  onChange: (v: T) => void
  title?: string
}) {
  return (
    <div className="seg" title={title}>
      {options.map(o => (
        <button
          key={o.value}
          type="button"
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  )
}

/* -------------------------------------------------------------------- menu */

/**
 * A dropdown that closes on outside click and on Escape.
 *
 * `mousedown` rather than `click` for the outside test: a `click` listener fires
 * after the target's own handler, so choosing an item in one menu while another
 * is open would run the item and *then* decide the menu was still open.
 */
export function Menu({
  label, children, icon, title,
}: {
  label: string
  children: (close: () => void) => ReactNode
  icon?: ReactNode
  title?: string
}) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    if (!open) return
    const away = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false)
    }
    const esc = (e: KeyboardEvent) => { if (e.key === 'Escape') setOpen(false) }
    document.addEventListener('mousedown', away)
    document.addEventListener('keydown', esc)
    return () => {
      document.removeEventListener('mousedown', away)
      document.removeEventListener('keydown', esc)
    }
  }, [open])

  return (
    <div className="menu-anchor" ref={box}>
      <button
        type="button"
        className="chip"
        aria-pressed={open}
        title={title}
        onClick={() => setOpen(v => !v)}
      >
        {icon}
        {label}
        <IconChevron size={10} />
      </button>
      {open && <div className="menu">{children(() => setOpen(false))}</div>}
    </div>
  )
}

export function MenuItem({
  label, hint, onClick,
}: { label: string; hint?: string; onClick: () => void }) {
  return (
    <button type="button" className="menu-item" onClick={onClick}>
      <span>{label}</span>
      {hint && <span className="faint data">{hint}</span>}
    </button>
  )
}

export const MenuDivider = () => <div className="menu-divider" />

/* ------------------------------------------------------------------ empty */

/**
 * Never a blank box.
 *
 * Every state that can outlast a frame says *what* it is doing and *from where*
 * — over IPC into a page that may be mid-navigation, "loading" alone is exactly
 * what this component exists not to say.
 */
export function Empty({ phase, detail }: { phase: string; detail?: string }) {
  return (
    <div className="empty">
      <span className="micro">{phase}</span>
      {detail && <span className="micro faint">{detail}</span>}
    </div>
  )
}
