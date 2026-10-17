/**
 * R BROWSER — the page preload.
 * =============================
 *
 * This file runs inside *someone else's website*. That is the whole reason it
 * is written the way it is:
 *
 *   - **It exposes nothing.** No `contextBridge`, no global, not even a
 *     property on `document`. A preload runs in an isolated world that shares
 *     the DOM, and sharing the DOM is all the picker needs. Anything reachable
 *     from the page's own JS is something the page can detect, drive, or break
 *     — and a site that notices it is being inspected is a site that behaves
 *     differently while you inspect it.
 *   - **It must not throw.** It runs on pages that freeze prototypes, replace
 *     `Element.prototype.matches`, and return proxies from `getAttribute`. A
 *     picker that dies on one hostile page is worse than one that returns a
 *     weaker selector, so every DOM read that a page can influence goes through
 *     a `safe*` wrapper and falls back rather than propagating.
 *   - **It runs in every frame.** Each frame draws its own overlay over its own
 *     elements, because hit testing only works inside the document that owns
 *     the node. A pick from a subframe carries `frameUrl`; nothing tries to
 *     project a child frame's rects into the top document, which would be wrong
 *     the moment the frame is transformed, scrolled or cross-origin.
 *
 * The visual rules are SIGNAL ROOM's and they are load-bearing, not taste:
 * zero radius, hairline borders rather than glows, no emoji, and **no blinking
 * or pulsing**. The highlight already moves with the pointer, and movement is
 * the liveness — an animated outline on top of a page the operator is trying to
 * read is noise competing with the thing being looked at.
 */

import { ipcRenderer } from 'electron'
import { RBPage } from '../shared/contract'
import { openGesture, shouldSwallow, afterEvent, type GestureState } from './gesture'
import { anchorsFor, computedFor, sourceHintFor } from './source-hint'
import type { ElementEdit, ElementRef, FillCredentials, PickMode, Viewport } from '../shared/contract'

/* -------------------------------------------------------------- constants */

/** SIGNAL ROOM's "act now" accent. It appears here and nowhere else in the
 *  overlay: the live highlight is the one thing the operator is aiming with. */
const ACCENT = 'rgb(230 59 46)'
/** Everything already picked is past tense, so it gets the hairline, not the
 *  accent. Two reds on screen means neither of them says "this one". */
const HAIRLINE = 'rgba(232,230,225,0.9)'
/** Above any page's own stacking. Sites do use 2147483647 themselves, and when
 *  they do we lose — but `position: fixed` + a late `documentElement` child
 *  wins against almost everything short of that. */
const Z = 2147483647

/** Curated, and deliberately short. A React app routinely carries serialised
 *  props in `data-*`; dumping the whole attribute map into a prompt buries the
 *  three fields that actually identify the node. */
const KEPT_ATTRS = [
  'data-testid',
  'data-test',
  'data-cy',
  'aria-label',
  'name',
  'role',
  'href',
  'type',
  'placeholder',
  'alt',
  'title',
] as const

/** The test-id attributes, in the order they are tried as a selector. These are
 *  the ones a coding agent can grep for in the source, which is the entire
 *  point of preferring them over a class. */
const TEST_ID_ATTRS = ['data-testid', 'data-test', 'data-cy'] as const

/** An id that a framework minted this render is worse than no id at all: it
 *  resolves today and is absent from the source and from tomorrow's DOM.
 *  - `field_1234`, `item-908`  — server/ORM row ids
 *  - `:r3:`                    — React `useId`
 *  - an 8+ hex run             — hashes of every provenance */
const ID_LOOKS_GENERATED = [/^[a-z]*[-_]?\d{3,}$/i, /:r[0-9a-z]+:/i, /[0-9a-f]{8,}/i]

/** CSS-modules / emotion / styled-components output. Same argument as ids: a
 *  perfectly stable DOM string that does not exist anywhere in the repo. */
const CLASS_LOOKS_HASHED = /^(css|sc|emotion)-[0-9a-z]{4,}$/i

/** More than this many classes in one selector and it stops being readable —
 *  a twenty-class Tailwind chain is technically unique and useless to a human
 *  reading the prompt, and it breaks on the next style tweak. They are still
 *  *recorded* in `classes`; they are just not what we build the selector from. */
const MAX_SELECTOR_CLASSES = 3

/** `innerText` is the most greppable thing about most elements, and also the
 *  thing most likely to be a whole article. */
const TEXT_CAP = 120

/** Scroll fires per frame; main only needs where the page came to rest. */
const SCROLL_SETTLE_MS = 150

/* ------------------------------------------------------------ safe DOM ops */
/* The page owns these prototypes and may have replaced them. Every one of the
 * wrappers below exists because the alternative is the picker throwing inside a
 * capture-phase listener, which does not merely lose the pick — it can leave
 * the click suppression half-applied and let a navigation through. */

function safe<T>(fn: () => T, fallback: T): T {
  try {
    return fn()
  } catch {
    return fallback
  }
}

function safeQueryCount(selector: string): number {
  return safe(() => document.querySelectorAll(selector).length, -1)
}

/** `-1` means "the page made this unanswerable", which is treated as *not*
 *  unique — we would rather fall through to the xpath than ship a selector we
 *  never actually resolved. That is the rule this whole file exists to serve:
 *  a selector that was reasoned about but not run is the bug. */
function isUnique(selector: string): boolean {
  return safeQueryCount(selector) === 1
}

function safeAttr(el: Element, name: string): string | null {
  return safe(() => {
    const v = el.getAttribute(name)
    return typeof v === 'string' ? v : null
  }, null)
}

function safeTag(el: Element): string {
  return safe(() => String(el.tagName || '').toLowerCase(), 'unknown')
}

function safeClasses(el: Element): string[] {
  return safe(() => {
    const raw = el.getAttribute('class')
    if (typeof raw !== 'string') return []
    return raw.split(/\s+/).filter((c) => c.length > 0)
  }, [])
}

/** `CSS.escape` is the only correct way to put an arbitrary id or class into a
 *  selector, and it is missing in enough embedded webviews to be worth a
 *  fallback. The fallback is conservative on purpose: a value it cannot escape
 *  simply fails the uniqueness check and we move to the next strategy. */
function esc(value: string): string {
  return safe(() => {
    const css = (window as unknown as { CSS?: { escape?: (s: string) => string } }).CSS
    if (css && typeof css.escape === 'function') return css.escape(value)
    return value.replace(/[^a-zA-Z0-9_-]/g, '\\$&')
  }, value)
}

/* -------------------------------------------------------- stability tests */

function idIsUsable(id: string): boolean {
  if (!id || /\s/.test(id)) return false
  return !ID_LOOKS_GENERATED.some((re) => re.test(id))
}

/**
 * A class segment that mixes letters and digits and is long enough to be a
 * hash (`bdVaJa2x`, `1x2y3z`) is treated as generated. `items-center`,
 * `gap-2` and `text-2xl` all survive this, which is what we want — Tailwind
 * classes are stable, they are merely *numerous*, and that is handled by the
 * count cap rather than by pretending they are hashes.
 */
function classIsStable(cls: string): boolean {
  if (CLASS_LOOKS_HASHED.test(cls)) return false
  const segments = cls.split(/[-_]/)
  for (const seg of segments) {
    if (seg.length >= 5 && /[a-z]/i.test(seg) && /\d/.test(seg)) return false
    if (seg.length >= 4 && /^\d+$/.test(seg)) return false
  }
  return true
}

/* --------------------------------------------------------------- selectors */

/** The index-based absolute xpath. It is never pretty and it always resolves,
 *  which is precisely its job: it is the handle that still works when the
 *  selector was built on a class the source does not contain. */
function xpathFor(el: Element): string {
  return safe(() => {
    const parts: string[] = []
    let node: Element | null = el
    while (node && node.nodeType === 1) {
      const tag = safeTag(node)
      const parent: Element | null = node.parentElement
      if (!parent) {
        parts.unshift(`/${tag}`)
        break
      }
      let index = 1
      for (const sibling of Array.from(parent.children)) {
        if (sibling === node) break
        if (safeTag(sibling) === tag) index += 1
      }
      parts.unshift(`/${tag}[${index}]`)
      node = parent
    }
    return parts.join('')
  }, '')
}

function nthOfType(el: Element): number {
  return safe(() => {
    const parent = el.parentElement
    if (!parent) return 1
    const tag = safeTag(el)
    let index = 1
    for (const sibling of Array.from(parent.children)) {
      if (sibling === el) break
      if (safeTag(sibling) === tag) index += 1
    }
    return index
  }, 1)
}

/** One element's contribution to a chained selector: tag, plus a usable id or
 *  a small number of stable classes when there are any. */
function localPart(el: Element, allowClasses: boolean): string {
  const tag = safeTag(el)
  const id = safeAttr(el, 'id')
  if (id && idIsUsable(id)) return `${tag}#${esc(id)}`
  if (!allowClasses) return tag
  const stable = safeClasses(el).filter(classIsStable)
  if (stable.length === 0 || stable.length > MAX_SELECTOR_CLASSES) return tag
  return tag + stable.map((c) => `.${esc(c)}`).join('')
}

/**
 * The shortest selector that **actually resolves to exactly one node**, tried
 * in this order:
 *
 *   1. `#id`, when the id does not look minted this render.
 *   2. `[data-testid=…]`, `[data-test=…]`, `[data-cy=…]` — the attributes put
 *      there for exactly this purpose, and the ones a reader can grep.
 *   3. `tag.stable-class…`, at most three classes and no hashes.
 *   4. a parent-chain walk, adding `:nth-of-type(n)` as it climbs.
 *
 * Every candidate is run through `document.querySelectorAll` before it is
 * returned. Steps 1–3 are cheap guesses and any of them can collide on a real
 * page; shipping one unverified means handing a coding agent an address for a
 * different element, which is a worse outcome than a long selector.
 */
function selectorFor(el: Element): string {
  const id = safeAttr(el, 'id')
  if (id && idIsUsable(id)) {
    const candidate = `#${esc(id)}`
    if (isUnique(candidate)) return candidate
  }

  for (const attr of TEST_ID_ATTRS) {
    const value = safeAttr(el, attr)
    if (value) {
      const candidate = `[${attr}="${value.replace(/["\\]/g, '\\$&')}"]`
      if (isUnique(candidate)) return candidate
    }
  }

  const classed = localPart(el, true)
  if (classed.indexOf('.') !== -1 && isUnique(classed)) return classed

  // The walk. Each step is qualified by `:nth-of-type` so a chain of anonymous
  // wrapper divs — the ordinary case in a React app — still narrows.
  const chain: string[] = []
  let node: Element | null = el
  let depth = 0
  while (node && node.nodeType === 1 && depth < 12) {
    let part = localPart(node, depth === 0)
    if (node.parentElement) part += `:nth-of-type(${nthOfType(node)})`
    chain.unshift(part)
    const candidate = chain.join(' > ')
    if (isUnique(candidate)) return candidate
    node = node.parentElement
    depth += 1
  }

  // Nothing resolved uniquely — which happens on pages that reparent under the
  // pointer. The caller always sends `xpath` too, so this is degraded, not lost.
  return chain.length ? chain.join(' > ') : safeTag(el)
}

/** A trail for a human reading the prompt, not a selector. Four steps is the
 *  most that still fits on one line and the most that still means anything. */
function pathFor(el: Element): string {
  return safe(() => {
    const steps: string[] = []
    let node: Element | null = el
    while (node && steps.length < 4) {
      const tag = safeTag(node)
      const id = safeAttr(node, 'id')
      const stable = safeClasses(node).filter(classIsStable)
      if (id && idIsUsable(id)) steps.unshift(`${tag}#${id}`)
      else if (stable.length) steps.unshift(`${tag}.${stable[0]}`)
      else steps.unshift(tag)
      node = node.parentElement
    }
    return steps.join(' > ')
  }, '')
}

function textFor(el: Element): string | undefined {
  return safe(() => {
    const raw = (el as HTMLElement).innerText
    if (typeof raw !== 'string') return undefined
    const collapsed = raw.replace(/\s+/g, ' ').trim()
    if (!collapsed) return undefined
    return collapsed.length > TEXT_CAP ? `${collapsed.slice(0, TEXT_CAP)}…` : collapsed
  }, undefined)
}

function attrsFor(el: Element): Record<string, string> | undefined {
  const out: Record<string, string> = {}
  let any = false
  for (const name of KEPT_ATTRS) {
    const value = safeAttr(el, name)
    if (value !== null && value !== '') {
      out[name] = value
      any = true
    }
  }
  return any ? out : undefined
}

/* ----------------------------------------------------------- the ElementRef */

function viewportRect(el: Element): DOMRect | null {
  return safe(() => el.getBoundingClientRect(), null)
}

/**
 * `rect` is in **page** coordinates — viewport rect plus current scroll —
 * because a screenshot clip taken later must not depend on where the page
 * happened to be scrolled when the operator pointed at it.
 */
function refFor(el: Element): ElementRef | null {
  const rect = viewportRect(el)
  if (!rect) return null
  const id = safeAttr(el, 'id')
  const classes = safeClasses(el)
  const path = pathFor(el)
  const text = textFor(el)
  const attrs = attrsFor(el)
  const ref: ElementRef = {
    selector: safe(() => selectorFor(el), safeTag(el)),
    xpath: xpathFor(el),
    tag: safeTag(el),
    rect: {
      x: Math.round(rect.left + window.scrollX),
      y: Math.round(rect.top + window.scrollY),
      width: Math.round(rect.width),
      height: Math.round(rect.height),
    },
  }
  if (text) ref.text = text
  if (id) ref.id = id
  if (classes.length) ref.classes = classes
  if (attrs) ref.attrs = attrs
  if (path) ref.path = path

  // The greppable half. Each of these is individually `safe`d inside
  // `source-hint.ts`, and wrapped again here: this runs on pages we do not
  // control, and a throw out of `refFor` is a lost pick *and* a half-applied
  // click suppression — see `swallow`. A missing anchor list is degraded; a
  // picker that dies on one hostile page is broken.
  const source = safe(() => sourceHintFor(el), undefined)
  const anchors = safe(() => anchorsFor(el, source, text), [])
  const computed = safe(() => computedFor(el), {})
  if (anchors.length) ref.anchors = anchors
  if (source) ref.source = source
  if (Object.keys(computed).length) ref.computed = computed

  // Only inside a frame. At top level its presence would read as "this pick was
  // somewhere else", which is the confusion the field exists to remove.
  if (window !== window.top) ref.frameUrl = location.href
  return ref
}

/** Selector first — it is the one a reader can check by eye — then the xpath,
 *  which is the whole reason the xpath is always sent. */
function resolveRef(ref: ElementRef): Element | null {
  if (ref.selector) {
    const found = safe(() => document.querySelector(ref.selector), null)
    if (found) return found
  }
  if (ref.xpath) {
    const found = safe(() => {
      const result = document.evaluate(
        ref.xpath,
        document,
        null,
        9 /* FIRST_ORDERED_NODE_TYPE — the numeric literal, because a page may
             have shadowed `XPathResult` */,
        null,
      )
      const node = result.singleNodeValue
      return node && node.nodeType === 1 ? (node as Element) : null
    }, null)
    if (found) return found
  }
  return null
}

/* -------------------------------------------------------------- the overlay */

interface Marker {
  el: Element
  n: number
}

/**
 * The overlay lives in a **closed** shadow root on one div appended to
 * `documentElement`. Closed rather than open because the page must not be able
 * to reach in and restyle it, and because a page's own `querySelectorAll('*')`
 * sweep — analytics libraries do this — must not enumerate our internals and
 * report them as page content.
 *
 * Everything in it is `pointer-events: none`. Hit testing is done with
 * `document.elementFromPoint` on the real page, never against the overlay: an
 * overlay that can be hit is an overlay that picks itself.
 */
class Overlay {
  private host: HTMLDivElement | null = null
  private box: HTMLDivElement | null = null
  private label: HTMLDivElement | null = null
  private markerLayer: HTMLDivElement | null = null

  ensure(): void {
    if (this.host) return
    const doc = document
    if (!doc.documentElement) return
    const host = doc.createElement('div')
    // Not a `data-*` name anyone would recognise, and nothing identifying: the
    // page can still see a stray div in its own tree, so at least give it
    // nothing to key off.
    host.setAttribute('style', 'all: initial; position: static;')
    const root = safe(() => host.attachShadow({ mode: 'closed' }), null)
    if (!root) return

    const style = doc.createElement('style')
    style.textContent = [
      ':host{all:initial}',
      '.layer{position:fixed;inset:0;pointer-events:none;z-index:' + Z + ';}',
      // Zero radius, hairline border, flat fill. No shadow, no glow, and above
      // all no animation — the highlight moves with the pointer and that
      // movement is the only liveness this needs.
      '.box{position:fixed;box-sizing:border-box;border:1px solid ' +
        ACCENT +
        ';background:rgba(230,59,46,0.08);border-radius:0;pointer-events:none;display:none;}',
      '.label{position:fixed;box-sizing:border-box;border:1px solid ' +
        ACCENT +
        ';background:rgb(16,16,16);color:rgb(232,230,225);' +
        'font:400 11px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace;' +
        'padding:2px 6px;border-radius:0;white-space:nowrap;pointer-events:none;display:none;' +
        'max-width:60vw;overflow:hidden;text-overflow:ellipsis;}',
      '.label .dim{color:rgba(232,230,225,0.55);}',
      '.mark{position:fixed;box-sizing:border-box;border:1px solid ' +
        HAIRLINE +
        ';border-radius:0;pointer-events:none;}',
      '.mark>span{position:absolute;top:0;left:0;background:rgb(16,16,16);' +
        'color:rgb(232,230,225);border:1px solid ' +
        HAIRLINE +
        ';font:400 10px/1.2 ui-monospace,SFMono-Regular,Menlo,monospace;padding:1px 4px;}',
    ].join('\n')

    const layer = doc.createElement('div')
    layer.className = 'layer'
    const box = doc.createElement('div')
    box.className = 'box'
    const label = doc.createElement('div')
    label.className = 'label'
    const markers = doc.createElement('div')

    layer.appendChild(markers)
    layer.appendChild(box)
    layer.appendChild(label)
    root.appendChild(style)
    root.appendChild(layer)
    doc.documentElement.appendChild(host)

    this.host = host
    this.box = box
    this.label = label
    this.markerLayer = markers
  }

  /**
   * Torn down completely rather than hidden. A `pointer-events:none` element
   * parked forever in someone else's document is exactly the kind of thing that
   * breaks a site's own hit testing or layout in a way nobody attributes to the
   * browser they were using.
   */
  destroy(): void {
    safe(() => {
      if (this.host && this.host.parentNode) this.host.parentNode.removeChild(this.host)
      return null
    }, null)
    this.host = null
    this.box = null
    this.label = null
    this.markerLayer = null
  }

  hideActive(): void {
    if (this.box) this.box.style.display = 'none'
    if (this.label) this.label.style.display = 'none'
  }

  /** `rect` here is viewport-relative, not the page coordinates of an
   *  `ElementRef` — the overlay is `position: fixed`, so this is the one place
   *  scroll must *not* be added back in. */
  drawActive(rect: DOMRect, caption: string, size: string): void {
    this.ensure()
    const { box, label } = this
    if (!box || !label) return
    box.style.display = 'block'
    box.style.left = `${Math.round(rect.left)}px`
    box.style.top = `${Math.round(rect.top)}px`
    box.style.width = `${Math.max(0, Math.round(rect.width))}px`
    box.style.height = `${Math.max(0, Math.round(rect.height))}px`

    label.textContent = ''
    const head = document.createElement('span')
    head.textContent = caption
    const tail = document.createElement('span')
    tail.className = 'dim'
    tail.textContent = `  ${size}`
    label.appendChild(head)
    label.appendChild(tail)

    label.style.display = 'block'
    // Above the box when there is room, inside-top when the element is against
    // the top of the viewport. A label drawn off-screen is a label that is not
    // there, and the size readout is half of why the operator is looking.
    const labelHeight = label.offsetHeight || 18
    const above = rect.top - labelHeight - 2
    label.style.top = `${Math.round(above >= 0 ? above : Math.min(rect.top + 2, window.innerHeight - labelHeight))}px`
    label.style.left = `${Math.round(Math.max(0, Math.min(rect.left, window.innerWidth - label.offsetWidth)))}px`
  }

  /** Persistent markers for a multi-pick run. Numbered, because "did I already
   *  tap that one?" is the only question they answer. */
  drawMarkers(markers: Marker[]): void {
    if (markers.length) this.ensure()
    const layer = this.markerLayer
    if (!layer) return
    layer.textContent = ''
    for (const marker of markers) {
      const rect = viewportRect(marker.el)
      if (!rect) continue
      const div = document.createElement('div')
      div.className = 'mark'
      div.style.left = `${Math.round(rect.left)}px`
      div.style.top = `${Math.round(rect.top)}px`
      div.style.width = `${Math.max(0, Math.round(rect.width))}px`
      div.style.height = `${Math.max(0, Math.round(rect.height))}px`
      const badge = document.createElement('span')
      badge.textContent = String(marker.n)
      div.appendChild(badge)
      layer.appendChild(div)
    }
  }

  get attached(): boolean {
    return this.host !== null
  }
}

const overlay = new Overlay()

/* -------------------------------------------------------- the comment panel */

/** `rgb(r, g, b)` / `rgba(...)` → `#rrggbb`, because `<input type=color>` only
 *  speaks hex and `getComputedStyle` only speaks rgb. Anything it cannot parse
 *  (a gradient, `transparent`, a named colour it did not resolve) returns null,
 *  and the field is then left at its default rather than showing a wrong swatch. */
function rgbToHex(value: string): string | null {
  const m = value.match(/^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/i)
  if (!m) return null
  const hex = (n: string) => Math.max(0, Math.min(255, parseInt(n, 10))).toString(16).padStart(2, '0')
  return `#${hex(m[1])}${hex(m[2])}${hex(m[3])}`
}

/** A leaf whose whole content is text — the only shape where replacing
 *  `textContent` is safe. Editing the text of a container would flatten every
 *  child it holds into the one string the operator typed. */
function isTextLeaf(el: Element): boolean {
  return safe(() => el.children.length === 0 && (el.textContent ?? '').trim().length > 0, false)
}

interface PanelResult {
  note: string
  edits: ElementEdit[]
}

/**
 * The small form that opens at the pointer the moment something is picked — the
 * "comment right where I clicked" half of the feature, and the edit-mode fields
 * that turn a note into a concrete before/after.
 *
 * It is the one interactive thing this preload draws, so it breaks the overlay's
 * `pointer-events: none` rule on purpose: the operator types into it. That is
 * also why every capture-phase swallow handler asks `panelOwns(event)` first —
 * a click on Save or a keystroke in the textarea must reach the panel, not be
 * eaten by the picker's navigation guard.
 *
 * Edits are previewed live on the real element and recorded as before/after.
 * Cancel restores the element exactly (`origStyle`, `origText`); Save leaves the
 * preview in place so a screenshot taken after the pick shows the "make it look
 * like this" state.
 */
class CommentPanel {
  host: HTMLDivElement | null = null
  private textarea: HTMLTextAreaElement | null = null
  private readonly el: Element
  private readonly done: (result: PanelResult | null) => void
  private readonly origStyle: string | null
  private readonly origText: string
  /** prop → { from, to }; `from` captured once, at open. */
  private readonly edits = new Map<string, { from: string; to: string }>()
  private closed = false

  constructor(el: Element, atX: number, atY: number, _multi: boolean, done: (r: PanelResult | null) => void) {
    this.el = el
    this.done = done
    this.origStyle = safe(() => el.getAttribute('style'), null)
    this.origText = safe(() => el.textContent ?? '', '')
    this.build(atX, atY)
  }

  /** True for any event that originated inside the panel, so the picker's
   *  window-level capture handlers let it through. A closed shadow root
   *  retargets to the host, so `composedPath` includes the host and stops
   *  there — exactly the boundary we want. */
  owns(event: Event): boolean {
    if (!this.host) return false
    return safe(() => event.composedPath().includes(this.host as EventTarget), false)
  }

  private computed(prop: string): string {
    return safe(() => getComputedStyle(this.el as HTMLElement).getPropertyValue(prop).trim(), '')
  }

  /** Apply a CSS property to the element and remember the change. A `to` equal
   *  to `from` erases the record rather than shipping a no-op edit. */
  private setProp(prop: string, to: string): void {
    const existing = this.edits.get(prop)
    const from = existing ? existing.from : this.computed(prop)
    safe(() => (this.el as HTMLElement).style.setProperty(prop, to), undefined)
    if (to.trim() === from.trim() || to.trim() === '') this.edits.delete(prop)
    else this.edits.set(prop, { from, to })
  }

  private setText(to: string): void {
    const existing = this.edits.get('text')
    const from = existing ? existing.from : this.origText
    safe(() => { this.el.textContent = to }, undefined)
    if (to === from) this.edits.delete('text')
    else this.edits.set('text', { from, to })
  }

  private build(atX: number, atY: number): void {
    if (!document.documentElement) return
    const host = document.createElement('div')
    host.setAttribute('style', 'all: initial; position: static;')
    const root = safe(() => host.attachShadow({ mode: 'closed' }), null)
    if (!root) return

    const style = document.createElement('style')
    style.textContent = [
      ':host{all:initial}',
      '.panel{position:fixed;z-index:' + Z + ';box-sizing:border-box;width:288px;max-width:92vw;' +
        'background:rgb(16,16,16);border:1px solid rgba(232,230,225,0.9);border-radius:0;' +
        'color:rgb(232,230,225);font:400 12px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;' +
        'padding:8px;pointer-events:auto;box-shadow:0 6px 24px rgba(0,0,0,0.5);}',
      '.cap{font-size:10px;color:rgba(232,230,225,0.55);white-space:nowrap;overflow:hidden;' +
        'text-overflow:ellipsis;margin-bottom:6px;}',
      '.panel textarea{box-sizing:border-box;width:100%;min-height:52px;resize:vertical;' +
        'background:rgb(10,10,10);color:inherit;border:1px solid rgba(232,230,225,0.25);' +
        'border-radius:0;padding:6px;font:inherit;outline:none;}',
      '.panel textarea:focus,.panel input:focus{border-color:rgba(232,230,225,0.6);}',
      '.edit{margin-top:8px;border-top:1px solid rgba(232,230,225,0.12);padding-top:8px;display:none;}',
      '.edit.on{display:block;}',
      '.field{display:flex;align-items:center;gap:6px;margin-bottom:6px;}',
      '.field label{flex:0 0 66px;font-size:10px;color:rgba(232,230,225,0.55);}',
      '.field input[type=text]{flex:1 1 auto;box-sizing:border-box;background:rgb(10,10,10);' +
        'color:inherit;border:1px solid rgba(232,230,225,0.25);border-radius:0;padding:3px 5px;' +
        'font:inherit;outline:none;min-width:0;}',
      '.field input[type=color]{flex:0 0 26px;height:22px;padding:0;border:1px solid rgba(232,230,225,0.25);' +
        'background:rgb(10,10,10);border-radius:0;}',
      '.edit textarea{min-height:38px;}',
      '.row{display:flex;justify-content:space-between;align-items:center;margin-top:8px;gap:8px;}',
      '.btns{display:flex;gap:6px;}',
      '.panel button{background:rgb(10,10,10);color:inherit;border:1px solid rgba(232,230,225,0.35);' +
        'border-radius:0;padding:3px 10px;font:inherit;cursor:pointer;}',
      '.panel button:hover{border-color:rgba(232,230,225,0.7);}',
      '.panel button.go{border-color:rgba(232,230,225,0.7);}',
      '.toggle{background:none;border:none;color:rgba(232,230,225,0.6);font:inherit;cursor:pointer;' +
        'padding:0;text-decoration:underline;}',
      '.toggle:hover{color:rgb(232,230,225);}',
      '.hint{font-size:10px;color:rgba(232,230,225,0.4);margin-top:2px;}',
    ].join('\n')

    const panel = document.createElement('div')
    panel.className = 'panel'

    const cap = document.createElement('div')
    cap.className = 'cap'
    cap.textContent = captionFor(this.el)
    panel.appendChild(cap)

    const ta = document.createElement('textarea')
    ta.setAttribute('placeholder', "what's wrong with this? — Enter to save, Esc to cancel")
    panel.appendChild(ta)
    this.textarea = ta

    // --- edit-mode fields, hidden until asked for ---------------------------
    const edit = document.createElement('div')
    edit.className = 'edit'

    if (isTextLeaf(this.el)) {
      edit.appendChild(this.textField('text', this.origText.trim(), (v) => this.setText(v)))
    }
    edit.appendChild(this.colorField('color', this.computed('color'), (v) => this.setProp('color', v)))
    edit.appendChild(
      this.colorField('background', this.computed('background-color'), (v) =>
        this.setProp('background-color', v),
      ),
    )
    edit.appendChild(
      this.textField('font size', this.computed('font-size'), (v) => this.setProp('font-size', v)),
    )

    const cssWrap = document.createElement('div')
    const cssTa = document.createElement('textarea')
    cssTa.setAttribute('placeholder', 'more css — padding: 8px; border-radius: 4px')
    cssTa.addEventListener('input', () => this.applyRawCss(cssTa.value))
    cssWrap.appendChild(cssTa)
    const hint = document.createElement('div')
    hint.className = 'hint'
    hint.textContent = 'declarations applied live to the element'
    cssWrap.appendChild(hint)
    edit.appendChild(cssWrap)

    panel.appendChild(edit)

    // --- footer -------------------------------------------------------------
    const row = document.createElement('div')
    row.className = 'row'
    const toggle = document.createElement('button')
    toggle.className = 'toggle'
    toggle.type = 'button'
    toggle.textContent = 'edit styles ▸'
    toggle.addEventListener('click', () => {
      const on = edit.classList.toggle('on')
      toggle.textContent = on ? 'edit styles ▾' : 'edit styles ▸'
    })
    const btns = document.createElement('div')
    btns.className = 'btns'
    const cancel = document.createElement('button')
    cancel.type = 'button'
    cancel.textContent = 'cancel'
    cancel.addEventListener('click', () => this.finish(false))
    const save = document.createElement('button')
    save.type = 'button'
    save.className = 'go'
    save.textContent = 'save'
    save.addEventListener('click', () => this.finish(true))
    btns.appendChild(cancel)
    btns.appendChild(save)
    row.appendChild(toggle)
    row.appendChild(btns)
    panel.appendChild(row)

    // Enter saves, Shift+Enter is a newline, Escape cancels. Keydown is taken on
    // the panel itself (bubble phase, inside the shadow root) so it never
    // reaches the picker's window-level guard — which in any case has already
    // stood down, `pickMode.active` being false while a panel is open.
    panel.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault()
        this.finish(true)
      } else if (e.key === 'Escape') {
        e.preventDefault()
        this.finish(false)
      }
    })

    root.appendChild(style)
    root.appendChild(panel)
    document.documentElement.appendChild(host)
    this.host = host

    // Place at the pointer, nudged down-right, then clamped so the whole panel
    // stays on screen — a form drawn half off the viewport is a form you cannot
    // finish. Measured after append, because its height depends on which fields
    // exist.
    const w = panel.offsetWidth || 288
    const h = panel.offsetHeight || 160
    const left = Math.max(6, Math.min(atX + 12, window.innerWidth - w - 6))
    const top = Math.max(6, Math.min(atY + 12, window.innerHeight - h - 6))
    panel.style.left = `${Math.round(left)}px`
    panel.style.top = `${Math.round(top)}px`

    // Focus the comment box so typing starts immediately — the whole gesture is
    // click, type, Enter.
    safe(() => ta.focus(), undefined)
  }

  private textField(label: string, initial: string, apply: (v: string) => void): HTMLDivElement {
    const field = document.createElement('div')
    field.className = 'field'
    const lab = document.createElement('label')
    lab.textContent = label
    const input = document.createElement('input')
    input.type = 'text'
    input.value = initial
    input.addEventListener('input', () => apply(input.value))
    field.appendChild(lab)
    field.appendChild(input)
    return field
  }

  /** A colour swatch plus a text box, because the swatch cannot express
   *  `transparent`, `currentColor` or a variable and the operator sometimes
   *  needs to. Editing either updates the element and the sibling. */
  private colorField(label: string, initial: string, apply: (v: string) => void): HTMLDivElement {
    const field = document.createElement('div')
    field.className = 'field'
    const lab = document.createElement('label')
    lab.textContent = label
    const swatch = document.createElement('input')
    swatch.type = 'color'
    const hex = rgbToHex(initial)
    if (hex) swatch.value = hex
    const text = document.createElement('input')
    text.type = 'text'
    text.value = initial
    swatch.addEventListener('input', () => {
      text.value = swatch.value
      apply(swatch.value)
    })
    text.addEventListener('input', () => {
      const h = rgbToHex(text.value) ?? (/^#[0-9a-f]{6}$/i.test(text.value) ? text.value : null)
      if (h) swatch.value = h
      apply(text.value)
    })
    field.appendChild(lab)
    field.appendChild(swatch)
    field.appendChild(text)
    return field
  }

  /** Parse a block of `prop: value;` declarations and apply each. Re-parsed
   *  whole on every keystroke rather than diffed — the operator is typing a few
   *  lines, and the last coherent state is the one that matters. */
  private applyRawCss(text: string): void {
    for (const decl of text.split(';')) {
      const idx = decl.indexOf(':')
      if (idx < 0) continue
      const prop = decl.slice(0, idx).trim().toLowerCase()
      const value = decl.slice(idx + 1).trim()
      if (prop && value) this.setProp(prop, value)
    }
  }

  private finish(save: boolean): void {
    if (this.closed) return
    this.closed = true
    const note = safe(() => (this.textarea ? this.textarea.value.trim() : ''), '')
    if (!save) {
      // Restore the element exactly — every previewed edit is undone.
      safe(() => {
        if (this.origStyle === null) this.el.removeAttribute('style')
        else this.el.setAttribute('style', this.origStyle)
        return null
      }, null)
      if (this.edits.has('text')) safe(() => { this.el.textContent = this.origText }, undefined)
    }
    this.teardown()
    const edits: ElementEdit[] = save
      ? Array.from(this.edits.entries()).map(([prop, e]) => ({ prop, from: e.from, to: e.to }))
      : []
    this.done(save ? { note, edits } : null)
  }

  private teardown(): void {
    safe(() => {
      if (this.host && this.host.parentNode) this.host.parentNode.removeChild(this.host)
      return null
    }, null)
    this.host = null
    this.textarea = null
  }
}

let activePanel: CommentPanel | null = null

/* ------------------------------------------------------------ picker state */

let pickMode: PickMode = { active: false, multi: false, shoot: false }
/** Where the pointer last was, in viewport px. Every re-render hit-tests from
 *  here rather than caching an element, so a scroll under a still pointer
 *  highlights whatever moved under it — which is what the operator sees. */
let pointer: { x: number; y: number } | null = null
/**
 * How many ancestors above the element under the cursor we are aiming at.
 * Escaping nested wrapper divs is most of what picking a precise node in a
 * React app *is*, so Alt climbs and Shift comes back down. It survives pointer
 * movement on purpose: someone who has climbed two wrappers to reach the row is
 * usually about to compare it with the next row.
 */
let climb = 0
/** Elements already picked in a multi run, in the order they were picked. */
let markers: Marker[] = []
/** A highlight requested by the chrome (hovering the annotation list), which
 *  outlives pick mode and is cleared only by an explicit null. */
let pinned: Element | null = null

let painting = false

function paint(): void {
  if (painting) return
  painting = true
  requestAnimationFrame(() => {
    painting = false
    render()
  })
}

function targetUnderPointer(): Element | null {
  if (!pointer) return null
  const base = safe(() => document.elementFromPoint(pointer!.x, pointer!.y), null)
  if (!base) return null
  let node: Element = base
  for (let i = 0; i < climb; i += 1) {
    const parent = node.parentElement
    // Clamped rather than wrapped. Climbing past `documentElement` and landing
    // back at a leaf would make the modifier feel broken; stopping is legible.
    if (!parent) break
    node = parent
  }
  return node
}

function captionFor(el: Element): string {
  const tag = safeTag(el).toUpperCase()
  const id = safeAttr(el, 'id')
  const classes = safeClasses(el).filter(classIsStable).slice(0, 2)
  let out = tag
  if (id && idIsUsable(id)) out += `#${id}`
  for (const c of classes) out += `.${c}`
  return out
}

function render(): void {
  const active = pickMode.active
  const showing = active || pinned !== null || markers.length > 0
  if (!showing) {
    if (overlay.attached) overlay.destroy()
    return
  }
  overlay.ensure()
  overlay.drawMarkers(markers)

  const el = active ? targetUnderPointer() : pinned
  if (!el) {
    overlay.hideActive()
    return
  }
  const rect = viewportRect(el)
  if (!rect) {
    overlay.hideActive()
    return
  }
  overlay.drawActive(
    rect,
    captionFor(el),
    `${Math.round(rect.width)} × ${Math.round(rect.height)}`,
  )
}

/* ------------------------------------------------------------ click capture */

/**
 * The single most important behaviour in this file.
 *
 * All four of `pointerdown`, `mousedown`, `mouseup` and `click` are suppressed
 * in the **capture** phase on `window`, because a site's own handler may be
 * bound to any of them and capture is the only phase that runs before all of
 * them. If a pick reaches the page, a link navigates — and then the
 * annotation's URL names a page the operator was never describing, while the
 * page they *were* describing is gone. There is no recovering from that after
 * the fact, so it is prevented rather than detected.
 *
 * The pick itself is taken on `pointerdown`: it is the first of the four, and a
 * `click` may never arrive at all once the earlier events are cancelled.
 */
function swallow(event: Event): void {
  event.preventDefault()
  event.stopPropagation()
  event.stopImmediatePropagation()
}

/**
 * The rest of the gesture is swallowed even after the picker disarms.
 *
 * The decision lives in `./gesture` as a pure function, not inline here: the
 * bug it encodes was invisible in place — a single pick disarms on
 * `pointerdown`, so `mouseup` and `click` arrived with `active` already false
 * and went straight through to the page, navigating any link that was picked.
 * `checks/gesture-check.ts` pins every ordering and was verified to go red
 * when the disjunction is removed.
 */
let gesture: GestureState = { openUntil: 0 }

function onPointerDown(event: PointerEvent): void {
  // A click inside an open panel is the operator typing/saving — it must reach
  // the panel, and the picker is suspended anyway. Let it through untouched.
  if (activePanel && activePanel.owns(event)) return
  if (!pickMode.active || activePanel) return
  swallow(event)
  // Opened before `disarm()` can clear `pickMode.active`, so the `mouseup` and
  // `click` still to come are suppressed on the single-pick path too.
  gesture = openGesture(Date.now())
  pointer = { x: event.clientX, y: event.clientY }
  const el = targetUnderPointer()
  if (!el) return
  const ref = refFor(el)
  if (!ref) return

  // Stamp the element so the main process can find *this exact node* from the
  // page's main world, where the framework expandos live. An attribute crosses
  // the world boundary; a selector would have to be re-resolved over there and
  // could match a different node, or none, on a page that re-rendered in
  // between. Main clears it once it has probed. Stamped now, sent on Save.
  safe(() => el.setAttribute('data-rb-pick', '1'), undefined)

  // A multi run keeps its running markers; both paths suspend picking while the
  // form is open (picking resumes on Save, if multi).
  const multi = pickMode.multi
  const shoot = pickMode.shoot
  const runningMarkers = multi ? markers.concat([{ el, n: markers.length + 1 }]) : []
  pickMode = { active: false, multi: false, shoot: false }
  climb = 0
  markers = runningMarkers
  pinned = el

  openPanel(el, ref, event.clientX, event.clientY, multi, shoot)
  paint()
}

/** Open the comment form at the pointer; on Save, emit the pick with the note
 *  and any live edits, and re-arm the picker when this was a multi run. */
function openPanel(el: Element, ref: ElementRef, atX: number, atY: number, multi: boolean, shoot: boolean): void {
  activePanel = new CommentPanel(el, atX, atY, multi, (result) => {
    activePanel = null
    if (result) {
      ipcRenderer.send(RBPage.picked, { ref, note: result.note, edits: result.edits })
    } else {
      // Cancelled: nothing is sent, so main never probes — clear the stamp we
      // set, and drop this pick's marker on a multi run.
      safe(() => el.removeAttribute('data-rb-pick'), undefined)
      if (multi && markers.length) markers = markers.slice(0, -1)
    }
    if (multi) {
      pickMode = { active: true, multi: true, shoot }
      pinned = null
    } else {
      pinned = null
      markers = []
    }
    paint()
  })
}

function onMouseEvent(event: Event): void {
  if (activePanel && activePanel.owns(event)) return
  if (!shouldSwallow(pickMode.active, gesture, Date.now())) return
  swallow(event)
  gesture = afterEvent(event.type, gesture)
}

function onPointerMove(event: PointerEvent): void {
  if (!pickMode.active || activePanel) return
  pointer = { x: event.clientX, y: event.clientY }
  paint()
}

function onKeyDown(event: KeyboardEvent): void {
  if (!pickMode.active) return
  if (event.key === 'Escape') {
    swallow(event)
    disarm()
    ipcRenderer.send(RBPage.cancelled)
    paint()
    return
  }
  // `event.repeat` guard: holding Alt auto-repeats, and without this one press
  // would climb to `<html>` in a few hundred milliseconds.
  if (event.repeat) return
  if (event.key === 'Alt') {
    climb += 1
    paint()
  } else if (event.key === 'Shift') {
    climb = Math.max(0, climb - 1)
    paint()
  }
}

function disarm(): void {
  pickMode = { active: false, multi: false, shoot: false }
  climb = 0
  markers = []
  // Deliberately does NOT clear `gesture`. Disarming happens halfway through a
  // mouse gesture, and the whole point of the latch is to outlive it.
}

/* ---------------------------------------------------------------- metrics */

/** Main needs the *document* size, not just the viewport: a full-page
 *  screenshot is stitched against `scrollHeight`, and a viewport height in its
 *  place silently crops everything below the fold. */
interface PageMetrics {
  viewport: Viewport
  scrollX: number
  scrollY: number
  scrollWidth: number
  scrollHeight: number
  url: string
  frameUrl?: string
}

function metrics(): PageMetrics {
  const doc = document.documentElement
  const body = document.body
  const out: PageMetrics = {
    viewport: {
      width: window.innerWidth,
      height: window.innerHeight,
      deviceScaleFactor: window.devicePixelRatio,
    },
    scrollX: Math.round(window.scrollX),
    scrollY: Math.round(window.scrollY),
    scrollWidth: Math.max(doc ? doc.scrollWidth : 0, body ? body.scrollWidth : 0, window.innerWidth),
    scrollHeight: Math.max(
      doc ? doc.scrollHeight : 0,
      body ? body.scrollHeight : 0,
      window.innerHeight,
    ),
    url: location.href,
  }
  if (window !== window.top) out.frameUrl = location.href
  return out
}

function sendMetrics(): void {
  safe(() => {
    ipcRenderer.send(RBPage.metrics, metrics())
    return null
  }, null)
}

let scrollTimer: ReturnType<typeof setTimeout> | null = null
function onScroll(): void {
  // The overlay is `position: fixed` over a page that just moved, so it has to
  // be repainted every frame; only the *message* to main is debounced.
  paint()
  if (scrollTimer !== null) clearTimeout(scrollTimer)
  scrollTimer = setTimeout(() => {
    scrollTimer = null
    sendMetrics()
  }, SCROLL_SETTLE_MS)
}

function onResize(): void {
  paint()
  sendMetrics()
}

/* ----------------------------------------------------------- credentials */
/*
 * The password-manager half of the preload: notice a login being submitted and
 * report it, and fill a saved one back in on return. It reads the page and
 * never interferes with it — no `preventDefault`, no submit of its own — so a
 * site behaves identically whether or not this is watching. Only the *top*
 * frame participates, and only real web origins: a `file://` page or a
 * cross-frame widget is not a login this should be learning.
 */

/** A same-origin http(s) top frame — the only place a login is captured or
 *  filled. Anything else returns a falsy origin and the feature stays dark. */
function credentialOrigin(): string {
  return safe(() => {
    if (window !== window.top) return ''
    if (location.protocol !== 'http:' && location.protocol !== 'https:') return ''
    return location.origin
  }, '')
}

function isVisible(el: HTMLElement): boolean {
  return safe(() => el.offsetParent !== null || el.getClientRects().length > 0, true)
}

/** The password box: the first visible `type=password`, or the first one at all
 *  if none is visible (a just-revealed field can report hidden for a tick). */
function passwordInput(requireValue: boolean): HTMLInputElement | null {
  return safe(() => {
    const all = Array.from(document.querySelectorAll('input[type="password"]')) as HTMLInputElement[]
    const pool = requireValue ? all.filter((i) => i.value) : all
    return pool.find((i) => isVisible(i)) ?? pool[0] ?? null
  }, null)
}

/**
 * The username that goes with a password box. Prefer the fields built to say so
 * — `type=email`, `autocomplete=username`, a name/id that reads as user/email/
 * login — and fall back to the nearest text-like input *before* the password in
 * DOM order, which is the ordinary stacked login layout.
 */
function usernameInput(pw: HTMLInputElement): HTMLInputElement | null {
  return safe(() => {
    const scope: ParentNode = pw.form ?? document
    const inputs = Array.from(scope.querySelectorAll('input')) as HTMLInputElement[]
    const typed = inputs.filter((i) => {
      const t = (i.type || 'text').toLowerCase()
      return t !== 'password' && t !== 'hidden' && t !== 'checkbox' && t !== 'radio' &&
        t !== 'submit' && t !== 'button' && t !== 'file'
    })
    const named = /user|email|login|account|phone|mobile/i
    const strong = typed.find((i) =>
      (i.type || '').toLowerCase() === 'email' ||
      /username|email/i.test(i.autocomplete || '') ||
      named.test(i.name || '') || named.test(i.id || ''))
    if (strong) return strong
    // Nearest preceding text field.
    const before = typed.filter((i) => pw.compareDocumentPosition(i) & Node.DOCUMENT_POSITION_PRECEDING)
    return before[before.length - 1] ?? typed[0] ?? null
  }, null)
}

/** Set a value the way a user would, so a controlled React/Vue field actually
 *  registers it — the native setter plus the `input`/`change` events a
 *  framework listens for. */
function setInputValue(input: HTMLInputElement, value: string): void {
  safe(() => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set
    if (setter) setter.call(input, value)
    else input.value = value
    input.dispatchEvent(new Event('input', { bubbles: true }))
    input.dispatchEvent(new Event('change', { bubbles: true }))
    return null
  }, null)
}

/** The last thing sent, so the several triggers (submit, Enter, button click)
 *  do not each report the same login. */
let lastCredentialSent = ''

function captureCredential(): void {
  const origin = credentialOrigin()
  if (!origin) return
  const pw = passwordInput(true)
  if (!pw || !pw.value) return
  const user = usernameInput(pw)
  const username = safe(() => (user?.value ?? '').trim(), '')
  const key = `${username}\u0000${pw.value}`
  if (key === lastCredentialSent) return
  lastCredentialSent = key
  safe(() => {
    ipcRenderer.send(RBPage.credentialSubmit, { origin, username, password: pw.value })
    return null
  }, null)
}

function onSubmitCapture(): void {
  // Read synchronously — a form submit may navigate immediately after.
  captureCredential()
}

function onCredentialKey(event: KeyboardEvent): void {
  if (event.key !== 'Enter') return
  const t = event.target as HTMLElement | null
  if (t && (t as HTMLInputElement).type === 'password') captureCredential()
}

function looksLikeSubmit(el: Element | null): boolean {
  if (!el) return false
  const btn = el.closest('button,[type="submit"],[role="button"],input[type="submit"]')
  return !!btn
}

function onCredentialClick(event: MouseEvent): void {
  // A JS-driven login (no form submit) commits on a button click; capture then,
  // but only when a password has actually been entered, so an ordinary button
  // press is not mistaken for a login.
  if (!looksLikeSubmit(event.target as Element | null)) return
  if (passwordInput(true)) captureCredential()
}

function requestCredentials(): void {
  const origin = credentialOrigin()
  if (!origin) return
  safe(() => {
    ipcRenderer.send(RBPage.credentialRequest, { origin })
    return null
  }, null)
}

onChannel<FillCredentials | null>(RBPage.fillCredentials, (cred) => {
  if (!cred || !credentialOrigin()) return
  const pw = passwordInput(false)
  if (!pw) return
  const user = usernameInput(pw)
  // Never overwrite what the operator is already typing — fill only empties.
  if (user && !user.value && cred.username) setInputValue(user, cred.username)
  if (!pw.value && cred.password) setInputValue(pw, cred.password)
})

/* ------------------------------------------------------------------- wiring */

/** One narrowing point for everything main sends. The payload crosses from
 *  another process, so it is typed once here and never re-asserted downstream;
 *  a handler that throws is swallowed rather than being allowed to kill the
 *  listener for every later message on the channel. */
function onChannel<T>(channel: string, handler: (payload: T) => void): void {
  ipcRenderer.on(channel, (_event, payload: unknown) => {
    safe(() => {
      handler(payload as T)
      return null
    }, null)
  })
}

onChannel<PickMode | null>(RBPage.setPickMode, (mode) => {
  if (mode && mode.active) {
    pickMode = { active: true, multi: !!mode.multi, shoot: !!mode.shoot }
    climb = 0
    markers = []
  } else {
    disarm()
  }
  paint()
})

onChannel<ElementRef | null>(RBPage.highlight, (ref) => {
  if (!ref) {
    pinned = null
    paint()
    return
  }
  const el = resolveRef(ref)
  // An element that is gone after a reload is an ordinary outcome, not an
  // error: the operator is hovering a list of things they annotated earlier.
  // Nothing is sent back and nothing throws.
  pinned = el
  if (el) {
    const rect = viewportRect(el)
    // Only scroll when it is actually outside the viewport. Scrolling a page
    // that is already showing the element moves it under the operator's eyes
    // for no reason.
    if (
      rect &&
      (rect.bottom < 0 ||
        rect.top > window.innerHeight ||
        rect.right < 0 ||
        rect.left > window.innerWidth)
    ) {
      safe(() => {
        el.scrollIntoView({ block: 'center', behavior: 'smooth' })
        return null
      }, null)
    }
  }
  paint()
})

/** Correlation-id request/response on one channel. Main sends
 *  `{ requestId, element }` and gets `{ requestId, element }` back; without the
 *  id a second remeasure in flight would be answered by the first one's reply
 *  and the chrome would draw the wrong rect. */
interface RemeasureRequest {
  requestId: string
  element: ElementRef
}

onChannel<RemeasureRequest | null>(RBPage.remeasure, (req) => {
  if (!req || typeof req.requestId !== 'string') return
  let fresh: ElementRef | null = null
  if (req.element) {
    const el = resolveRef(req.element)
    if (el) fresh = refFor(el)
  }
  ipcRenderer.send(RBPage.remeasure, { requestId: req.requestId, element: fresh })
})

window.addEventListener('pointerdown', onPointerDown, true)
window.addEventListener('mousedown', onMouseEvent, true)
window.addEventListener('mouseup', onMouseEvent, true)
window.addEventListener('click', onMouseEvent, true)
window.addEventListener('pointermove', onPointerMove, true)
window.addEventListener('keydown', onKeyDown, true)
window.addEventListener('scroll', onScroll, true)
window.addEventListener('resize', onResize)

// Credential capture. Read-only and never `preventDefault` — a login page must
// behave identically whether or not this is watching. Capture-phase `submit` so
// the values are read before a navigating submit tears the form down; the click
// and Enter fallbacks catch the JS-driven logins that never fire `submit`.
window.addEventListener('submit', onSubmitCapture, true)
window.addEventListener('click', onCredentialClick, true)
window.addEventListener('keydown', onCredentialKey, true)

function announceReady(): void {
  safe(() => {
    ipcRenderer.send(RBPage.ready, { url: location.href, top: window === window.top })
    return null
  }, null)
  sendMetrics()
  // Ask main for any saved login for this origin. Main also pushes a fill on
  // navigation; the fill handler only ever touches empty fields on a page that
  // actually has a password box, so an extra request is a harmless no-op.
  requestCredentials()
}

// A preload can be evaluated either side of `DOMContentLoaded` depending on how
// fast the document is; firing on the event alone means a cached page never
// announces itself at all, and main then waits forever for a frame that is
// already there.
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', announceReady, { once: true })
} else {
  announceReady()
}
