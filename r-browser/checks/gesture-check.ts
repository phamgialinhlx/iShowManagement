/**
 * The pick gesture, as a sequence.
 *
 * Every case below is a real ordering the browser produces, and the first one
 * is a regression test for a bug that shipped into the first version of the
 * picker: a single pick let `click` through and the link navigated.
 */
import { openGesture, isOpen, shouldSwallow, afterEvent, GESTURE_MS, type GestureState } from '../page/gesture'

let failed = 0
const ok = (cond: boolean, what: string) => {
  if (!cond) failed++
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${what}`)
}

console.log('pick gesture\n')

// --- the regression -------------------------------------------------------
// Single pick: the picker disarms on pointerdown, so `active` is false for
// everything that follows. Without the latch every one of these reaches the
// page and a link navigates.
{
  const t = 1_000_000
  let g: GestureState = openGesture(t)
  let active = false // disarm() ran during pointerdown

  ok(shouldSwallow(active, g, t + 1), 'single pick: mousedown is swallowed after disarm')
  g = afterEvent('mousedown', g)
  ok(shouldSwallow(active, g, t + 12), 'single pick: mouseup is swallowed after disarm')
  g = afterEvent('mouseup', g)
  ok(shouldSwallow(active, g, t + 20), 'single pick: CLICK is swallowed after disarm — the link must not navigate')
  g = afterEvent('click', g)
  ok(!shouldSwallow(active, g, t + 21), 'the latch drops once click has been consumed')
  void active
}

// --- multi-pick -----------------------------------------------------------
// The case that hid the bug: still armed, so `active` alone would have done.
{
  const t = 2_000_000
  let g: GestureState = openGesture(t)
  const active = true
  ok(shouldSwallow(active, g, t + 20), 'multi pick: click is swallowed while still armed')
  g = afterEvent('click', g)
  ok(shouldSwallow(active, g, t + 21), 'multi pick: still swallowing, because still armed')
}

// --- the abandoned gesture ------------------------------------------------
// Pointer leaves the window between down and up, so `click` never arrives and
// nothing ever closes the latch. It must close itself.
{
  const t = 3_000_000
  const g = openGesture(t)
  ok(isOpen(g, t + GESTURE_MS - 1), 'an abandoned gesture is still closed just before the deadline')
  ok(!isOpen(g, t + GESTURE_MS), 'an abandoned gesture expires on its own')
  ok(!shouldSwallow(false, g, t + GESTURE_MS + 5000), 'a much later, unrelated click is not swallowed')
}

// --- never armed ----------------------------------------------------------
{
  const g: GestureState = { openUntil: 0 }
  ok(!shouldSwallow(false, g, 4_000_000), 'with the picker off, the page gets its own clicks')
}

console.log(`\n${failed ? `${failed} FAILED` : 'all passed'}`)
process.exit(failed ? 1 : 0)
