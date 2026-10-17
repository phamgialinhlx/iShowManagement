import type { TabState } from '@shared/contract'
import { IconClose, IconPlus } from './Icons'
import { hostOf } from '../lib/url'

/**
 * The tab strip.
 *
 * Horizontally scrolled rather than shrink-to-fit: tabs that keep narrowing end
 * up as a row of identical favicons with no titles, which is a strip that can no
 * longer answer the only question it is asked. A fixed width plus a scrollbar
 * keeps every tab readable and says honestly that there are more.
 */
export function TabStrip({
  tabs, activeId, onActivate, onClose, onNew,
}: {
  tabs: TabState[]
  activeId: string | null
  onActivate: (id: string) => void
  onClose: (id: string) => void
  onNew: () => void
}) {
  return (
    /*
     * The strip is wrapped, and the wrapper is what drags the window.
     *
     * `titleBarStyle: 'hiddenInset'` removes the native title bar, so on macOS
     * **nothing** moves the window unless the page declares a drag region. This
     * shipped without one: the window could not be moved at all, and the 78px
     * inset left for the traffic lights read as a title bar that did nothing.
     * Same failure rmux had when `core:default` omitted `allow-start-dragging`.
     *
     * The drag areas sit *outside* the scrolling strip on purpose. Put inside,
     * a trailing spacer scrolls away the moment there are enough tabs to
     * overflow — so the one case where the window is hardest to grab is exactly
     * the case where the handle disappears.
     */
    <div className="tabbar">
      <div className="tabbar-inset" aria-hidden="true" />
      <div className="tabstrip" role="tablist">
        {tabs.map(tab => (
          <div
            key={tab.id}
            role="tab"
            aria-selected={tab.id === activeId}
            className="tab"
            title={tab.title || tab.url}
            onMouseDown={e => {
              // Middle click closes, as in every other browser. `mousedown`
              // because the platform does not deliver a `click` for button 1.
              if (e.button === 1) { e.preventDefault(); onClose(tab.id) }
              else if (e.button === 0) onActivate(tab.id)
            }}
          >
            {tab.favicon
              ? <img className="tab-favicon" src={tab.favicon} alt="" />
              /* A favicon that has not arrived leaves a box of the same size, or
                 every title shifts sideways the moment one loads. */
              : <span className="tab-favicon" />}
            <span className="tab-title">{tab.title || hostOf(tab.url) || 'new tab'}</span>
            <button
              type="button"
              className="icon-btn tab-close"
              title="close tab"
              style={{ width: 16, height: 16 }}
              onMouseDown={e => e.stopPropagation()}
              onClick={e => { e.stopPropagation(); onClose(tab.id) }}
            >
              <IconClose size={11} />
            </button>
            {/* Liveness by movement, never by a pulsing dot — rule 2. */}
            {tab.loading && <span className="tab-loading" />}
          </div>
        ))}
        <button type="button" className="tab-new" title="new tab" onClick={onNew}>
          <IconPlus size={13} />
        </button>
      </div>
      <div className="tabbar-drag" aria-hidden="true" />
    </div>
  )
}
