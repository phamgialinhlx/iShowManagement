/**
 * The window can be moved.
 *
 * `titleBarStyle: 'hiddenInset'` removes the native title bar on macOS, so the
 * window is immovable unless the page declares `-webkit-app-region: drag`
 * somewhere. R Browser shipped its first build without one: the window could
 * not be dragged at all, and the 78px left inset reserved for the traffic
 * lights read as a title bar that did nothing — an affordance offering a
 * gesture it could not answer. It was reported the first time the app was
 * opened.
 *
 * **This is checked syntactically because nothing else can see it.** It is not
 * a type error, it is not a build failure, it throws nothing at runtime, and a
 * harness that renders the DOM cannot feel whether a window moved. The same
 * reasoning as rmux's `tests/window_drag.rs`, which guards the Tauri ACL grant
 * for `allow-start-dragging`, and `src-tauri/tests/no_console_window.rs`: when
 * a rule is invisible to every automatic gate, the gate has to read the source.
 */
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

// Set by `scripts/run-checks.mjs`. This harness is bundled into a temp
// directory before it runs, so `import.meta.url` points at the bundle rather
// than at the source it exists to read.
const root = process.env['RB_ROOT'] ?? process.cwd()
const css = readFileSync(join(root, 'renderer/styles.css'), 'utf8')
const tabstrip = readFileSync(join(root, 'renderer/components/TabStrip.tsx'), 'utf8')
const main = readFileSync(join(root, 'main/index.ts'), 'utf8')

let failed = 0
const ok = (cond: boolean, what: string) => {
  if (!cond) failed++
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${what}`)
}

console.log('the window can be moved\n')

/** Strip comments, or a rule merely *described* in prose counts as declared. */
const rules = css.replace(/\/\*[\s\S]*?\*\//g, '')

const hidesTitleBar = /titleBarStyle:\s*.*hiddenInset/.test(main)
ok(hidesTitleBar, 'main still uses a hiddenInset titlebar (if this flips, re-read this check)')

// The rule that was missing. `drag` must appear as a real declaration, not
// inside a comment explaining that it ought to.
const dragDecls = rules.match(/-webkit-app-region:\s*drag/g) ?? []
ok(dragDecls.length > 0, `a drag region is declared (found ${dragDecls.length})`)

// A drag region the tabs live inside would make every tab unclickable, because
// macOS routes the press to the window move instead of to the control.
ok(
  /\.tabstrip\s*\{[^}]*-webkit-app-region:\s*no-drag/s.test(rules),
  'the scrolling tab strip opts out, so tabs stay clickable',
)

// The handle must survive a full strip. `flex-shrink` other than 0 lets a row
// of tabs squeeze it to nothing, and a window with many tabs is the one the
// operator most wants to move.
const handle = /\.tabbar-drag\s*\{([^}]*)\}/s.exec(rules)
ok(!!handle, 'a dedicated drag handle exists')
ok(
  !!handle && /flex:\s*0\s+0\s+\d/.test(handle[1] ?? ''),
  `the handle cannot be shrunk away by a full tab strip (got: ${handle?.[1]?.trim().replace(/\s+/g, ' ') ?? 'nothing'})`,
)

// It must be outside the scroller, or it scrolls out of reach exactly when the
// strip is full — the case it exists for.
const markup = tabstrip.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\{\/\*[\s\S]*?\*\/\}/g, '')
const stripOpen = markup.indexOf('className="tabstrip"')
const stripClose = markup.indexOf('tabbar-drag')
ok(stripOpen !== -1 && stripClose !== -1 && stripClose > stripOpen, 'the handle is rendered after the strip')

// The traffic lights need their room, and that room should drag rather than
// sit dead.
ok(/\.is-mac\s+\.tabbar-inset\s*\{[^}]*flex-basis:\s*\d+px/s.test(rules), 'macOS reserves room for the traffic lights')
ok(
  /\.tabbar-inset\s*\{[^}]*\}/s.test(rules) && markup.includes('tabbar-inset'),
  'that room is a rendered element, so it inherits the drag region',
)
ok(
  !/\.is-mac\s+\.tabstrip\s*\{[^}]*margin-left/s.test(rules),
  'the old dead-space margin is gone (it looked like a titlebar and did nothing)',
)

console.log(`\n${failed ? `${failed} FAILED` : 'all passed'}`)
process.exit(failed ? 1 : 0)
