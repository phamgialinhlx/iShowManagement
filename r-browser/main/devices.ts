/**
 * R BROWSER — device presets.
 * ===========================
 *
 * **This list is curated, and staying short is the feature.** Chromium ships
 * something like ninety device profiles and every one of them is real; a picker
 * with ninety entries is a picker nobody scrolls, so the operator ends up using
 * whichever three are near the top and the other eighty-seven exist to make the
 * list slow. What a developer browser is actually for is *crossing a
 * breakpoint* — the width at which a layout changes its mind — and there are
 * about nine of those in practice:
 *
 *   360 · 390 · 430 · 768 · 834 · 1024 · 1280 · 1440 · 1920
 *
 * Every entry below earns its place by sitting on one of those, or by being the
 * device someone will name out loud in a bug report ("it breaks on my 14-inch").
 * A second handset that renders identically to one already here adds a row and
 * no coverage. If a device is genuinely missing, the emulation state carries
 * `customWidth`/`customHeight` for exactly that case — a free size is a better
 * answer than a ninety-item list.
 *
 * Dimensions are **CSS pixels in portrait**, looked up rather than guessed, and
 * rotation swaps them (`EmulationState.rotated`) rather than being a second
 * entry per device — otherwise this file doubles in length and says nothing new.
 *
 * `deviceScaleFactor` travels with them because it is not cosmetic: a 2560px
 * screenshot of a 1280px viewport reads as a desktop layout when it is a laptop
 * one, and an agent handed the pixel numbers with no DPR will reason about the
 * wrong width.
 *
 * `mobile` and `touch` are set **truthfully**, not by group. They are separate
 * facts and the group does not decide them: Surface Pro is a touch device that
 * is emphatically not `mobile` (setting the mobile flag gives it a phone
 * viewport meta behaviour it does not have), and a 4K signage panel is neither.
 * Getting this wrong changes `navigator.maxTouchPoints`, the `hover`/`pointer`
 * media queries and Chromium's mobile viewport handling all at once, which
 * surfaces as "the site looks nothing like it does on the real device".
 */

import type { DevicePreset } from '../shared/contract'

/* User agents are shared where the device shares one — a UA typed out six times
 * is a UA that drifts in one of the six. They are current-shape rather than
 * pinned to a build: the point is that the site's own sniffing takes the right
 * branch, and no site branches on a Chrome patch number. */
const UA_IOS_PHONE =
  'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 ' +
  '(KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1'

const UA_IOS_TABLET =
  'Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) AppleWebKit/605.1.15 ' +
  '(KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1'

const UA_ANDROID_PHONE = (model: string, release = '14') =>
  `Mozilla/5.0 (Linux; Android ${release}; ${model}) AppleWebKit/537.36 ` +
  '(KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36'

/* Note the absent `Mobile` token — that is the whole difference between an
 * Android tablet UA and an Android phone UA, and sites do read it. */
const UA_WINDOWS_TOUCH =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.0.0'

export const DEVICES: DevicePreset[] = [
  /* ------------------------------------------------------------------ phones
   * The four widths that matter are 360, 375, 390 and 412/430. Below 360 is
   * effectively extinct on the web; above 430 you are in tablet layout. */
  {
    id: 'android-small',
    label: 'Android (small)',
    group: 'phone',
    width: 360,
    height: 800,
    deviceScaleFactor: 3,
    mobile: true,
    touch: true,
    userAgent: UA_ANDROID_PHONE('SM-A155F', '13'),
  },
  {
    id: 'iphone-se',
    label: 'iPhone SE',
    group: 'phone',
    // The narrowest viewport still worth supporting, and the one that finds
    // every overflow bug first.
    width: 375,
    height: 667,
    deviceScaleFactor: 2,
    mobile: true,
    touch: true,
    userAgent: UA_IOS_PHONE,
  },
  {
    id: 'iphone-13',
    label: 'iPhone 13 / 14',
    group: 'phone',
    width: 390,
    height: 844,
    deviceScaleFactor: 3,
    mobile: true,
    touch: true,
    userAgent: UA_IOS_PHONE,
  },
  {
    id: 'iphone-14-pro-max',
    label: 'iPhone 14 / 15 Pro Max',
    group: 'phone',
    width: 430,
    height: 932,
    deviceScaleFactor: 3,
    mobile: true,
    touch: true,
    userAgent: UA_IOS_PHONE,
  },
  {
    id: 'pixel-7',
    label: 'Pixel 7',
    group: 'phone',
    width: 412,
    height: 915,
    // 2.625, not 3 — the fractional DPR is real and it is exactly the case
    // where a hairline border rounds to nothing on one device and 1px on
    // another. Rounding it to 3 here would hide that class of bug.
    deviceScaleFactor: 2.625,
    mobile: true,
    touch: true,
    userAgent: UA_ANDROID_PHONE('Pixel 7'),
  },
  {
    id: 'galaxy-s20-ultra',
    label: 'Galaxy S20 Ultra',
    group: 'phone',
    width: 412,
    height: 915,
    // Same CSS box as the Pixel, a very different DPR. Kept because 3.5 is the
    // high end and image `srcset` decisions are made against it.
    deviceScaleFactor: 3.5,
    mobile: true,
    touch: true,
    userAgent: UA_ANDROID_PHONE('SM-G988B', '13'),
  },

  /* ----------------------------------------------------------------- tablets
   * 768 is the classic `md` breakpoint; 820/834 is where most tablet layouts
   * actually land; 1024 portrait collides with the desktop breakpoint, which
   * is precisely why iPad Pro 12.9 is worth having in the list. */
  {
    id: 'ipad-mini',
    label: 'iPad mini',
    group: 'tablet',
    width: 768,
    height: 1024,
    deviceScaleFactor: 2,
    mobile: true,
    touch: true,
    userAgent: UA_IOS_TABLET,
  },
  {
    id: 'ipad-air',
    label: 'iPad Air',
    group: 'tablet',
    width: 820,
    height: 1180,
    deviceScaleFactor: 2,
    mobile: true,
    touch: true,
    userAgent: UA_IOS_TABLET,
  },
  {
    id: 'ipad-pro-11',
    label: 'iPad Pro 11"',
    group: 'tablet',
    width: 834,
    height: 1194,
    deviceScaleFactor: 2,
    mobile: true,
    touch: true,
    userAgent: UA_IOS_TABLET,
  },
  {
    id: 'ipad-pro-12-9',
    label: 'iPad Pro 12.9"',
    group: 'tablet',
    width: 1024,
    height: 1366,
    deviceScaleFactor: 2,
    mobile: true,
    touch: true,
    userAgent: UA_IOS_TABLET,
  },
  {
    id: 'surface-pro-7',
    label: 'Surface Pro 7',
    group: 'tablet',
    width: 912,
    height: 1368,
    deviceScaleFactor: 2,
    // Not `mobile`: it runs desktop Windows and desktop Edge, and flagging it
    // mobile would apply a viewport-meta behaviour the real device does not
    // have. It *is* touch, and `hover: none` is the interesting consequence.
    mobile: false,
    touch: true,
    userAgent: UA_WINDOWS_TOUCH,
  },

  /* ---------------------------------------------------------------- desktops
   * No `userAgent` override: these run the same Chromium R Browser is, so the
   * real UA is the accurate one. Overriding it here would be inventing a fact.
   * DPR is the thing that varies and it is the thing that is set. */
  {
    id: 'desktop-1280',
    label: '1280 × 800',
    group: 'desktop',
    width: 1280,
    height: 800,
    deviceScaleFactor: 1,
    mobile: false,
    touch: false,
  },
  {
    id: 'desktop-1440',
    label: '1440 × 900',
    group: 'desktop',
    width: 1440,
    height: 900,
    deviceScaleFactor: 2,
    mobile: false,
    touch: false,
  },
  {
    id: 'macbook-pro-14',
    label: 'MacBook Pro 14"',
    group: 'desktop',
    width: 1512,
    height: 982,
    deviceScaleFactor: 2,
    mobile: false,
    touch: false,
  },
  {
    id: 'macbook-pro-16',
    label: 'MacBook Pro 16"',
    group: 'desktop',
    width: 1728,
    height: 1117,
    deviceScaleFactor: 2,
    mobile: false,
    touch: false,
  },
  {
    id: 'desktop-1080p',
    label: '1920 × 1080 (FHD)',
    group: 'desktop',
    width: 1920,
    height: 1080,
    deviceScaleFactor: 1,
    mobile: false,
    touch: false,
  },
  {
    id: 'desktop-qhd',
    label: '2560 × 1440 (QHD)',
    group: 'desktop',
    width: 2560,
    height: 1440,
    deviceScaleFactor: 1,
    mobile: false,
    touch: false,
  },
  {
    id: 'desktop-4k',
    label: '3840 × 2160 (4K)',
    group: 'desktop',
    width: 3840,
    height: 2160,
    deviceScaleFactor: 2,
    mobile: false,
    touch: false,
  },

  /* ------------------------------------------------------------------ watch
   * Under ~250px nearly every grid, dialog and fixed-width control breaks, and
   * it breaks in a way no phone preset reveals. Watches are also the honest
   * test of whether a "responsive" layout has a floor or merely a slope. */
  {
    id: 'apple-watch-45',
    label: 'Apple Watch 45mm',
    group: 'watch',
    width: 198,
    height: 242,
    deviceScaleFactor: 2,
    mobile: true,
    touch: true,
  },
  {
    id: 'apple-watch-ultra',
    label: 'Apple Watch Ultra',
    group: 'watch',
    width: 205,
    height: 251,
    deviceScaleFactor: 2,
    mobile: true,
    touch: true,
  },
  {
    id: 'galaxy-watch',
    label: 'Galaxy Watch',
    group: 'watch',
    // Square, which is its own case: a layout tuned for a tall phone and a
    // short watch can still be wrong at 1:1.
    width: 196,
    height: 196,
    deviceScaleFactor: 2,
    mobile: true,
    touch: true,
  },

  /* ------------------------------------------------------------------ panel
   * Kiosks and signage. Both of these are landscape in real life, so they are
   * stored portrait like everything else and expected to be rotated — the
   * alternative is one device in this file that means something different by
   * `width`, which is worse than an extra click. */
  {
    id: 'panel-480',
    label: 'Small panel 480 × 320',
    group: 'panel',
    width: 480,
    height: 320,
    deviceScaleFactor: 1,
    mobile: false,
    // Kiosk panels are touch and have no pointer; that combination is what
    // makes hover-only affordances disappear entirely on one.
    touch: true,
  },
  {
    id: 'panel-ultrawide-strip',
    label: 'Ultrawide strip 1920 × 515',
    group: 'panel',
    width: 1920,
    height: 515,
    deviceScaleFactor: 1,
    mobile: false,
    touch: false,
  },
]

/** Index built once. A picker re-renders on every keystroke and a linear scan
 *  per row is the kind of cost nobody notices until the list grows. */
const BY_ID = new Map(DEVICES.map((d) => [d.id, d]))

/**
 * `undefined` rather than a throw or a default: a stored `deviceId` outlives
 * the list that produced it, so a preset renamed or dropped between versions
 * must degrade to "fit the window" and not take the tab down with it. The
 * caller decides what absence means; here it is only reported.
 */
export function deviceById(id: string): DevicePreset | undefined {
  return BY_ID.get(id)
}
