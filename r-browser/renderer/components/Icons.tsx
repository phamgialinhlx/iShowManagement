/**
 * The icon set, hand-written.
 *
 * Rule 3: no emoji, inline SVG only — Lucide-style, 1.6px strokes, square caps.
 * No icon package is installed on purpose: this app needs about fifteen glyphs,
 * and a dependency would bring several thousand plus a second opinion about
 * stroke weight. `currentColor` throughout, so an icon inherits the tone of
 * whatever control it sits in and rule 0 is decided by the button, never here.
 */
import type { SVGProps } from 'react'

type Props = { size?: number } & SVGProps<SVGSVGElement>

function Glyph({ size = 14, children, ...rest }: Props) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="square"
      strokeLinejoin="miter"
      aria-hidden="true"
      focusable="false"
      {...rest}
    >
      {children}
    </svg>
  )
}

export const IconBack = (p: Props) => (
  <Glyph {...p}><path d="M15 19 8 12l7-7" /></Glyph>
)

export const IconForward = (p: Props) => (
  <Glyph {...p}><path d="m9 5 7 7-7 7" /></Glyph>
)

export const IconReload = (p: Props) => (
  <Glyph {...p}>
    <path d="M20 11a8 8 0 1 0-.6 4" />
    <path d="M20 5v6h-6" />
  </Glyph>
)

export const IconStop = (p: Props) => (
  <Glyph {...p}><path d="M6 6 18 18" /><path d="M18 6 6 18" /></Glyph>
)

export const IconClose = IconStop

export const IconPlus = (p: Props) => (
  <Glyph {...p}><path d="M12 5v14" /><path d="M5 12h14" /></Glyph>
)

/** PICK. A crosshair, because the gesture is "point at that one". */
export const IconPick = (p: Props) => (
  <Glyph {...p}>
    <path d="M12 3v4" /><path d="M12 17v4" />
    <path d="M3 12h4" /><path d="M17 12h4" />
    <path d="M9 9h6v6H9z" />
  </Glyph>
)

export const IconCamera = (p: Props) => (
  <Glyph {...p}>
    <path d="M3 8h4l2-3h6l2 3h4v11H3z" />
    <path d="M9 13a3 3 0 1 0 6 0 3 3 0 1 0-6 0" />
  </Glyph>
)

/** A picture, distinct from the camera: the camera *takes* a shot, this one
 *  copies the shot that already exists. Two actions, two glyphs. */
export const IconImage = (p: Props) => (
  <Glyph {...p}>
    <path d="M3 5h18v14H3z" />
    <path d="M3 16l5-5 4 4 3-3 6 6" />
    <path d="M8.5 9.5a1 1 0 1 0 0-.01" />
  </Glyph>
)

export const IconCode = (p: Props) => (
  <Glyph {...p}><path d="m9 8-5 4 5 4" /><path d="m15 8 5 4-5 4" /></Glyph>
)

export const IconCopy = (p: Props) => (
  <Glyph {...p}><path d="M9 9h11v11H9z" /><path d="M5 15H4V4h11v1" /></Glyph>
)

export const IconChevron = (p: Props) => (
  <Glyph {...p}><path d="m6 9 6 6 6-6" /></Glyph>
)

export const IconRotate = (p: Props) => (
  <Glyph {...p}>
    <path d="M4 14h10V4H4z" />
    <path d="M14 10h6v10h-10v-6" />
  </Glyph>
)

export const IconTrash = (p: Props) => (
  <Glyph {...p}>
    <path d="M4 7h16" /><path d="M9 7V4h6v3" /><path d="M6 7v13h12V7" />
  </Glyph>
)

export const IconEdit = (p: Props) => (
  <Glyph {...p}><path d="M4 20h4L20 8l-4-4L4 16z" /></Glyph>
)

/** RE-MEASURE. A ruler: the question is "where is that element *now*". */
export const IconRuler = (p: Props) => (
  <Glyph {...p}>
    <path d="M3 15 15 3l6 6L9 21z" />
    <path d="M7 11l2 2" /><path d="M11 7l2 2" /><path d="M15 11l-2 2" />
  </Glyph>
)

export const IconSend = (p: Props) => (
  <Glyph {...p}><path d="M4 12h15" /><path d="m13 6 6 6-6 6" /></Glyph>
)

/** The inspector toggle: a pane divided on its right. */
export const IconPanel = (p: Props) => (
  <Glyph {...p}><path d="M3 4h18v16H3z" /><path d="M15 4v16" /></Glyph>
)

const STAR = 'M12 3.5 14.6 9l5.9.6-4.4 4 1.3 5.8L12 16.9 6.6 19.4 7.9 13.6 3.5 9.6 9.4 9z'

export const IconStar = (p: Props) => (
  <Glyph {...p}><path d={STAR} /></Glyph>
)

/** The pinned state: same silhouette, filled, so bookmarked reads at a glance. */
export const IconStarFilled = (p: Props) => (
  <Glyph {...p} fill="currentColor"><path d={STAR} /></Glyph>
)

export const IconClock = (p: Props) => (
  <Glyph {...p}><circle cx="12" cy="12" r="8" /><path d="M12 8v4l3 2" /></Glyph>
)
