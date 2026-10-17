import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type {
  Annotation, CaptureState, ConsoleEntry, ControlState, CredentialOffer, DevicePreset,
  EmulationState, NetworkEntry, TabState,
} from '@shared/contract'
import { promptFor, promptForSingle, type PromptOptions } from '@shared/prompt'
import { TabStrip } from './components/TabStrip'
import { Toolbar, type CopyActions } from './components/Toolbar'
import { DeviceBar } from './components/DeviceBar'
import { Inspector, type InspectorTab } from './components/Inspector'
import { NotesPanel } from './components/NotesPanel'
import { ConsolePanel, asText, bucket } from './components/ConsolePanel'
import { NetworkPanel, failedAsText } from './components/NetworkPanel'
import { StatusStrip } from './components/StatusStrip'
import { SavePasswordBar } from './components/SavePasswordBar'
import { PasswordsPanel } from './components/PasswordsPanel'
import { Empty } from './components/Bits'
import { usePageSlot, usePref, type Rect } from './lib/hooks'

/**
 * What the three checkboxes beside COPY PROMPT start as.
 *
 * `PromptOptions` is `@shared/prompt`'s own type rather than a renderer copy —
 * a second definition of "what goes in a prompt" is a second place for the
 * screenshot rule to be got wrong. Only the booleans are persisted; the
 * `console` and `network` arrays are filled at the moment the prompt is built,
 * because they are the *current* tab's capture and stale five minutes later.
 */
const DEFAULT_PROMPT_OPTIONS: PromptOptions = {
  includeScreenshots: true,
  includeConsole: true,
  includeNetwork: false,
}

/** How much console history travels with a pick. Captured *at* the pick, never
 *  read later: by the time a prompt is generated the page has usually been
 *  reloaded and the errors that prompted the note are gone. */
const CONSOLE_TAIL = 25

/** A tab that has never had its emulation touched. Kept as a constant so the
 *  device bar has something to edit before main has answered with a real one. */
const NEUTRAL: EmulationState = {
  deviceId: null,
  rotated: false,
  colorScheme: 'auto',
  reducedMotion: false,
  forcedColors: false,
  zoom: 0,
}

export default function App() {
  const [tabs, setTabs] = useState<TabState[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const [devices, setDevices] = useState<DevicePreset[]>([])

  const [annotations, setAnnotations] = useState<Annotation[]>([])
  const [loaded, setLoaded] = useState(false)
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set())
  const [draftId, setDraftId] = useState<string | null>(null)
  const [stale, setStale] = useState<ReadonlySet<string>>(new Set())

  const [logs, setLogs] = useState<Record<string, ConsoleEntry[]>>({})
  const [net, setNet] = useState<Record<string, NetworkEntry[]>>({})
  const [captures, setCaptures] = useState<Record<string, CaptureState>>({})
  const [control, setControl] = useState<ControlState>({ connected: false, sessions: [] })

  const [offer, setOffer] = useState<CredentialOffer | null>(null)

  const [picking, setPicking] = useState(false)
  const [multi, setMulti] = usePref('pick.multi', false)
  const [emuLocal, setEmuLocal] = useState<Record<string, EmulationState>>({})

  const [panelOpen, setPanelOpen] = usePref('panel.open', true)
  const [panelTab, setPanelTab] = usePref<InspectorTab>('panel.tab', 'notes')
  const [panelWidth, setPanelWidth] = usePref('panel.width', 380)
  const [deviceBar, setDeviceBar] = usePref('devicebar', false)
  const [promptOptions, setPromptOptions] = usePref<PromptOptions>('prompt.options', DEFAULT_PROMPT_OPTIONS)

  const [addressFocusToken, setAddressFocusToken] = useState(0)
  const [slot, setSlot] = useState({ width: 0, height: 0 })

  const slotRef = useRef<HTMLDivElement | null>(null)
  const onMeasure = useCallback((r: Rect) => setSlot({ width: r.width, height: r.height }), [])
  const remeasureSlot = usePageSlot(slotRef, onMeasure)

  const tab = useMemo(() => tabs.find(t => t.id === activeId) ?? null, [tabs, activeId])
  const tabId = tab?.id ?? null

  /*
   * Handlers reach for the *current* tab through a ref rather than closing over
   * it. Every subscription below is registered once, and a closure over `tab`
   * would pin the tab that happened to be active when the effect ran — so a pick
   * made in the second tab would be filed against the first. That is the kind of
   * bug that only appears after the app has been open for a while.
   */
  const tabRef = useRef<TabState | null>(null)
  tabRef.current = tab
  const logsRef = useRef(logs)
  logsRef.current = logs
  const devicesRef = useRef(devices)
  devicesRef.current = devices
  const multiRef = useRef(multi)
  multiRef.current = multi
  const optionsRef = useRef(promptOptions)
  optionsRef.current = promptOptions

  /* ------------------------------------------------------------------ boot */

  useEffect(() => {
    void (async () => {
      const [list, presets, stored, state] = await Promise.all([
        window.rbrowse.tabs.list(),
        window.rbrowse.emulation.devices(),
        window.rbrowse.store.get(),
        window.rbrowse.control.status(),
      ])
      setTabs(list)
      setActiveId(prev => prev ?? list[0]?.id ?? null)
      setDevices(presets)
      setAnnotations(stored)
      setControl(state)
      setLoaded(true)
    })()
  }, [])

  useEffect(() => {
    const offs = [
      window.rbrowse.tabs.onChanged(next => {
        setTabs(next)
        setActiveId(prev => (prev && next.some(t => t.id === prev) ? prev : next[0]?.id ?? null))
        // Drop an optimistic emulation once main reports the same thing — see
        // the note on `emuLocal` below.
        setEmuLocal(local => {
          let changed = false
          const out: Record<string, EmulationState> = {}
          for (const [id, value] of Object.entries(local)) {
            const real = next.find(t => t.id === id)
            if (real && JSON.stringify(real.emulation) === JSON.stringify(value)) { changed = true; continue }
            out[id] = value
          }
          return changed ? out : local
        })
      }),
      window.rbrowse.console.onEntry(e => {
        const { tabId: id, ...entry } = e
        setLogs(prev => ({ ...prev, [id]: [...(prev[id] ?? []), entry] }))
      }),
      window.rbrowse.network.onEntry(e => {
        const { tabId: id, ...entry } = e
        setNet(prev => {
          const list = prev[id] ?? []
          // A request is described by several CDP events, so an entry that
          // arrives twice is an *update*, not a duplicate row.
          const at = list.findIndex(x => x.id === entry.id)
          const next = at === -1 ? [...list, entry] : list.map((x, i) => (i === at ? { ...x, ...entry } : x))
          return { ...prev, [id]: next }
        })
      }),
      window.rbrowse.capture.onState(s => setCaptures(prev => ({ ...prev, [s.tabId]: s }))),
      window.rbrowse.control.onState(setControl),
      window.rbrowse.onOpenUrl(({ url }) => { void window.rbrowse.tabs.open(url) }),
      window.rbrowse.pick.onCancelled(() => setPicking(false)),
      window.rbrowse.passwords.onOffer(o => setOffer(o)),
      window.rbrowse.pick.onPicked(({ tabId: id, element, note: text, edits, screenshot, screenshotPath, viewport }) => {
        const owner = tabRef.current
        const preset = devicesRef.current.find(d => d.id === owner?.emulation.deviceId)
        const note: Annotation = {
          id: newId(),
          at: Date.now(),
          url: owner?.url ?? '',
          title: owner?.title,
          element,
          // The operator typed the note in the in-page form; it arrives filled.
          note: text ?? '',
          edits: edits && edits.length ? edits : undefined,
          screenshot,
          screenshotPath,
          viewport,
          device: preset?.label,
          colorScheme: owner?.emulation.colorScheme,
          consoleTail: (logsRef.current[id] ?? []).slice(-CONSOLE_TAIL),
        }
        setAnnotations(prev => [...prev, note])
        // A new note is selected: the operator just made it, and having to tick
        // it before COPY PROMPT would work is a step nobody would guess at.
        setSelected(prev => new Set(prev).add(note.id))
        // Only drop into the list's edit field when nothing was typed in-page —
        // otherwise the note is already written and re-opening it for editing
        // moves the cursor away from where the operator's attention is.
        if (!note.note) setDraftId(note.id)
        setPanelOpen(true)
        setPanelTab('notes')
        // Multi keeps the picker armed on the *page* side; single-shot mode is
        // over, so the button must stop being red.
        if (!multiRef.current) setPicking(false)
      }),
    ]
    return () => { for (const off of offs) off() }
  }, [setPanelOpen, setPanelTab])

  /* Seed the panels when the active tab changes. Live events only carry what
     happened since we subscribed; a tab opened before this window existed
     already has a backlog, and an empty console beside a loaded page reads as
     capture being broken. */
  useEffect(() => {
    if (!tabId) return
    let live = true
    void (async () => {
      const [c, n] = await Promise.all([
        window.rbrowse.console.get(tabId),
        window.rbrowse.network.get(tabId),
      ])
      if (!live) return
      setLogs(prev => ({ ...prev, [tabId]: c }))
      setNet(prev => ({ ...prev, [tabId]: n }))
    })()
    return () => { live = false }
  }, [tabId])

  /* Annotations persist through main, debounced: a pick is followed by a
     keystroke every few hundred milliseconds while the note is typed, and
     writing a file (with base64 screenshots in it) on each one is a stutter in
     the one flow that has to stay fast. */
  useEffect(() => {
    if (!loaded) return
    const t = window.setTimeout(() => { void window.rbrowse.store.set(annotations) }, 400)
    return () => window.clearTimeout(t)
  }, [annotations, loaded])

  /* --------------------------------------------------------------- actions */

  const entries = tabId ? logs[tabId] ?? [] : []
  const requests = tabId ? net[tabId] ?? [] : []
  const capture = tabId ? captures[tabId] ?? null : null
  const emulation = tab ? emuLocal[tab.id] ?? tab.emulation ?? NEUTRAL : NEUTRAL

  const selectedNotes = useMemo(
    () => annotations.filter(a => selected.has(a.id)),
    [annotations, selected],
  )

  const startPick = useCallback((shoot: boolean, forceMulti?: boolean) => {
    const id = tabRef.current?.id
    if (!id) return
    const wantMulti = forceMulti ?? multiRef.current
    if (forceMulti !== undefined) setMulti(forceMulti)
    setPicking(true)
    void window.rbrowse.pick.start(id, { multi: wantMulti, shoot })
  }, [setMulti])

  const togglePick = useCallback(() => {
    const id = tabRef.current?.id
    if (!id) return
    if (picking) { setPicking(false); void window.rbrowse.pick.stop(id) }
    else startPick(optionsRef.current.includeScreenshots)
  }, [picking, startPick])

  const shoot = useCallback(async (kind: 'viewport' | 'full' | 'element') => {
    const owner = tabRef.current
    if (!owner) return
    if (kind === 'element') { startPick(true); return }
    const png = kind === 'full'
      ? await window.rbrowse.capture.fullPage(owner.id)
      : await window.rbrowse.capture.viewport(owner.id)
    // A shot with no element is a note about the page as a whole, which
    // `Annotation.element` is explicitly allowed to be absent for.
    const note: Annotation = {
      id: newId(),
      at: Date.now(),
      url: owner.url,
      title: owner.title,
      note: '',
      screenshot: png,
      viewport: { width: slot.width, height: slot.height },
      colorScheme: owner.emulation?.colorScheme,
      consoleTail: (logsRef.current[owner.id] ?? []).slice(-CONSOLE_TAIL),
    }
    setAnnotations(prev => [...prev, note])
    setSelected(prev => new Set(prev).add(note.id))
    setDraftId(note.id)
    setPanelOpen(true)
    setPanelTab('notes')
  }, [slot.width, slot.height, startPick, setPanelOpen, setPanelTab])

  const copyPrompt = useCallback(async () => {
    // The capture travels *with* the options rather than being read inside
    // `promptFor`: the shared module is pure and has no way to reach a tab, and
    // which tab's network log belongs in a prompt is the chrome's question.
    const text = promptFor(selectedNotes, {
      ...promptOptions,
      console: promptOptions.includeConsole ? entries : undefined,
      network: promptOptions.includeNetwork ? requests : undefined,
    })
    await window.rbrowse.clipboard.text(text)
    const kb = (new TextEncoder().encode(text).length / 1024).toFixed(1)
    return `copied · ${selectedNotes.length} notes, ${kb} kB`
  }, [selectedNotes, promptOptions, entries, requests])

  /**
   * One note on its own.
   *
   * Reachable per row because an agent handed several items reliably drops the
   * later ones — measured elsewhere as prompt-level accuracy falling from 0.95
   * at one instruction to 0.48 at ten, and reported by operators as "it only
   * fixed two of my six". The batch is still offered, and the payload carries a
   * checklist to make an omission visible, but one item at a time is the shape
   * that actually lands.
   */
  const copyOne = useCallback(async (a: Annotation) => {
    const text = promptForSingle(a, {
      ...promptOptions,
      console: promptOptions.includeConsole ? entries : undefined,
      network: promptOptions.includeNetwork ? requests : undefined,
    })
    await window.rbrowse.clipboard.text(text)
    return 'copied this one'
  }, [promptOptions, entries, requests])

  const copy: CopyActions = useMemo(() => ({
    console: async () => {
      await window.rbrowse.clipboard.text(asText(entries))
      return `copied · ${entries.length} lines`
    },
    errors: async () => {
      const only = entries.filter(e => bucket(e.level) === 'error')
      await window.rbrowse.clipboard.text(asText(only))
      return `copied · ${only.length} errors`
    },
    har: async () => {
      if (!tabId) return 'no tab'
      const har = await window.rbrowse.network.har(tabId)
      await window.rbrowse.clipboard.text(har)
      return `copied · har ${(har.length / 1024).toFixed(1)} kB`
    },
    failed: async () => {
      const text = failedAsText(requests)
      await window.rbrowse.clipboard.text(text)
      return text ? 'copied · failures' : 'nothing failed'
    },
    url: async () => {
      await window.rbrowse.clipboard.text(tabRef.current?.url ?? '')
      return 'copied · url'
    },
    prompt: copyPrompt,
  }), [entries, requests, tabId, copyPrompt])

  /* ------------------------------------------------------------ shortcuts */

  /*
   * Accelerators are owned by main and arrive here as names.
   *
   * Nothing binds a raw key in this renderer on purpose: the page view holds
   * focus almost all the time, so a `keydown` listener in the chrome would
   * simply never fire — which reads as "the shortcut does not work" rather than
   * as "the chrome is not focused".
   */
  useEffect(() => {
    return window.rbrowse.onShortcut(name => {
      const id = tabRef.current?.id
      switch (name) {
        case 'pick': togglePick(); break
        case 'pick-multi': startPick(optionsRef.current.includeScreenshots, true); break
        case 'shot-viewport': void shoot('viewport'); break
        case 'shot-full': void shoot('full'); break
        case 'devtools': if (id) void window.rbrowse.tabs.devtools(id); break
        case 'focus-address': setAddressFocusToken(v => v + 1); break
        case 'new-tab': void window.rbrowse.tabs.open(); break
        case 'close-tab': if (id) void window.rbrowse.tabs.close(id); break
        case 'copy-prompt': void copyPrompt(); break
        case 'toggle-panel': setPanelOpen(!panelOpen); break
        default: break
      }
    })
  }, [togglePick, startPick, shoot, copyPrompt, panelOpen, setPanelOpen])

  /* ----------------------------------------------------------------- notes */

  const patch = useCallback((id: string, fn: (a: Annotation) => Annotation) => {
    setAnnotations(prev => prev.map(a => (a.id === id ? fn(a) : a)))
  }, [])

  const onRemeasure = useCallback(async (id: string) => {
    const owner = tabRef.current
    const note = annotations.find(a => a.id === id)
    if (!owner || !note?.element) return false
    const found = await window.rbrowse.pick.remeasure(owner.id, note.element)
    setStale(prev => {
      const next = new Set(prev)
      if (found) next.delete(id)
      else next.add(id)
      return next
    })
    // A note whose element has gone is *marked*, never deleted: what the
    // operator wrote is still the useful half, and deleting it would throw away
    // the only record that the element was ever there.
    if (found) patch(id, a => ({ ...a, element: found }))
    return found !== null
  }, [annotations, patch])

  const onReshoot = useCallback(async (id: string) => {
    const owner = tabRef.current
    const note = annotations.find(a => a.id === id)
    if (!owner || !note) return
    const png = note.element
      ? await window.rbrowse.capture.element(owner.id, note.element)
      : await window.rbrowse.capture.viewport(owner.id)
    patch(id, a => ({ ...a, screenshot: png }))
  }, [annotations, patch])

  const onHover = useCallback((id: string | null) => {
    const owner = tabRef.current
    if (!owner) return
    const note = id ? annotations.find(a => a.id === id) : null
    void window.rbrowse.pick.highlight(owner.id, note?.element ?? null)
  }, [annotations])

  /* ---------------------------------------------------------------- render */

  const panel = panelTab === 'passwords' ? (
    // The manager does not need a page open — it is the saved set, not this
    // tab's. So it sits ahead of the `!tabId` gate the page-bound panels take.
    <PasswordsPanel />
  ) : !tabId ? null : panelTab === 'notes' ? (
    <NotesPanel
      annotations={annotations}
      selected={selected}
      draftId={draftId}
      stale={stale}
      options={promptOptions}
      onNote={(id, note) => patch(id, a => ({ ...a, note }))}
      onDelete={id => {
        setAnnotations(prev => prev.filter(a => a.id !== id))
        setSelected(prev => { const n = new Set(prev); n.delete(id); return n })
      }}
      onDeleteMany={ids => {
        const drop = new Set(ids)
        setAnnotations(prev => prev.filter(a => !drop.has(a.id)))
        setSelected(prev => { const n = new Set(prev); for (const id of ids) n.delete(id); return n })
      }}
      onReshoot={onReshoot}
      onRemeasure={onRemeasure}
      onHover={onHover}
      onToggle={id => setSelected(prev => {
        const n = new Set(prev)
        if (n.has(id)) n.delete(id)
        else n.add(id)
        return n
      })}
      onSelectAll={() => setSelected(new Set(annotations.map(a => a.id)))}
      onSelectNone={() => setSelected(new Set())}
      onOptions={setPromptOptions}
      onCopyPrompt={copyPrompt}
      onCopyOne={copyOne}
      onDraftHandled={() => setDraftId(null)}
    />
  ) : panelTab === 'console' ? (
    <ConsolePanel
      entries={entries}
      capture={capture}
      onClear={async () => {
        if (!tabId) return
        await window.rbrowse.console.clear(tabId)
        setLogs(prev => ({ ...prev, [tabId]: [] }))
      }}
      onCopy={text => window.rbrowse.clipboard.text(text)}
    />
  ) : (
    <NetworkPanel
      entries={requests}
      onClear={async () => {
        if (!tabId) return
        await window.rbrowse.network.clear(tabId)
        setNet(prev => ({ ...prev, [tabId]: [] }))
      }}
      onHar={() => window.rbrowse.network.har(tabId)}
      onBody={requestId => window.rbrowse.network.body(tabId, requestId)}
      onCopy={text => window.rbrowse.clipboard.text(text)}
    />
  )

  return (
    <div className="app">
      <div className="chrome">
        <TabStrip
          tabs={tabs}
          activeId={activeId}
          onActivate={id => { setActiveId(id); void window.rbrowse.tabs.activate(id) }}
          onClose={id => { void window.rbrowse.tabs.close(id) }}
          onNew={() => { void window.rbrowse.tabs.open() }}
        />
        <Toolbar
          tab={tab}
          picking={picking}
          multi={multi}
          panelOpen={panelOpen}
          deviceBar={deviceBar}
          addressFocusToken={addressFocusToken}
          onNavigate={url => { if (tabId) void window.rbrowse.tabs.navigate(tabId, url) }}
          onBack={() => { if (tabId) void window.rbrowse.tabs.back(tabId) }}
          onForward={() => { if (tabId) void window.rbrowse.tabs.forward(tabId) }}
          onReload={hard => { if (tabId) void window.rbrowse.tabs.reload(tabId, hard) }}
          onStop={() => { if (tabId) void window.rbrowse.tabs.stop(tabId) }}
          onDevtools={() => { if (tabId) void window.rbrowse.tabs.devtools(tabId) }}
          onClearData={scope => window.rbrowse.data.clear(scope)}
          onPick={togglePick}
          onMulti={setMulti}
          onShot={kind => void shoot(kind)}
          onTogglePanel={() => setPanelOpen(!panelOpen)}
          onDeviceBar={next => {
            setDeviceBar(next)
            // Turning the bar off also returns the page to responsive: leaving
            // an emulation in force with no control on screen is a setting that
            // acts invisibly, which is worse than either state.
            if (!next && tabId) {
              const reset = { ...emulation, deviceId: null }
              setEmuLocal(prev => ({ ...prev, [tabId]: reset }))
              void window.rbrowse.emulation.set(tabId, reset)
            }
          }}
          copy={copy}
        />
        {tab && deviceBar && (
          <DeviceBar
            devices={devices}
            state={emulation}
            slot={slot}
            onChange={next => {
              if (!tabId) return
              setEmuLocal(prev => ({ ...prev, [tabId]: next }))
              void window.rbrowse.emulation.set(tabId, next)
            }}
          />
        )}
        {/* The save-password bar lives in the chrome, not over the page: the
            page is a native view painted on top, so a bar drawn there would be
            hidden. Sitting here it shrinks the page slot instead, exactly like
            the device bar. */}
        {offer && (
          <SavePasswordBar
            offer={offer}
            onSave={async () => {
              const r = await window.rbrowse.passwords.save(offer.tabId)
              // Keep the bar up on failure so its reason is visible; a failed
              // save that vanished would read as a save that worked.
              if (r.ok) setOffer(null)
              return r
            }}
            onNever={() => { void window.rbrowse.passwords.dismiss(offer.tabId, true); setOffer(null) }}
            onDismiss={() => { void window.rbrowse.passwords.dismiss(offer.tabId); setOffer(null) }}
          />
        )}
      </div>

      <div className="middle">
        {/*
          The page slot. Empty by design — main paints a `WebContentsView` over
          this exact rectangle, so nothing rendered inside it would ever be
          visible. Its only job is to be measured.
        */}
        <div className="page-slot" ref={slotRef}>
          {!tab && (
            <div className="page-slot-empty">
              <Empty phase="no tab open" detail="press + in the tab strip, or ⌘T" />
            </div>
          )}
        </div>

        {panelOpen && (
          <Inspector
            tab={panelTab}
            width={panelWidth}
            counts={{
              notes: annotations.length,
              errors: entries.filter(e => bucket(e.level) === 'error').length,
              requests: requests.length,
            }}
            onTab={setPanelTab}
            onWidth={w => {
              setPanelWidth(w)
              // The slot's own observer will catch this, but only after the
              // browser has laid the new width out. The extra frame is what
              // keeps the native page view from trailing the grip by a beat.
              requestAnimationFrame(remeasureSlot)
            }}
            onClose={() => setPanelOpen(false)}
          >
            {panel ?? <Empty phase="no tab open" detail="notes attach to a page" />}
          </Inspector>
        )}
      </div>

      <StatusStrip
        control={control}
        capture={capture}
        tab={tab}
        slot={slot}
        selectedCount={selectedNotes.length}
        onConnect={async () => { setControl(await window.rbrowse.control.connect()) }}
        onActivate={sessionId => window.rbrowse.control.activate(sessionId)}
        onReport={sessionId => window.rbrowse.control.report(sessionId, selectedNotes)}
      />
    </div>
  )
}

/** `crypto.randomUUID` needs a secure context, which `file://` in a packaged
 *  Electron app is not guaranteed to be. The fallback is not cryptographic and
 *  does not need to be — these ids only have to be unique within one list. */
function newId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID()
  return `a-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`
}
