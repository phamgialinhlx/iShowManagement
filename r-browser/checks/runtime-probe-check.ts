/**
 * The main-world probe.
 *
 * Two things are pinned here and the first one is the interesting one.
 *
 * **The expression is a string, so nothing type-checks it.** A stray brace or a
 * bad regex in `PROBE_EXPRESSION` would compile fine, ship fine, and then fail
 * at runtime as `exceptionDetails` — which `evaluate` deliberately turns into
 * `undefined` so a broken probe cannot lose a pick. The failure is therefore
 * *completely silent*: every pick would simply have no component name, forever,
 * and nothing anywhere would say why. So the syntax is checked here, by
 * compiling it.
 *
 * **And its output is untrusted.** The expression runs in a document we do not
 * control, which can shadow `document.querySelector` and return anything at
 * all. `toSourceHint` is the narrowing, and these cases are what it must refuse.
 */
import {
  PROBE_EXPRESSION, CLEAR_EXPRESSION, PICK_ATTR, toSourceHint, looksLikeBundlePath,
} from '../main/runtime-probe'

let failed = 0
const ok = (what: string, cond: boolean, detail?: string) => {
  if (!cond) failed++
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${what}${cond || !detail ? '' : `\n      ${detail}`}`)
}

console.log('main-world runtime probe\n')

/* ------------------------------------------------------ the expression itself */

let compiled = ''
try {
  // Not executed — there is no DOM here and faking one would prove nothing.
  // Compiling is exactly the half that a string loses and the half that fails
  // silently.
  new Function(`return ${PROBE_EXPRESSION}`)
  new Function(`return ${CLEAR_EXPRESSION}`)
  compiled = 'ok'
} catch (e) {
  compiled = e instanceof Error ? e.message : String(e)
}
ok('1. the probe expression is syntactically valid JavaScript', compiled === 'ok', compiled)

ok('2. both expressions address the same marker attribute',
  PROBE_EXPRESSION.includes(PICK_ATTR) && CLEAR_EXPRESSION.includes(PICK_ATTR))

ok('3. the probe is one expression, so Runtime.evaluate returns its value',
  PROBE_EXPRESSION.trim().startsWith('(()') && PROBE_EXPRESSION.trim().endsWith(')()'))

ok('4. React is probed by _debugOwner, not by the render tree',
  PROBE_EXPRESSION.includes('_debugOwner'),
  'the owner chain is who wrote the JSX, which is what a reader can grep for')

// Property *access*, not a mention: the `via` string deliberately names
// `_debugSource` so the reader of a payload understands why there is a
// component but no file. Matching the bare word flagged that explanation,
// which is the check being wrong rather than the code.
ok('5. _debugSource is never actually read',
  !/[.\[]\s*['"]?_debugSource/.test(PROBE_EXPRESSION),
  'React 19 removed it; probing for it would be dead code that looks alive')

/* --------------------------------------------------------------- narrowing */

ok('6. a well-formed Svelte hint survives',
  toSourceHint({ framework: 'svelte', via: 'Svelte __svelte_meta.loc', file: 'src/Todo.svelte', line: 15, column: 5 })?.file
    === 'src/Todo.svelte')

ok('7. a bundle path is refused, and takes its line with it',
  (() => {
    const h = toSourceHint({ framework: 'vue', via: 'x', component: 'Card', file: '/assets/index-a3f1c9de.js', line: 1 })
    return h?.component === 'Card' && h.file === undefined && h.line === undefined
  })(),
  'a path into build output is the worst thing this payload can carry — the agent edits a file that is regenerated on save')

ok('8. a line with no file is dropped',
  toSourceHint({ framework: 'react', via: 'x', component: 'Btn', line: 42 })?.line === undefined,
  'a line number floating free of a path is not a location')

ok('9. a hint with neither component nor file is not worth emitting',
  toSourceHint({ framework: 'react', via: 'React fiber (no named owner found)' }) === undefined)

ok('10. junk from a hostile page is refused',
  toSourceHint(null) === undefined &&
  toSourceHint('component') === undefined &&
  toSourceHint({ framework: 42, via: {} }) === undefined &&
  toSourceHint({ framework: 'react', via: 'x', component: { toString: () => 'Evil' } }) === undefined)

ok('11. owners are filtered and capped',
  (() => {
    const h = toSourceHint({
      framework: 'react', via: 'x', component: 'A',
      owners: ['B', 'C', null, 42, 'D', 'E', 'F'],
    })
    return h?.owners?.length === 4 && h.owners[0] === 'B' && h.owners[3] === 'E'
  })())

ok('12. an absurdly long component name is refused rather than truncated',
  toSourceHint({ framework: 'react', via: 'x', component: 'X'.repeat(500) })?.component === undefined,
  'a page can return a megabyte; silently truncating it would put page-chosen text in the payload')

/* ------------------------------------------------------------ bundle paths */

const bundles = [
  '/assets/index-a3f1c9de.js',
  'https://app.example.com/_next/static/chunks/main.js',
  '/node_modules/.vite/deps/react_jsx-dev-runtime.js',
  'dist/app.js',
  'build/static/js/2.chunk.js',
]
const sources = [
  'src/components/Button.tsx',
  'src/routes/+page.svelte',
  'app/settings/page.tsx',
  'C:\\src\\components\\Button.tsx',
  'packages/ui/src/Card.vue',
]
ok('13. every bundle-shaped path is rejected',
  bundles.every(looksLikeBundlePath),
  bundles.filter((b) => !looksLikeBundlePath(b)).join(', '))
ok('14. no real source path is rejected',
  sources.every((p) => !looksLikeBundlePath(p)),
  sources.filter(looksLikeBundlePath).join(', '))

console.log(`\n${failed ? `${failed} FAILED` : 'all passed'}`)
process.exit(failed ? 1 : 0)
