/**
 * Source hints and grep anchors — the parts that are decidable without a DOM.
 *
 * What is pinned here is exactly what a browser could not tell us anyway: the
 * parse of a locator attribute, the rejection of generated strings, and the
 * ranking. The DOM-reading halves (`sourceHintFor`'s walk, `anchorsFor`'s
 * attribute reads, `computedFor`'s `getComputedStyle`) are not simulable and a
 * harness that faked them would pass on precisely the output that is broken —
 * the same rule `scripts/run-checks.mjs` already states.
 *
 * Two cases here are regressions waiting to happen rather than examples:
 *
 *   - **The Windows path.** `C:\src\Button.tsx:42:7:button` parsed left to
 *     right yields the path `C` — a file that exists nowhere, named plausibly
 *     enough that nobody questions it. code-inspector parses from the right and
 *     so do we.
 *   - **The bundle path.** `/assets/index-a3f1.js:1:2841` must produce no
 *     `file` at all. Emitting it invites an agent to edit build output, where
 *     the change is overwritten by the next build and appears in no diff anyone
 *     reviews.
 *
 * And one that is a rule rather than a case: a **hashed class is never emitted
 * as an anchor**. It is not merely useless — it sends the reader to grep for a
 * string the tree does not contain, and an empty search reads as "the element
 * is not in this repo".
 */
import {
  RUNTIME_PROBES_NEED_MAIN_WORLD,
  classIsAuthored,
  classLooksSemantic,
  curateComputed,
  describeState,
  hrefPath,
  idIsUsableAnchor,
  looksLikeBundle,
  nameIsUseful,
  parseInspPath,
  parseSourceLoc,
  rankAnchors,
  rankClasses,
} from '../page/source-hint'

let failed = 0
const ok = (cond: boolean, what: string) => {
  if (!cond) failed++
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${what}`)
}

console.log('source hints & grep anchors\n')

// --- the isolated world ---------------------------------------------------
// Not a behaviour, a statement: runtime framework probes are absent from this
// file on purpose and the constant is where that is recorded.
ok(RUNTIME_PROBES_NEED_MAIN_WORLD === true, 'runtime probes are documented as main-world-only')

// --- data-insp-path, parsed from the right --------------------------------
{
  const win = parseInspPath('C:\\src\\components\\Button.tsx:42:7:button')
  ok(win?.file === 'C:\\src\\components\\Button.tsx', 'windows drive colon survives the parse')
  ok(win?.line === 42 && win?.column === 7, 'windows stamp keeps line and column')

  const posix = parseInspPath('/Users/na/app/src/App.tsx:10:5:div')
  ok(posix?.file === '/Users/na/app/src/App.tsx', 'a posix absolute path keeps its leading slash')
  ok(posix?.line === 10 && posix?.column === 5, 'posix stamp keeps line and column')

  const noTag = parseInspPath('src/App.tsx:3:1')
  ok(noTag?.file === 'src/App.tsx' && noTag.line === 3, 'a stamp with no tag field still parses')
}

// --- never a bundle path --------------------------------------------------
{
  ok(parseSourceLoc('/assets/index-a3f1.js:1:2841') === undefined, 'BUNDLE PATH YIELDS NO file')
  ok(parseInspPath('/assets/index-a3f1.js:1:2841:div') === undefined, 'nor through the insp parse')
  ok(looksLikeBundle('dist/main.js'), 'dist/ is build output')
  ok(looksLikeBundle('_next/static/chunks/main.js'), 'next static chunks are build output')
  ok(looksLikeBundle('js/app.4f2c9ab1.js'), 'a content hash in the filename is build output')
  ok(looksLikeBundle('vendor.min.js'), 'a minified file is build output')
  ok(looksLikeBundle('anything.js', 1, 2841), 'line 1 column 2841 is minified whatever it is called')
  ok(!looksLikeBundle('src/components/Button.tsx', 42, 7), 'a dev-server source path is not a bundle')
  ok(!looksLikeBundle('src/outline/Panel.tsx'), '"out" inside a word is not the out/ directory')
}

// --- a vite dev URL is a project path -------------------------------------
{
  const loc = parseSourceLoc('http://localhost:5173/src/components/Button.tsx:42:7')
  ok(loc?.file === 'src/components/Button.tsx', 'the dev-server origin is stripped to a project path')
  ok(loc?.line === 42 && loc?.column === 7, 'and the position survives')
}

// --- generated ids --------------------------------------------------------
{
  ok(!idIsUsableAnchor(':r7:'), 'a React useId is rejected')
  ok(!idIsUsableAnchor('radix-«r0»'), 'a Radix id is rejected')
  ok(!idIsUsableAnchor('item-908'), 'a trailing counter is rejected')
  ok(!idIsUsableAnchor('menu-9f3ba21c'), 'an 8+ hex run is rejected')
  ok(!idIsUsableAnchor('two words'), 'whitespace is rejected')
  ok(idIsUsableAnchor('checkout-form'), 'a hand-written id is kept')
  ok(idIsUsableAnchor('main-nav'), 'and another')
}

// --- generated classes ----------------------------------------------------
{
  ok(!classIsAuthored('Button_primary__3xYz'), 'a CSS-modules hash is rejected')
  ok(!classIsAuthored('sc-bdVaJa'), 'a styled-components class is rejected')
  ok(!classIsAuthored('css-1a2b3c'), 'an emotion class is rejected')
  ok(!classIsAuthored('emotion-9fd21a'), 'and emotion under its own prefix')
  ok(!classIsAuthored('grid-1024-wide'), 'a 4+ digit run is rejected')
  ok(classIsAuthored('btn__label'), 'BEM with no digits survives — it is authored')
  ok(classIsAuthored('btn--primary'), 'a BEM modifier survives')
  ok(classIsAuthored('mt-6'), 'a Tailwind utility survives — it is literally in the JSX')
  ok(classIsAuthored('text-2xl'), 'and a scale step')
  ok(classLooksSemantic('btn--primary'), 'a BEM modifier reads as semantic')
  ok(!classLooksSemantic('mt-6'), 'a spacing utility does not')
  ok(!classLooksSemantic('flex'), 'nor a bare display utility')
}

// --- class ranking --------------------------------------------------------
{
  const ranked = rankClasses(['mt-6', 'css-1a2b3c', 'btn--primary', 'flex', 'card__body'])
  ok(ranked.length === 2, 'at most two classes survive ranking')
  ok(ranked[0] === 'btn--primary' && ranked[1] === 'card__body', 'semantic classes outrank utilities')
  ok(ranked.indexOf('css-1a2b3c') === -1, 'the hash never appears')
}

// --- the ranking, end to end ----------------------------------------------
{
  const anchors = rankAnchors({
    testId: 'checkout-submit',
    component: 'SubmitButton',
    owners: ['CheckoutForm', '_Anon', 'Unknown'],
    id: ':r7:',
    text: 'Place order',
    classes: ['css-1a2b3c', 'btn--primary', 'mt-6', 'sc-bdVaJa'],
    attrs: [
      { name: 'aria-label', value: 'Place order' },
      { name: 'href', value: 'https://shop.example.com/checkout?step=2' },
    ],
  })
  const values = anchors.map((a) => a.value)
  ok(anchors[0]?.kind === 'testid', 'the test id ranks first')
  ok(anchors[0]?.value === 'checkout-submit', 'and carries its value')
  ok(anchors[1]?.value === 'SubmitButton' && anchors[1].kind === 'component', 'component next')
  ok(values.indexOf('CheckoutForm') === 2, 'then the owner that makes the instance findable')
  ok(values.indexOf('_Anon') === -1 && values.indexOf('Unknown') === -1, 'noise names are dropped')
  ok(values.indexOf(':r7:') === -1, 'the generated id never appears')
  ok(values.indexOf('css-1a2b3c') === -1, 'A HASHED CLASS IS NEVER EMITTED AS AN ANCHOR')
  ok(values.indexOf('sc-bdVaJa') === -1, 'nor the styled-components one')
  ok(anchors.length <= 6, 'the list is capped at six')
  ok(values.indexOf('Place order') < values.indexOf('btn--primary'), 'text outranks class in English')
  const text = anchors.find((a) => a.kind === 'text')
  ok(!!text?.caveat, 'the text anchor always carries its i18n caveat')
}

// --- the i18n demotion ----------------------------------------------------
// A caveat is a sentence the reader may act after; the rank decides what they
// grep *first*, and on a translated app the visible string is not in the source
// at all.
{
  const anchors = rankAnchors({
    text: 'Zur Kasse',
    classes: ['btn--primary'],
    i18n: true,
  })
  const values = anchors.map((a) => a.value)
  ok(values.indexOf('btn--primary') < values.indexOf('Zur Kasse'), 'i18n drops text BELOW class')
  ok(anchors.find((a) => a.kind === 'text')?.caveat?.includes('locale files') === true,
     'and the caveat says where the string actually lives')
}

// --- text and href shaping ------------------------------------------------
{
  const long = 'x'.repeat(200)
  ok(rankAnchors({ text: long }).length === 0, 'a long string is not an anchor — it greps to nothing')
  ok(rankAnchors({ text: '  Sign   in  ' })[0]?.value === 'Sign in', 'whitespace is collapsed')
  ok(hrefPath('https://shop.example.com/checkout?step=2') === '/checkout', 'href keeps the path only')
  ok(hrefPath('/pricing') === '/pricing', 'a relative href is already a path')
  ok(hrefPath('#') === undefined, 'a bare fragment is not an anchor')
  ok(hrefPath('javascript:void(0)') === undefined, 'nor a javascript: href')
  ok(hrefPath('/') === undefined, 'nor the site root, which locates nothing')
}

// --- names ----------------------------------------------------------------
{
  ok(nameIsUseful('CheckoutForm'), 'a component name is useful')
  ok(!nameIsUseful('_c'), 'an internal wrapper is not')
  ok(!nameIsUseful('Unknown'), 'nor a placeholder')
  ok(!nameIsUseful(undefined), 'nor nothing at all')
}

// --- computed curation ----------------------------------------------------
{
  const values: Record<string, string> = {
    display: 'flex',
    position: 'static',
    opacity: '1',
    visibility: 'visible',
    'z-index': 'auto',
    'background-color': 'rgba(0, 0, 0, 0)',
    color: 'rgb(120, 120, 120)',
    'flex-direction': 'row',
    'pointer-events': 'none',
  }
  const out = curateComputed((prop) => values[prop])
  ok(out.display === 'flex', 'a decision is reported')
  ok(out.color === 'rgb(120, 120, 120)', 'and a colour that was set')
  ok(out['pointer-events'] === 'none', '"none" is a finding for pointer-events')
  ok(!('position' in out), 'static is the absence of a decision')
  ok(!('opacity' in out), 'so is opacity 1')
  ok(!('visibility' in out), 'and visibility visible')
  ok(!('z-index' in out), 'and an auto z-index')
  ok(!('background-color' in out), 'and a transparent background')
  ok(!('flex-direction' in out), 'and the default flex direction')
  ok(Object.keys(out).length <= 12, 'the style set is capped')

  // The regression: a shared list of "boring words" skipped `none` everywhere,
  // which threw away the two values most likely to BE the complaint.
  const hidden = curateComputed((prop) => ({ display: 'none', overflow: 'hidden' })[prop])
  ok(hidden.display === 'none', 'display:none is a finding, not a default')
  ok(hidden.overflow === 'hidden', 'and so is a clipped overflow')
}

// --- the state that explains the styles -----------------------------------
// The documented wrong edit: an agent is shown a button's grey, is not told the
// button was disabled, and hardcodes the grey into the component.
{
  const on = describeState({ disabled: true, ariaExpanded: 'false', hover: true, focus: false })
  ok(on['state:disabled'] === 'true', 'a disabled control says so')
  ok(on['aria-expanded'] === 'false', 'aria-expanded="false" is a fact and is reported')
  ok(on['state:hover'] === 'true', 'and a hovered element, which a screenshot often is')
  ok(!('state:focus' in on), 'but "not focused" is the ordinary case and says nothing')
  ok(Object.keys(describeState({})).length === 0, 'a plain element reports no state at all')
}

console.log(`\n${failed ? `${failed} FAILED` : 'all passed'}`)
process.exit(failed ? 1 : 0)
