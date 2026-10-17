import { useCallback, useEffect, useRef, type ReactNode } from 'react'
import { Seg } from './Bits'
import { IconClose } from './Icons'

export type InspectorTab = 'notes' | 'console' | 'network' | 'passwords'

/** Below this the panel cannot show a note's summary line, and above it the page
 *  stops being a page. Both ends are clamps rather than refusals — a drag that
 *  simply stops is readable; one that snaps back is not. */
const MIN = 240
const MAX_FRACTION = 0.72

/**
 * The right-hand inspector.
 *
 * Resizing it shrinks the page slot, which fires the slot's `ResizeObserver`,
 * which debounces a `setLayout` — so the page follows the grip once the drag
 * settles rather than reflowing on every pointer move. That chain is the whole
 * reason the panel is a sibling of the slot rather than an overlay on top of it.
 */
export function Inspector({
  tab, width, counts, onTab, onWidth, onClose, children,
}: {
  tab: InspectorTab
  width: number
  counts: { notes: number; errors: number; requests: number }
  onTab: (t: InspectorTab) => void
  onWidth: (w: number) => void
  onClose: () => void
  children: ReactNode
}) {
  const dragging = useRef(false)

  const move = useCallback((e: PointerEvent) => {
    if (!dragging.current) return
    const next = Math.round(window.innerWidth - e.clientX)
    onWidth(Math.max(MIN, Math.min(next, Math.round(window.innerWidth * MAX_FRACTION))))
  }, [onWidth])

  useEffect(() => {
    const up = () => { dragging.current = false }
    // Listeners live on the window for the whole life of the panel rather than
    // being attached per drag: a pointer released outside the window never
    // delivers an `up` to the element, and the panel would then follow the
    // cursor forever with no button held.
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
    window.addEventListener('pointercancel', up)
    return () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
      window.removeEventListener('pointercancel', up)
    }
  }, [move])

  return (
    <>
      <div
        className="inspector-grip"
        title="drag to resize"
        onPointerDown={e => { dragging.current = true; e.preventDefault() }}
      />
      <div className="inspector" style={{ width }}>
        <div className="inspector-head">
          <Seg
            value={tab}
            options={[
              { value: 'notes', label: `notes ${counts.notes}` },
              { value: 'console', label: counts.errors ? `console ${counts.errors}` : 'console' },
              { value: 'network', label: `network ${counts.requests}` },
              { value: 'passwords', label: 'passwords' },
            ]}
            onChange={onTab}
          />
          <button type="button" className="icon-btn" title="close inspector" onClick={onClose}>
            <IconClose size={12} />
          </button>
        </div>
        {children}
      </div>
    </>
  )
}
