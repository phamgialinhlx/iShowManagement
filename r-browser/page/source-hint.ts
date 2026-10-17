/**
 * R BROWSER — where is this element *authored*?
 * =============================================
 *
 * The picker can say exactly where a node is in the DOM. That is not the
 * question a coding agent has to answer: it has to find the same node in a
 * source tree it can read but cannot run. This file is the bridge, and it
 * produces three things, in descending order of how much they can be trusted:
 *
 *   1. `anchorsFor` — literal strings that are *present in the source*. This is
 *      the highest-value output in the whole app. **Claude Code has no semantic
 *      index**; `Grep` is ripgrep. A handle that is not a literal in the tree is
 *      worth nothing, however stable it is in the DOM.
 *   2. `sourceHintFor` — a build-time locator attribute, when the project was
 *      built with a plugin that stamps one. Authoritative when present and
 *      absent otherwise; there is no third state, and that is the point.
 *   3. `computedFor` — a dozen computed styles *plus the interaction state that
 *      explains them*. Never the 300+ `getComputedStyle` returns.
 *
 * ---------------------------------------------------------------------------
 * WHY THERE IS NO RUNTIME FRAMEWORK DETECTION HERE
 * ---------------------------------------------------------------------------
 *
 * Every guide to this problem reaches for the same set of handles:
 * `__reactFiber$<random>`, `__reactProps$<random>`, `__vueParentComponent`,
 * `__vue__`, `__vnode`, `__svelte_meta`, `__ngContext__`, `window.ng`,
 * `window.__REACT_DEVTOOLS_GLOBAL_HOOK__`. **None of them can be read from this
 * file**, and the reason is structural rather than a bug to be worked around.
 *
 * All of those are JavaScript **expando properties** — values the page's own
 * scripts hang on a DOM node or on `window`. This preload runs in a Chromium
 * **isolated world**: `main/tab.ts` constructs the page view with
 * `contextIsolation: true`, `nodeIntegration: false` and `sandbox: true`, which
 * is precisely that arrangement. An isolated world shares the **DOM node** with
 * the page and nothing else — each world keeps its own JS wrapper for that
 * node, with its own property bag. From here those properties are not
 * `undefined`; they were never in this world at all.
 *
 * That failure is silent, which is what makes it expensive. A probe for
 * `__reactFiber$…` compiles, type-checks, runs on every page, never throws, and
 * returns nothing — for ever. A harness of pure helpers cannot catch it either,
 * because the helpers are all correct; it is the input that never arrives. So
 * the probes are **deleted rather than left in**: dead code that looks like a
 * working feature is worse than an absent one, because the next person reads it
 * as "detection exists but this page is unusual".
 *
 * `data-*` attributes are real DOM state and **do** cross the world boundary.
 * That is not a coincidence — it is why this entire ecosystem (code-inspector,
 * react-dev-inspector, vue-inspector, Onlook, Lovable) converged on stamping
 * the source location into an attribute at build time. It is the one channel
 * that survives.
 *
 * Runtime introspection is still worth having; it simply has to run in the
 * **main world**, from the main process, over CDP `Runtime.evaluate`. That is a
 * separate seam and deliberately not in this file.
 * ---------------------------------------------------------------------------
 *
 * One more rule, paid for elsewhere and enforced throughout: **a bundle path is
 * never emitted**. Under a dev server the stamped path is a real source path
 * (`src/components/Button.tsx`). A stale or production stamp can name build
 * output (`/assets/index-a3f1.js`), and pointing an agent at that invites an
 * edit that the next build overwrites and no review ever sees. Detected and
 * dropped — the hint then carries a component name and no file, which is honest
 * and still useful. An absent field is a correct answer; a fabricated one is
 * the worst outcome this file can produce.
 *
 * Everything the page can influence is wrapped **individually**. A page may
 * define an attribute getter that throws, freeze prototypes, or shadow
 * `Object.keys`; one hostile probe must not cost the others, and nothing here
 * may throw out of `sourceHintFor` into the picker's capture-phase listener —
 * see `preload-page.ts` for why a throw there is worse than a degraded answer.
 */

import type { GrepAnchor, SourceHint } from '../shared/contract'

/**
 * Deliberately exported, deliberately constant.
 *
 * It exists to be found by whoever comes to add `__reactFiber$…` back — see the
 * header. Runtime framework introspection is not missing from this file; it is
 * impossible in this file, and it belongs to a main-world CDP evaluation
 * driven from the main process.
 */
export const RUNTIME_PROBES_NEED_MAIN_WORLD = true

/* -------------------------------------------------------------------- safe */

/** The same shape as the picker's, kept local: this module must not import
 *  `preload-page.ts` (which pulls `electron`) or the check harness could not
 *  load it, and a cycle between the two files would be worse than a duplicate
 *  four-line helper. */
function safe<T>(fn: () => T, fallback: T): T {
  try {
    return fn()
  } catch {
    return fallback
  }
}

/* ------------------------------------------------------- generated strings */

/**
 * An id that a framework minted this render is worse than no id at all: it
 * resolves today, is absent from the source, and is a different string
 * tomorrow. These mirror the picker's own rejection list plus `radix-«…»`,
 * which Radix generates per mount and which no source tree contains.
 */
const ID_LOOKS_GENERATED = [
  /^[a-z]*[-_]?\d{3,}$/i, // field_1234, item-908 — server/ORM row ids
  /:r[0-9a-z]+:/i, //        :r7: — React `useId`
  /[0-9a-f]{8,}/i, //        an 8+ hex run — hashes of every provenance
  /^radix[-_]/i, //          radix-«r0», and its plainer variants
  /[«»]/, //                 the fancy quotes themselves, whoever minted them
]

export function idIsUsableAnchor(id: string): boolean {
  if (!id || /\s/.test(id)) return false
  return !ID_LOOKS_GENERATED.some((re) => re.test(id))
}

/** styled-components (`sc-bdVaJa`) and emotion (`css-1a2b3c`) — a stable DOM
 *  string that exists nowhere in the repo. */
const CLASS_LOOKS_HASHED = /^(css|sc|emotion)-[0-9a-z]{4,}$/i

/** CSS modules: `Button_primary__3xYz`. The trailing `__` segment must contain
 *  a digit before we reject it, or ordinary BEM (`btn__label`) — which *is*
 *  authored and *is* greppable — would be thrown away with it. */
const CLASS_MODULE_HASH = /__[A-Za-z0-9]*\d[A-Za-z0-9]*$/

/**
 * Is this class something a person typed into the source?
 *
 * **A hashed class is an anti-handle.** It is not merely useless — emitting it
 * sends the reader to grep for a string the tree does not contain, and a search
 * that returns nothing reads as "the element is not in this repo". So the bar
 * is deliberately on the side of dropping a real class rather than shipping a
 * generated one.
 *
 * Tailwind survives on purpose: `mt-6`, `items-center` and `text-2xl` all
 * appear verbatim in the JSX. They are handled by ranking and a count cap
 * (`rankClasses`), not by pretending they are hashes.
 */
export function classIsAuthored(cls: string): boolean {
  if (!cls || cls.length < 2) return false
  if (CLASS_LOOKS_HASHED.test(cls)) return false
  if (CLASS_MODULE_HASH.test(cls)) return false
  for (const seg of cls.split(/[-_:]/)) {
    // A segment long enough to be a hash and mixing letters with digits.
    if (seg.length >= 5 && /[a-z]/i.test(seg) && /\d/.test(seg)) return false
    // A pure-digit run that long is a generated counter, not a scale step.
    if (seg.length >= 4 && /^\d+$/.test(seg)) return false
  }
  return true
}

/** Utility frameworks name things by their effect; a design system names them
 *  by their role. `btn--primary` finds the component, `mt-6` finds four hundred
 *  files — so a semantic-looking class outranks a utility even though both are
 *  literally present. */
const UTILITY_PREFIX =
  /^(?:sm|md|lg|xl|2xl|hover|focus|active|dark|group|peer):|^(?:m|p)[trblxyse]?-|^(?:w|h|min|max|gap|top|left|right|bottom|inset|z|order|col|row|basis|grow|shrink)-|^(?:flex|grid|text|bg|border|rounded|font|leading|tracking|space|divide|shadow|opacity|ring|outline|transition|duration|ease|translate|scale|rotate|cursor|overflow|object|fill|stroke|justify|items|content|self|place)-|^(?:flex|grid|block|inline|hidden|absolute|relative|fixed|sticky|static|truncate|container|sr-only|antialiased)$/

export function classLooksSemantic(cls: string): boolean {
  if (/(?:--|__)/.test(cls)) return true // BEM says "this is a component part"
  return !UTILITY_PREFIX.test(cls)
}

/** Authored classes only, semantic first, **capped at 2**. More than two is a
 *  Tailwind chain, which is long, unreadable in a prompt, and no more locating
 *  than the first one. */
export function rankClasses(classes: readonly string[]): string[] {
  const authored = classes.filter(classIsAuthored)
  const semantic = authored.filter(classLooksSemantic)
  const rest = authored.filter((c) => !classLooksSemantic(c))
  return semantic.concat(rest).slice(0, 2)
}

/* --------------------------------------------------- source-path recovery */

/**
 * Build output, by every marker we can name cheaply.
 *
 * Erring toward "this is a bundle" is the safe direction: the cost of a false
 * positive is a hint with no `file`, and the cost of a false negative is an
 * agent editing `dist/`.
 */
export function looksLikeBundle(path: string, line?: number, column?: number): boolean {
  if (/(?:^|\/)(?:assets|dist|build|out|_next\/static|static\/js|\.output|\.nuxt)\//i.test(path)) {
    return true
  }
  if (/\.min\.(?:js|mjs|cjs)$/i.test(path)) return true
  // `index-a3f1.js`, `main.4f2c9ab1.js` — a content hash in the filename.
  if (/[-.][0-9a-f]{4,}\.(?:js|mjs|cjs)$/i.test(path)) return true
  // Everything on line 1 at column 2841 is minified, whatever it is called.
  if (line !== undefined && line <= 1 && column !== undefined && column > 200) return true
  return false
}

export interface SourceLoc {
  file: string
  line?: number
  column?: number
}

/** `http://localhost:5173/src/App.tsx` → `src/App.tsx`. Under a dev server the
 *  served path *is* the project path, which is the entire reason these stamps
 *  are usable; the origin is noise the agent would have to strip anyway. A
 *  Windows drive letter is left alone — `C:\src\…` has no `//` after its colon,
 *  so the scheme pattern does not match it. */
function stripOrigin(raw: string): string {
  const withoutScheme = raw.replace(/^[a-z][a-z0-9+.-]*:\/\/[^/]*/i, '')
  const withoutQuery = withoutScheme.replace(/[?#].*$/, '')
  // Vite serves `/src/…`; a leading slash there is the *server* root, not the
  // filesystem root, and an absolute-looking path invites an agent to open `/`.
  // A real POSIX absolute path keeps its slash only when nothing was stripped.
  return withoutScheme === raw ? withoutQuery : withoutQuery.replace(/^\/+(?=[^/])/, '')
}

function finiteOrUndefined(value: string | undefined): number | undefined {
  if (value === undefined || value === '') return undefined
  const n = Number(value)
  return Number.isFinite(n) ? n : undefined
}

/**
 * `path:line:column` — the shape every locator attribute uses.
 *
 * **Parsed from the right.** A Windows path carries its own colon
 * (`C:\src\App.tsx:42:7`), so a left-to-right split truncates it at the drive
 * letter and produces the path `C` — a file that exists nowhere, named
 * plausibly enough that nobody questions it. Only trailing all-digit fields are
 * taken, and everything before them is joined back as the path.
 */
export function parseSourceLoc(value: string): SourceLoc | undefined {
  if (typeof value !== 'string') return undefined
  const trimmed = value.trim()
  if (!trimmed) return undefined

  const parts = trimmed.split(':')
  let column: number | undefined
  let line: number | undefined
  if (parts.length > 2 && /^\d+$/.test(parts[parts.length - 1])) {
    column = finiteOrUndefined(parts.pop())
  }
  if (parts.length > 1 && /^\d+$/.test(parts[parts.length - 1])) {
    line = finiteOrUndefined(parts.pop())
  }
  // Only one trailing number was present: it is the line, not the column.
  if (line === undefined && column !== undefined) {
    line = column
    column = undefined
  }

  const file = stripOrigin(parts.join(':'))
  if (!file) return undefined
  if (looksLikeBundle(file, line, column)) return undefined
  const loc: SourceLoc = { file }
  if (line !== undefined) loc.line = line
  if (column !== undefined) loc.column = column
  return loc
}

/**
 * code-inspector's `data-insp-path`, whose value is `path:line:column:tag`.
 *
 * Four fields, the last of which is a tag name rather than a number, so
 * `parseSourceLoc` alone would read `button` as part of the path. The tag is
 * dropped and the rest goes through the same right-to-left parse — which is how
 * code-inspector itself reads it, and for the same Windows reason.
 */
export function parseInspPath(value: string): SourceLoc | undefined {
  if (typeof value !== 'string') return undefined
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const parts = trimmed.split(':')
  // Drop a trailing non-numeric field: that is the tag name. Anything else is
  // left for `parseSourceLoc`, including the perfectly ordinary case of a stamp
  // that carries no tag at all.
  if (parts.length > 3 && !/^\d+$/.test(parts[parts.length - 1])) parts.pop()
  return parseSourceLoc(parts.join(':'))
}

/* ------------------------------------------------------ component naming */

const NAME_IS_NOISE = /^(?:unknown|anonymous|object|_.*)$/i

/** Anonymous and internal wrapper names are dropped rather than shipped: a
 *  prompt that says the element lives in `_c` has spent a line saying nothing,
 *  and it crowds out the handle that would have found it. */
export function nameIsUseful(name: string | undefined): name is string {
  if (!name) return false
  if (name.length < 2 || name.length > 60) return false
  return !NAME_IS_NOISE.test(name)
}

/* -------------------------------------------------------------------- i18n */

/** The caveat is not decoration. An i18n'd label lives in a locale file and the
 *  source holds only `t('cart.checkout')`, so grepping the visible string finds
 *  the JSON and not the component — and the reader has to be told that before
 *  they conclude the element is not in the repo. */
const TEXT_CAVEAT = 'Visible text may be an i18n key in source rather than a literal.'
const TEXT_CAVEAT_I18N =
  'This app looks internationalised — the source almost certainly holds a key, ' +
  'not this string. Grep the locale files for it to recover the key first.'

export function textCaveat(i18n: boolean): string {
  return i18n ? TEXT_CAVEAT_I18N : TEXT_CAVEAT
}

/** Cheap signals only, all read defensively. `window.i18next` and friends are
 *  main-world expandos and would not be visible here (see the header), so the
 *  one signal that genuinely crosses is the `lang` attribute — real DOM state.
 *  It decides the wording of a caveat and the rank of one anchor; a wrong
 *  answer costs a sentence, not a field. */
function detectI18n(): boolean {
  return safe(() => {
    const root = document.documentElement
    const lang = root ? root.getAttribute('lang') : null
    if (typeof lang === 'string' && lang && !/^en\b/i.test(lang)) return true
    // A `<link rel="alternate" hreflang=…>` set is the other purely-DOM tell,
    // and it is what an internationalised site publishes for search engines.
    return safe(() => document.querySelectorAll('link[rel="alternate"][hreflang]').length > 1, false)
  }, false)
}

/* ------------------------------------------------------------- href paths */

/** `href` is emitted as a **path only**. The origin is per-environment — the
 *  same route is `localhost:5173/pricing` in dev and `example.com/pricing` in
 *  production — and a router file contains the path, never the origin. */
export function hrefPath(value: string): string | undefined {
  if (typeof value !== 'string') return undefined
  const trimmed = value.trim()
  if (!trimmed || /^(?:javascript|data|blob|mailto|tel):/i.test(trimmed)) return undefined
  if (trimmed.startsWith('#')) return undefined
  const path = trimmed.replace(/^[a-z][a-z0-9+.-]*:\/\/[^/]*/i, '').replace(/[?#].*$/, '')
  if (!path || path === '/') return undefined
  return path
}

/* ------------------------------------------------------------ the anchors */

const TEXT_ANCHOR_CAP = 60
const MAX_ANCHORS = 6

/**
 * A plain description of an element, so the ranking is testable without a DOM.
 *
 * The split is deliberate: reading the DOM is the part that has to be defensive
 * and cannot be tested in Node, and choosing what to emit is the part that has
 * rules worth pinning. Keeping them in one function would have made the rules
 * unreachable from a harness — which is how `classIsAuthored` would quietly
 * stop rejecting hashes.
 */
export interface AnchorInput {
  /** The winning test-id value, whichever attribute carried it. */
  testId?: string
  component?: string
  owners?: readonly string[]
  id?: string
  text?: string
  classes?: readonly string[]
  /** `aria-label`, `name`, `placeholder`, `alt`, `href` — in that order. */
  attrs?: ReadonlyArray<{ name: string; value: string }>
  /** Demotes text below class, and firms up its caveat. See `detectI18n`. */
  i18n?: boolean
}

/**
 * Best-first, deduplicated, capped at `MAX_ANCHORS`.
 *
 * The order is the whole design. A `data-testid` was typed by a person for
 * exactly this purpose and is unique by convention; a component name is
 * function-level and measures best for repair; an id is good when it was not
 * minted this render; text is excellent *until the app is translated*; a class
 * is otherwise last because most of them are utilities.
 *
 * **On an internationalised page, text drops below class**, rather than merely
 * carrying a caveat. A caveat is a sentence the reader may act after; the rank
 * is what decides which anchor they grep *first*, and on a translated app the
 * visible string is not in the source at all. Ranking it top there spends the
 * reader's first search on a guaranteed miss.
 *
 * Six is the cap because a longer list stops being a ranking and becomes a
 * dump, and the reader greps the first plausible entry regardless of how many
 * follow.
 */
export function rankAnchors(input: AnchorInput): GrepAnchor[] {
  const out: GrepAnchor[] = []
  const seen = new Set<string>()
  const push = (value: string | undefined, kind: GrepAnchor['kind'], caveat?: string): void => {
    if (!value) return
    const trimmed = value.trim()
    if (!trimmed || seen.has(trimmed)) return
    if (out.length >= MAX_ANCHORS) return
    seen.add(trimmed)
    const anchor: GrepAnchor = { value: trimmed, kind }
    if (caveat) anchor.caveat = caveat
    out.push(anchor)
  }

  const i18n = !!input.i18n

  const pushText = (): void => {
    if (!input.text) return
    const collapsed = input.text.replace(/\s+/g, ' ').trim()
    // Only when short: a long string is wrapped, split across JSX expressions
    // or interpolated, so grepping it finds nothing while looking like the most
    // obvious handle in the list.
    if (!collapsed || collapsed.length > TEXT_ANCHOR_CAP) return
    push(collapsed, 'text', textCaveat(i18n))
  }

  const pushClasses = (): void => {
    // Authored classes only, at most two. Never a hash — see `classIsAuthored`.
    for (const cls of rankClasses(input.classes ?? [])) push(cls, 'class')
  }

  // 1. test ids — hand-authored, unique by convention, the best handle there is.
  push(input.testId, 'testid')

  // 2. the component, then its owners. `Button` is shared; its owner is what
  //    makes *this* instance findable.
  if (nameIsUseful(input.component)) push(input.component, 'component')
  for (const owner of input.owners ?? []) {
    if (nameIsUseful(owner)) push(owner, 'component')
  }

  // 3. the id, only when it was not minted this render.
  if (input.id && idIsUsableAnchor(input.id)) push(input.id, 'id')

  // 4/5. text and classes, in whichever order the page's language warrants.
  if (i18n) {
    pushClasses()
    pushText()
  } else {
    pushText()
    pushClasses()
  }

  // 6. the remaining labelled attributes, in the order they are worth grepping.
  for (const attr of input.attrs ?? []) {
    if (!attr || typeof attr.value !== 'string') continue
    const value = attr.name === 'href' ? hrefPath(attr.value) : attr.value.trim()
    if (!value) continue
    push(value, 'attr', attr.name === 'aria-label' ? textCaveat(i18n) : undefined)
  }

  return out
}

/* ------------------------------------------- computed styles, plus state */

/** Properties that can carry a *visual* complaint. Nothing else: dumping all
 *  300 buys localisation at the price of instruction-following, and an agent
 *  reading four screens of style has lost the sentence the operator wrote. */
const COMPUTED_KEYS = [
  'display',
  'position',
  'width',
  'height',
  'color',
  'background-color',
  'opacity',
  'visibility',
  'overflow',
  'z-index',
  'font-size',
  'pointer-events',
  'cursor',
  'flex-direction',
  'grid-template-columns',
] as const

/**
 * A value equal to the property's initial state says nothing — it is the
 * absence of a decision, and reporting it is noise that reads exactly like a
 * finding.
 *
 * **Per property, never a shared list of boring words.** The first version of
 * this skipped `none` and `auto` globally, which threw away
 * `pointer-events: none` and `display: none` — the two values most likely to be
 * the entire answer to "why doesn't this button work" and "why can't I see it".
 * A word is only boring against the property it is the initial value *of*.
 */
const CSS_INITIAL: Record<string, string> = {
  display: 'inline',
  position: 'static',
  width: 'auto',
  height: 'auto',
  'background-color': 'rgba(0, 0, 0, 0)',
  opacity: '1',
  visibility: 'visible',
  overflow: 'visible',
  'z-index': 'auto',
  'pointer-events': 'auto',
  cursor: 'auto',
  'flex-direction': 'row',
  'grid-template-columns': 'none',
  // `color` and `font-size` have no boring value: whatever they resolved to is
  // the thing a complaint about colour or size is about.
}

const MAX_COMPUTED = 12

/** Pure so the curation can be pinned without a browser: the caller supplies a
 *  reader, which is `getComputedStyle().getPropertyValue` in the page and a
 *  plain lookup in a harness. */
export function curateComputed(read: (prop: string) => string | undefined): Record<string, string> {
  const out: Record<string, string> = {}
  let count = 0
  for (const key of COMPUTED_KEYS) {
    if (count >= MAX_COMPUTED) break
    const value = safe(() => read(key), undefined)
    if (typeof value !== 'string') continue
    const trimmed = value.trim()
    if (!trimmed) continue
    if (CSS_INITIAL[key] === trimmed) continue
    out[key] = trimmed
    count += 1
  }
  return out
}

/**
 * The state the styles came from.
 *
 * **A computed style without its state is a documented trap.** The best-known
 * wrong edit in this field is an agent being shown a button's grey, not being
 * told the button was `:disabled`, and hardcoding the grey into the component —
 * which "fixes" the screenshot and breaks the enabled button for everyone. The
 * same applies to `:hover` and `:focus`, which a screenshot taken under the
 * operator's own pointer will frequently be in.
 *
 * Reported only when *on*: `aria-expanded="false"` is a real fact and stays,
 * but "not hovered" is the ordinary case and says nothing.
 */
export interface InteractionState {
  disabled?: boolean
  ariaDisabled?: string
  ariaExpanded?: string
  hover?: boolean
  focus?: boolean
}

export function describeState(state: InteractionState): Record<string, string> {
  const out: Record<string, string> = {}
  if (state.disabled) out['state:disabled'] = 'true'
  if (state.ariaDisabled) out['aria-disabled'] = state.ariaDisabled
  if (state.ariaExpanded) out['aria-expanded'] = state.ariaExpanded
  if (state.hover) out['state:hover'] = 'true'
  if (state.focus) out['state:focus'] = 'true'
  return out
}

function readState(el: Element): InteractionState {
  const state: InteractionState = {}
  // `:disabled` rather than the attribute alone: a `<button>` inside a disabled
  // `<fieldset>` is disabled and carries no attribute of its own.
  state.disabled = safe(() => el.matches(':disabled'), false)
  state.hover = safe(() => el.matches(':hover'), false)
  // `:focus-visible` is narrower and misses a click-focused control, which is
  // exactly the one the operator just pointed at.
  state.focus = safe(() => el.matches(':focus'), false)
  const ariaDisabled = safe(() => el.getAttribute('aria-disabled'), null)
  if (typeof ariaDisabled === 'string' && ariaDisabled) state.ariaDisabled = ariaDisabled
  const ariaExpanded = safe(() => el.getAttribute('aria-expanded'), null)
  if (typeof ariaExpanded === 'string' && ariaExpanded) state.ariaExpanded = ariaExpanded
  return state
}

/** Styles first, then the state that explains them. The state entries are few
 *  and sit outside `MAX_COMPUTED`: capping them away would drop precisely the
 *  field that stops the wrong edit. */
export function computedFor(el: Element): Record<string, string> {
  return safe(() => {
    const style = window.getComputedStyle(el as HTMLElement)
    const styles = style ? curateComputed((prop) => style.getPropertyValue(prop)) : {}
    return { ...styles, ...describeState(readState(el)) }
  }, {})
}

/* ---------------------------------------------- build-time data attributes */

/**
 * The only detection that works from here — and, given the header, the only one
 * worth writing in this file at all.
 *
 * Order matters: the specific stamps first, the generic catches last, because a
 * project that carries two of them is carrying one from its framework plugin
 * and one from a generic tool, and the framework's is the more precise.
 */
type AttrProbe = (el: Element) => SourceHint | undefined

function attrOf(el: Element, name: string): string | undefined {
  return safe(() => {
    const value = el.getAttribute(name)
    return typeof value === 'string' && value.length ? value : undefined
  }, undefined)
}

function hintFromLoc(via: string, loc: SourceLoc | undefined): SourceHint {
  const hint: SourceHint = { framework: 'attribute', via }
  if (loc) {
    hint.file = loc.file
    if (loc.line !== undefined) hint.line = loc.line
    if (loc.column !== undefined) hint.column = loc.column
  }
  return hint
}

/** code-inspector — `path:line:column:tag`, parsed right to left. */
const probeInspPath: AttrProbe = (el) => {
  const raw = attrOf(el, 'data-insp-path')
  if (!raw) return undefined
  return hintFromLoc('data-insp-path', parseInspPath(raw))
}

/** react-dev-inspector — three separate attributes, already split for us, so
 *  there is no parse to get wrong. */
const probeReactDevInspector: AttrProbe = (el) => {
  const file = attrOf(el, 'data-inspector-relative-path')
  if (!file) return undefined
  const clean = file.replace(/[?#].*$/, '')
  const line = finiteOrUndefined(attrOf(el, 'data-inspector-line'))
  const column = finiteOrUndefined(attrOf(el, 'data-inspector-column'))
  const hint: SourceHint = { framework: 'attribute', via: 'data-inspector-relative-path' }
  if (clean && !looksLikeBundle(clean, line, column)) {
    hint.file = clean
    if (line !== undefined) hint.line = line
    if (column !== undefined) hint.column = column
  }
  return hint
}

/** vue-inspector — `file:line:column`. */
const probeVueInspector: AttrProbe = (el) => {
  const raw = attrOf(el, 'data-v-inspector')
  if (!raw) return undefined
  return hintFromLoc('data-v-inspector', parseSourceLoc(raw))
}

/**
 * Onlook — `data-oid` plus `data-ocname`.
 *
 * `data-oid` is an **opaque 7-character nanoid**, not a path: it is a key into
 * Onlook's own map and means nothing to anyone holding only the DOM. So the
 * component name is taken from `data-ocname` and **no `file` is emitted at
 * all**. Do not try to decode the oid — a path derived from a random id is a
 * fabricated path, which is the one thing this file must never produce.
 */
const probeOnlook: AttrProbe = (el) => {
  const oid = attrOf(el, 'data-oid')
  const name = attrOf(el, 'data-ocname')
  if (!oid && !name) return undefined
  const hint: SourceHint = { framework: 'attribute', via: oid ? 'data-oid' : 'data-ocname' }
  if (nameIsUseful(name)) hint.component = name
  return hint
}

/** Lovable — `data-lov-id` carries the locator, `data-path` the plain file. */
const probeLovable: AttrProbe = (el) => {
  const id = attrOf(el, 'data-lov-id')
  const path = attrOf(el, 'data-path')
  if (!id && !path) return undefined
  const loc = parseSourceLoc(id ?? path ?? '') ?? (path ? parseSourceLoc(path) : undefined)
  const hint = hintFromLoc(id ? 'data-lov-id' : 'data-path', loc)
  const name = attrOf(el, 'data-lov-name') ?? attrOf(el, 'data-component-name')
  if (nameIsUseful(name)) hint.component = name
  return hint
}

/** The generic catches. Anything that stamps a `path:line:col` under a name we
 *  have seen used; parsed the same way, reported under its own name so the
 *  reader can judge it. */
const GENERIC_LOC_ATTRS = ['data-carbon8r', 'data-source', 'data-source-loc', 'data-dev-source'] as const

const probeGeneric: AttrProbe = (el) => {
  for (const name of GENERIC_LOC_ATTRS) {
    const raw = attrOf(el, name)
    if (!raw) continue
    // A stamp we could not parse — or that parsed to build output — is still
    // reported, as an attribute hint with no file. The `via` tells the reader
    // which attribute to go and look at themselves.
    return hintFromLoc(name, parseSourceLoc(raw))
  }
  return undefined
}

const PROBES: AttrProbe[] = [
  probeInspPath,
  probeReactDevInspector,
  probeVueInspector,
  probeOnlook,
  probeLovable,
  probeGeneric,
]

/** Ten is enough to escape the anonymous wrapper divs a component tree wraps a
 *  leaf in, and short enough that a pick in a deep page costs nothing. Beyond
 *  that the stamp found belongs to a layout, not to the thing pointed at. */
const ATTR_WALK_DEPTH = 10

/**
 * Walks up from the element looking for a build-time stamp.
 *
 * Returns `undefined` when there is none — which is the ordinary case, because
 * most projects are not built with any of these plugins. That is a correct and
 * honest answer, and far better than the alternative: a `SourceHint` the prompt
 * presents as a location and an agent acts on.
 */
export function sourceHintFor(el: Element): SourceHint | undefined {
  if (!el || typeof el !== 'object') return undefined
  return safe(() => {
    let node: Element | null = el
    let depth = 0
    while (node && depth < ATTR_WALK_DEPTH) {
      for (const probe of PROBES) {
        const hint = safe(() => probe(node as Element), undefined)
        if (hint) return hint
      }
      node = safe(() => node!.parentElement, null)
      depth += 1
    }
    return undefined
  }, undefined)
}

/* ------------------------------------------------- the anchors, from a DOM */

const TEST_ID_ATTRS = ['data-testid', 'data-test', 'data-cy', 'data-qa'] as const
const LABEL_ATTRS = ['aria-label', 'name', 'placeholder', 'alt', 'href'] as const

/**
 * The DOM-reading half. Everything it gathers goes through `rankAnchors`, which
 * holds the rules and is the part a harness can reach.
 *
 * `text` is passed in rather than re-read: the caller already computed and
 * capped it, and two definitions of "this element's text" is two answers that
 * drift.
 */
export function anchorsFor(
  el: Element,
  hint: SourceHint | undefined,
  text: string | undefined,
): GrepAnchor[] {
  return safe(() => {
    const input: AnchorInput = { i18n: detectI18n() }

    for (const name of TEST_ID_ATTRS) {
      const value = attrOf(el, name)
      if (value) {
        input.testId = value
        break
      }
    }

    if (hint && hint.component) input.component = hint.component
    if (hint && hint.owners) input.owners = hint.owners

    const id = attrOf(el, 'id')
    if (id) input.id = id
    if (text) input.text = text

    input.classes = safe(() => {
      const raw = el.getAttribute('class')
      if (typeof raw !== 'string') return []
      return raw.split(/\s+/).filter((c) => c.length > 0)
    }, [])

    const attrs: Array<{ name: string; value: string }> = []
    for (const name of LABEL_ATTRS) {
      const value = attrOf(el, name)
      if (value) attrs.push({ name, value })
    }
    input.attrs = attrs

    return rankAnchors(input)
  }, [])
}
