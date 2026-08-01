import { useEffect, useRef, useState } from "react";

const GLYPHS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/\\<>=*+-";

/**
 * Text that scrambles into place, character by character.
 *
 * Used for panel titles. It resolves left to right so the label becomes readable
 * progressively rather than all at once, which keeps it from reading as a glitch.
 *
 * Spaces are never scrambled — preserving word boundaries throughout means the
 * shape of the final string is stable, so nothing around it reflows mid-animation.
 */
export function Decode({
  text,
  className,
  /** ms per character; total run is roughly text.length * speed. */
  speed = 28,
}: {
  text: string;
  className?: string;
  speed?: number;
}) {
  const [shown, setShown] = useState(text);
  const frame = useRef(0);

  useEffect(() => {
    // Respect the OS setting rather than animating anyway at a shorter duration:
    // a scramble effect is precisely the kind of motion the preference exists for.
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setShown(text);
      return;
    }

    frame.current = 0;
    let raf = 0;
    let last = performance.now();

    const tick = (now: number) => {
      if (now - last >= speed) {
        last = now;
        frame.current += 1;

        const settled = frame.current;
        if (settled >= text.length) {
          setShown(text);
          return;
        }

        setShown(
          text
            .split("")
            .map((ch, i) => {
              if (i < settled || ch === " ") return ch;
              return GLYPHS[Math.floor(Math.random() * GLYPHS.length)];
            })
            .join(""),
        );
      }
      raf = requestAnimationFrame(tick);
    };

    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [text, speed]);

  // The accessible name must be the real text, not whatever frame we are on —
  // otherwise a screen reader announces scrambled glyphs.
  return (
    <span className={className} aria-label={text}>
      <span aria-hidden="true">{shown}</span>
    </span>
  );
}
