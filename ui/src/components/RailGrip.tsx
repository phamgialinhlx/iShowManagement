/**
 * The draggable edge of a rail.
 *
 * A hairline that widens its *hit area* without widening its appearance: the
 * visible line stays 1px so the chrome does not gain a rule it did not have,
 * while the grabbable strip is 5px, which is the difference between a control
 * you can hit and one you have to aim at.
 *
 * It is absolutely positioned over the rail's edge rather than laid out beside
 * it, so adding a grip does not change any rail's width by a pixel — the panes
 * next to it stay exactly where they were.
 */
export function RailGrip({
  side,
  onPointerDown,
}: {
  /** Which edge of the rail this sits on. */
  side: "left" | "right";
  onPointerDown: (event: React.PointerEvent) => void;
}) {
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      title="Drag to resize"
      onPointerDown={onPointerDown}
      // Double-click is not wired to "reset": a rail that jumps to a width
      // nobody chose, from a gesture aimed at the list behind it, is a worse
      // accident than a rail left slightly wrong.
      className="absolute top-0 z-10 h-full cursor-col-resize"
      style={{ [side]: -2, width: 5 } as React.CSSProperties}
    >
      <div
        className="h-full"
        style={{
          width: 1,
          marginLeft: side === "left" ? 2 : undefined,
          marginRight: side === "right" ? 2 : undefined,
          background: "transparent",
        }}
      />
    </div>
  );
}
