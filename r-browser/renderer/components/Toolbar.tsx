import { useEffect, useRef, useState } from 'react'
import type { Suggestion, TabState } from '@shared/contract'
import { Menu, MenuDivider, MenuItem, OutcomeLabel } from './Bits'
import {
  IconBack, IconCamera, IconClock, IconCode, IconCopy, IconForward, IconPanel, IconPick,
  IconReload, IconStar, IconStarFilled, IconStop,
} from './Icons'
import { normaliseUrl } from '@shared/url'
import { useOutcome } from '../lib/hooks'

/** How long to wait after a keystroke before asking main for suggestions. One
 *  IPC round trip per character typed fast is wasteful and makes the list
 *  flicker; 90ms is below the threshold where it reads as lag. */
const SUGGEST_DEBOUNCE = 90

export interface CopyActions {
  console: () => Promise<string>
  errors: () => Promise<string>
  har: () => Promise<string>
  failed: () => Promise<string>
  url: () => Promise<string>
  prompt: () => Promise<string>
}

export function Toolbar({
  tab, picking, multi, panelOpen, deviceBar, addressFocusToken,
  onNavigate, onBack, onForward, onReload, onStop, onDevtools,
  onPick, onMulti, onShot, onTogglePanel, onDeviceBar, onClearData, copy,
}: {
  tab: TabState | null
  picking: boolean
  multi: boolean
  panelOpen: boolean
  deviceBar: boolean
  /** Bumped by the `focus-address` accelerator. Main owns the key, so the only
   *  thing that crosses into this component is "it happened, again". */
  addressFocusToken: number
  onNavigate: (url: string) => void
  onBack: () => void
  onForward: () => void
  onReload: (hard: boolean) => void
  onStop: () => void
  onDevtools: () => void
  onPick: () => void
  onMulti: (v: boolean) => void
  onShot: (kind: 'viewport' | 'full' | 'element') => void
  onTogglePanel: () => void
  onDeviceBar: (v: boolean) => void
  onClearData: (scope: 'cache' | 'all') => Promise<void>
  copy: CopyActions
}) {
  const input = useRef<HTMLInputElement | null>(null)
  const [draft, setDraft] = useState(tab?.url ?? '')
  const [editing, setEditing] = useState(false)
  const { outcome, run } = useOutcome()

  // Address-bar suggestions. `open` is "the omnibox is focused"; the strip shows
  // whenever there is at least one match. `active` is the keyboard-highlighted
  // row, −1 meaning "none, Enter uses what was typed".
  const [suggestions, setSuggestions] = useState<Suggestion[]>([])
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(-1)
  const blurTimer = useRef<number | null>(null)

  // Whether the current page is bookmarked, so the star can show its state. Read
  // from main on every tab/url change — the star is a view of the store, never a
  // guess the toolbar keeps on its own.
  const [bookmarked, setBookmarked] = useState(false)
  useEffect(() => {
    let live = true
    if (!tab?.url) { setBookmarked(false); return }
    void window.rbrowse.bookmarks.has(tab.url).then(b => { if (live) setBookmarked(b) })
    return () => { live = false }
  }, [tab?.url])

  const fetchSuggestions = (query: string) => {
    void window.rbrowse.history.suggest(query).then(setSuggestions)
  }

  // Debounced refresh while the omnibox is open. The first open (focus) fetches
  // immediately for a snappy list; subsequent keystrokes debounce.
  useEffect(() => {
    if (!open) return
    const t = window.setTimeout(() => fetchSuggestions(draft), SUGGEST_DEBOUNCE)
    return () => window.clearTimeout(t)
  }, [draft, open])

  useEffect(() => () => { if (blurTimer.current !== null) window.clearTimeout(blurTimer.current) }, [])

  /*
   * The address bar shows `tab.url` — but **never rewrites itself under a
   * half-typed URL**. A page that redirects, or a `pushState` route change,
   * fires a tab update every few hundred milliseconds, and a naive `value={url}`
   * deletes what the operator is in the middle of typing. So the field is
   * uncontrolled-by-the-tab while it is focused, and re-syncs on blur.
   */
  useEffect(() => {
    if (!editing) setDraft(tab?.url ?? '')
  }, [tab?.url, tab?.id, editing])

  useEffect(() => {
    if (addressFocusToken === 0) return
    input.current?.focus()
    input.current?.select()
  }, [addressFocusToken])

  if (!tab) {
    // Nothing to steer. Rather than a row of dead arrows, the toolbar reduces to
    // the one control that still works — rule: absent, not disabled-and-greyed.
    return (
      <div className="toolbar">
        <span className="micro">no tab open</span>
      </div>
    )
  }

  const closeOmni = () => { setOpen(false); setActive(-1) }

  const navigateTo = (raw: string) => {
    const url = normaliseUrl(raw)
    if (!url) return
    setEditing(false)
    closeOmni()
    input.current?.blur()
    onNavigate(url)
  }

  const go = () => navigateTo(draft)

  const toggleBookmark = () => {
    if (!tab?.url) return
    const next = !bookmarked
    setBookmarked(next)
    if (next) void window.rbrowse.bookmarks.add(tab.url, tab.title)
    else void window.rbrowse.bookmarks.remove(tab.url)
  }

  return (
    <>
    <div className="toolbar">
      <div className="toolbar-nav">
        {/* Absent, not disabled: a greyed arrow invites "how do I enable this",
            a missing one asks nothing. */}
        {tab.canGoBack && (
          <button type="button" className="icon-btn" title="back" onClick={onBack}>
            <IconBack />
          </button>
        )}
        {tab.canGoForward && (
          <button type="button" className="icon-btn" title="forward" onClick={onForward}>
            <IconForward />
          </button>
        )}
        {tab.loading ? (
          <button type="button" className="icon-btn" title="stop" onClick={onStop}>
            <IconStop />
          </button>
        ) : (
          <button
            type="button"
            className="icon-btn"
            title="reload — hold ⌥ to bypass the cache"
            onClick={e => onReload(e.altKey)}
          >
            <IconReload />
          </button>
        )}
      </div>

      <div className="address" data-error={tab.error ? 'true' : undefined}>
        <input
          ref={input}
          spellCheck={false}
          autoComplete="off"
          value={draft}
          placeholder="url, or a word to search"
          onChange={e => { setDraft(e.target.value); setActive(-1) }}
          onFocus={() => { setEditing(true); input.current?.select(); setOpen(true); fetchSuggestions(draft) }}
          onBlur={() => {
            setEditing(false)
            setDraft(tab.url)
            // Close after the click on a suggestion has had a chance to land —
            // a suggestion's mousedown fires before this blur, but its click
            // fires just after, so tearing the list down immediately would eat
            // the selection.
            blurTimer.current = window.setTimeout(closeOmni, 150)
          }}
          onKeyDown={e => {
            if (e.key === 'ArrowDown' && suggestions.length) {
              e.preventDefault()
              setActive(i => (i + 1) % suggestions.length)
            } else if (e.key === 'ArrowUp' && suggestions.length) {
              e.preventDefault()
              setActive(i => (i <= 0 ? suggestions.length - 1 : i - 1))
            } else if (e.key === 'Enter') {
              if (active >= 0 && suggestions[active]) navigateTo(suggestions[active].url)
              else go()
            } else if (e.key === 'Escape') {
              // First Escape closes the suggestion list; once it is closed, a
              // second restores the loaded URL — the only way out of a
              // half-typed address without retyping it.
              if (open) closeOmni()
              else { setDraft(tab.url); input.current?.blur() }
            }
          }}
        />
        {tab.error && <span className="micro alert" title={tab.error}>failed</span>}
        <button
          type="button"
          className="icon-btn address-star"
          aria-pressed={bookmarked}
          title={bookmarked ? 'remove bookmark' : 'bookmark this page'}
          // mousedown, not click: the address input's blur would otherwise fire
          // first and the button press would land on a toolbar mid-reflow.
          onMouseDown={e => { e.preventDefault(); toggleBookmark() }}
        >
          {bookmarked ? <IconStarFilled size={13} /> : <IconStar size={13} />}
        </button>
      </div>

      <div className="toolbar-actions">
        {/*
          PICK armed is the one place a persistent red belongs: the app is
          waiting on the operator to point at something. It reverts the instant
          the picker disarms — see rule 0.
        */}
        <button
          type="button"
          className={picking ? 'chip armed' : 'chip'}
          aria-pressed={picking}
          title={picking ? 'picking — click an element in the page, or press Escape' : 'pick an element'}
          onClick={onPick}
        >
          <IconPick size={11} />
          pick
        </button>
        <button
          type="button"
          className="chip"
          aria-pressed={multi}
          title="stay armed after each pick"
          onClick={() => onMulti(!multi)}
        >
          multi
        </button>

        <Menu label="shot" icon={<IconCamera size={11} />} title="screenshot">
          {close => (
            <>
              <MenuItem label="viewport" onClick={() => { close(); onShot('viewport') }} />
              <MenuItem label="full page" onClick={() => { close(); onShot('full') }} />
              <MenuItem
                label="element…"
                hint="pick"
                onClick={() => { close(); onShot('element') }}
              />
              {/*
                There is no REGION here, and its absence is deliberate rather
                than unfinished. A region has to be *drawn*, and the page is a
                native view painted over the slot — a marquee drawn in the
                chrome's DOM would be both invisible and unclickable, because
                there is no chrome pixel over the page to drag on. Offering it
                would be a control that cannot work; element capture is the
                gesture that does, and it goes through the picker.
              */}
            </>
          )}
        </Menu>

        <button
          type="button"
          className="chip"
          aria-pressed={tab.devtoolsOpen}
          title="devtools — while it is open the debugger is theirs, not ours"
          onClick={onDevtools}
        >
          <IconCode size={11} />
          devtools
        </button>

        <Menu label="cache" icon={<IconReload size={11} />}>
          {close => (
            <>
              <MenuItem
                label="clear cache"
                onClick={() => {
                  close()
                  void run('clearing cache', async () => {
                    await onClearData('cache')
                    return 'cache cleared'
                  })
                }}
              />
              <MenuItem
                label="clear cookies & site data"
                onClick={() => {
                  close()
                  void run('clearing site data', async () => {
                    await onClearData('all')
                    return 'cookies & data cleared'
                  })
                }}
              />
            </>
          )}
        </Menu>

        <Menu label="copy" icon={<IconCopy size={11} />}>
          {close => (
            <>
              <MenuItem label="console" onClick={() => { close(); void run('copying', copy.console) }} />
              <MenuItem label="errors only" onClick={() => { close(); void run('copying', copy.errors) }} />
              <MenuDivider />
              <MenuItem label="network (har)" onClick={() => { close(); void run('copying', copy.har) }} />
              <MenuItem label="failed requests" onClick={() => { close(); void run('copying', copy.failed) }} />
              <MenuDivider />
              <MenuItem label="page url" onClick={() => { close(); void run('copying', copy.url) }} />
              <MenuItem label="prompt from selection" onClick={() => { close(); void run('building', copy.prompt) }} />
            </>
          )}
        </Menu>

        {/* The COPY menu's outcome reports here, beside the control that
            started it — disabling a menu that has already closed says nothing,
            so the label carries the whole report: in flight, then the result. */}
        <OutcomeLabel outcome={outcome} />

        {/*
          The device bar is a mode rather than a permanent row: it costs a line
          of chrome and most sessions never leave the desktop. Its switch lives
          here because this is where the other view toggles are.
        */}
        <button
          type="button"
          className="chip"
          aria-pressed={deviceBar}
          title="device bar"
          onClick={() => onDeviceBar(!deviceBar)}
        >
          device
        </button>

        <button
          type="button"
          className="chip"
          aria-pressed={panelOpen}
          title="inspector"
          onClick={onTogglePanel}
        >
          <IconPanel size={11} />
        </button>
      </div>
    </div>

    {/*
      The suggestion list is a chrome strip, not a floating dropdown.
      The page is a native view painted over the renderer, so anything drawn in
      its rectangle is hidden behind it — a dropdown would be invisible. Sitting
      in the chrome, it shrinks the page slot instead (the slot's own observer
      reports the smaller size to main), exactly as the device and password bars
      do. Rows beyond the fetched cap scroll inside a bounded strip.
    */}
    {open && suggestions.length > 0 && (
      <div className="omni-suggest">
        {suggestions.map((s, i) => (
          <button
            key={`${s.kind}:${s.url}`}
            type="button"
            className={i === active ? 'omni-row active' : 'omni-row'}
            // mousedown fires before the input's blur, so this never loses the
            // click to the list being torn down; preventDefault keeps focus off
            // the button.
            onMouseDown={e => { e.preventDefault(); navigateTo(s.url) }}
            onMouseEnter={() => setActive(i)}
          >
            <span className="omni-icon">
              {s.kind === 'bookmark' ? <IconStarFilled size={12} /> : <IconClock size={12} />}
            </span>
            <span className="omni-main">
              {s.title && <span className="omni-title truncate">{s.title}</span>}
              <span className="omni-url truncate">{s.url}</span>
            </span>
            {s.kind === 'history' && s.visitCount && s.visitCount > 1 && (
              <span className="omni-count micro">{s.visitCount}×</span>
            )}
          </button>
        ))}
      </div>
    )}
    </>
  )
}
