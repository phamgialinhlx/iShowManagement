import { useEffect, useLayoutEffect, useRef, useState } from "react";

/**
 * The one context menu in the app.
 *
 * This is `TreeMenu`'s shell, lifted out so the file tree and the session rail
 * are the same menu rather than two that look alike. The alternative was tried
 * on a feature branch — a near-verbatim copy under a second name — and the two
 * had already drifted before it was reviewed: a different `min-w`, a different
 * re-measure strategy, `flex items-center` against `flex items-baseline`. A
 * menu is the wrong place to discover that two components disagree, because the
 * disagreement is only visible when both are open, which is never.
 *
 * Everything load-bearing lives here so a caller cannot omit it by accident.
 */

/** Where the pointer was. The menu grows from this point. */
export type MenuAt = { x: number; y: number };

/**
 * A positioned, dismissable menu surface.
 *
 * ## Flip before clamping
 *
 * A right-click near the bottom of a long list — which is most of them — used to
 * put half the items below the edge of the app, unreachable, with nothing
 * indicating anything was missing. Delete was simply gone.
 *
 * A menu that grows **upward** from the cursor keeps the pointer on its edge,
 * where the hand already is. Clamping instead slides the list up under the
 * cursor so it lands on whichever item happens to be there — which is how
 * someone deletes a file they meant to rename. Clamping is the fallback for a
 * menu too tall to fit either way, not the first choice.
 *
 * ## The scale is measured, never assumed
 *
 * `getBoundingClientRect` reports viewport pixels while the `top`/`left` written
 * here are in the page's own coordinate space. Any transform between them — a
 * `zoom`, a `scale` — makes the two disagree, and the error grows the further
 * down the window you click. Dividing the rect by the element's own
 * `offsetHeight` recovers the factor without this component needing to know that
 * such a transform exists or where it is applied.
 */
export function MenuSurface({
  at,
  onClose,
  children,
  minWidth = 190,
}: {
  at: MenuAt;
  onClose: () => void;
  children: React.ReactNode;
  minWidth?: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [placed, setPlaced] = useState(at);
  const [measured, setMeasured] = useState(false);

  // Dismiss on an outside click or Escape, like every context menu.
  //
  // `mousedown` rather than `click`: a menu that survives until mouseup can be
  // dismissed by a drag that started inside it, and the item under the release
  // point would fire.
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest("[data-menu]")) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;

    const place = () => {
      const rect = el.getBoundingClientRect();
      // Falls back to 1 for a zero-height element, which would otherwise divide
      // by zero and place the menu at NaN — that is, nowhere.
      const scale = el.offsetHeight > 0 ? rect.height / el.offsetHeight : 1;
      const margin = 8;

      // **Measured from the element's own box, never from where it currently
      // sits.** `offsetWidth`/`offsetHeight` are the layout size and do not move
      // with `left`/`top`, which is what makes running this twice give the same
      // answer.
      //
      // Deriving from `getBoundingClientRect().bottom` instead — the obvious
      // reading, and what this replaced — is *not* idempotent: the first run
      // flips the menu up, and the second sees a box that no longer overflows
      // and puts it straight back down. With a one-shot effect that never
      // showed; the moment placement could run again it un-flipped the menu on
      // the next observer callback. Caught by `menu-check.ts`, which measured
      // it growing to `bottom 1202` in a 987px window.
      const width = el.offsetWidth;
      const height = el.offsetHeight;

      let x = at.x;
      let y = at.y;

      if (at.y + height * scale > window.innerHeight - margin) {
        // Flip if the menu fits above the cursor; otherwise clamp, because
        // unreachable is worse than displaced.
        y =
          height * scale <= at.y - margin
            ? at.y - height
            : Math.max(margin, (window.innerHeight - margin - height * scale) / scale);
      }

      if (at.x + width * scale > window.innerWidth - margin) {
        x = Math.max(margin, at.x - width);
      }

      setPlaced({ x, y });
      setMeasured(true);
    };

    place();

    // **Re-measure whenever the menu's own height changes**, rather than asking
    // callers to list what makes it change. Opening a rename field or a delete
    // confirmation makes the menu a different size, and a position computed for
    // the previous one is wrong for this one — the menu drifts off the bottom
    // exactly when it has grown a destructive button.
    //
    // The observer is the point: the previous version took a dependency array,
    // so every new inline state a caller added was a placement bug waiting for
    // someone to right-click near the bottom of the screen. Placement does not
    // change the element's size, so this cannot feed itself.
    const observer = new ResizeObserver(place);
    observer.observe(el);
    return () => observer.disconnect();
  }, [at.x, at.y]);

  return (
    <div
      ref={ref}
      data-menu
      className="menu fixed z-[80] p-1"
      style={{
        left: placed.x,
        top: placed.y,
        minWidth,
        // Hidden for the one frame between "rendered at the cursor" and
        // "measured and moved". Without it the menu is visibly drawn off the
        // bottom of the window and then jumps, which reads as a glitch even
        // though the end state is correct.
        visibility: measured ? "visible" : "hidden",
      }}
    >
      {children}
    </div>
  );
}

/**
 * One row of a menu.
 *
 * `destructive` is the only colour this carries, and it is the design system's
 * "you must act" red — reserved for the item that ends something. A menu where
 * three things are red is a menu where nothing is.
 */
export function MenuItem({
  label,
  onClick,
  destructive,
  disabled,
  hint,
}: {
  label: string;
  onClick: () => void;
  destructive?: boolean;
  disabled?: boolean;
  /** A short right-aligned note — a shortcut, or where the action will land. */
  hint?: string;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="data flex w-full items-baseline gap-2 px-3 py-[5px] text-left text-[11px]"
      style={{
        color: disabled
          ? "var(--text-faint)"
          : destructive
            ? "rgb(var(--primary))"
            : "var(--text)",
      }}
      onMouseEnter={(e) => {
        if (!disabled) e.currentTarget.style.background = "var(--hover)";
      }}
      onMouseLeave={(e) => {
        e.currentTarget.style.background = "transparent";
      }}
    >
      <span className="truncate">{label}</span>
      {hint && (
        <span className="micro ml-auto shrink-0 truncate" style={{ maxWidth: "9rem" }}>
          {hint}
        </span>
      )}
    </button>
  );
}

/** The divider between groups of items. */
export function MenuDivider() {
  return <hr className="hairline my-1" />;
}
