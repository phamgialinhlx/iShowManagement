import { useCallback, useEffect, useState } from "react";

/**
 * A rail the operator can drag wider, remembered between runs.
 *
 * Both rails carry lists whose useful width is the *content's*, not a number
 * chosen once: a long hyphenated project name truncates at 216px while someone
 * with short hostnames wants that space back for the deck. Truncation is the
 * specific failure — the rail's whole job is telling sessions apart, and
 * `long-project-nam…` beside `long-project-name…` tells you nothing.
 *
 * ## Why this is not the CSS `resize` property
 *
 * `resize: horizontal` needs `overflow` other than `visible`, which clips the
 * rail's own popovers, and its result cannot be read back — so the width could
 * not be persisted and every launch would reset it. A pointer drag is a few
 * lines and gives both.
 *
 * ## Bounds, and why they are not advisory
 *
 * A rail dragged to nothing is unrecoverable: the grip goes with it, so there is
 * no way to drag it back and nothing on screen says the rail exists. The minimum
 * keeps a grabbable edge. The maximum stops a rail from swallowing the deck,
 * which is the same trap in the other direction.
 */

/** Below this a rail has no grabbable edge left to drag back. */
export const RAIL_MIN = 140;
export const RAIL_MAX = 640;

const clamp = (n: number) => Math.min(RAIL_MAX, Math.max(RAIL_MIN, Math.round(n)));

export function useRailWidth(storageKey: string, fallback: number) {
  const [width, setWidth] = useState(fallback);

  // Read once on mount rather than in `useState`'s initialiser: this module is
  // imported by the Settings window too, which has no rail, and touching
  // `localStorage` during render makes that a render-time side effect.
  useEffect(() => {
    const stored = Number(localStorage.getItem(storageKey));
    if (Number.isFinite(stored) && stored > 0) setWidth(clamp(stored));
  }, [storageKey]);

  /**
   * @param side which edge carries the grip. A rail on the right grows as the
   *        pointer moves *left*, so the delta is inverted — getting this wrong
   *        gives a rail that shrinks when you pull it open, which reads as the
   *        drag being broken rather than reversed.
   */
  const startResize = useCallback(
    (event: React.PointerEvent, side: "left" | "right") => {
      event.preventDefault();
      const startX = event.clientX;
      const startWidth = width;

      const move = (e: PointerEvent) => {
        const delta = side === "right" ? startX - e.clientX : e.clientX - startX;
        setWidth(clamp(startWidth + delta));
      };
      const finish = (e: PointerEvent) => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", finish);
        // Body cursor is restored here rather than in `move`, or a drag that
        // ends outside the window leaves the whole app showing a resize cursor.
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
        const delta = side === "right" ? startX - e.clientX : e.clientX - startX;
        try {
          localStorage.setItem(storageKey, String(clamp(startWidth + delta)));
        } catch {
          /* a full localStorage must not break resizing */
        }
      };

      // Held for the whole drag: without it the pointer leaving the 4px grip
      // flickers the cursor back to an arrow on every frame.
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", finish);
    },
    [storageKey, width],
  );

  return { width, startResize };
}
