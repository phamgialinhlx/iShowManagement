import { useEffect, useRef, useState } from "react";

import { MAX_DECK_SIDE, clampDeck, deckCells, deckLabel, isPresetDeck, type Deck } from "../lib/grid";
import { useWorkspace } from "../lib/workspace";

/**
 * Any grid, chosen by the operator.
 *
 * The presets are what most screens want; this is for the ones they do not — a
 * 49" ultrawide, a portrait monitor, a laptop that wants exactly two rows of
 * three. Two ways to choose, because they suit different hands: **sweep the
 * matrix** (the table-size picker every editor has, so it needs no explaining)
 * or **type the numbers** when you know them.
 *
 * The chip names the active custom deck (`2×3`) rather than staying "CUSTOM",
 * so the current layout is readable without opening anything — and it is
 * underlined like a selected preset, because at 10px tone alone is not a state.
 */
export function DeckPicker() {
  const deck = useWorkspace((s) => s.deck);
  const setDeck = useWorkspace((s) => s.setDeck);

  const [open, setOpen] = useState(false);
  const [hover, setHover] = useState<Deck | null>(null);
  const [rows, setRows] = useState(String(deck.rows));
  const [cols, setCols] = useState(String(deck.cols));
  const box = useRef<HTMLDivElement>(null);

  const custom = !isPresetDeck(deck);

  // Re-seed the fields from the live deck each time the picker opens, so they
  // describe what is on screen rather than whatever was typed last time.
  useEffect(() => {
    if (!open) return;
    setRows(String(deck.rows));
    setCols(String(deck.cols));
    setHover(null);
  }, [open, deck.rows, deck.cols]);

  // Same dismissal as SAVED and the context menu: outside click or Escape.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const apply = (next: Deck) => {
    setDeck(clampDeck(next));
    setOpen(false);
  };

  const typed = clampDeck({ rows: Number(rows), cols: Number(cols) });
  const typedValid = /^\d+$/.test(rows) && /^\d+$/.test(cols);
  // What the readout describes: the cell under the pointer, else the fields.
  const shown = hover ?? typed;

  const cell = 14;
  const side = Array.from({ length: MAX_DECK_SIDE }, (_, i) => i + 1);

  return (
    <div className="relative" ref={box}>
      <button
        type="button"
        className="data px-[5px] text-[10px]"
        onClick={() => setOpen((v) => !v)}
        title={custom ? `Custom grid: ${deck.rows} rows × ${deck.cols} columns — click to change` : "Choose any grid size"}
        aria-label="Custom grid size"
        style={{
          border: "1px solid var(--border)",
          color: custom || open ? "var(--text)" : "var(--text-soft)",
          background: custom || open ? "var(--hover)" : "transparent",
          textDecoration: custom ? "underline" : "none",
        }}
      >
        {custom ? deckLabel(deck) : "CUSTOM"}
      </button>

      {open && (
        <div
          className="menu absolute bottom-full mb-1 right-0 flex flex-col gap-2 p-2"
          style={{ border: "1px solid var(--border)", zIndex: 50 }}
        >
          <span className="micro whitespace-nowrap">GRID · ROWS × COLS</span>

          {/* The matrix: hovering sizes a block from the top-left, clicking applies. */}
          <div
            role="grid"
            aria-label="Pick a grid size"
            className="grid"
            style={{ gridTemplateColumns: `repeat(${MAX_DECK_SIDE}, ${cell}px)`, gap: 2 }}
            onMouseLeave={() => setHover(null)}
          >
            {side.map((r) =>
              side.map((c) => {
                const lit = r <= shown.rows && c <= shown.cols;
                return (
                  <button
                    key={`${r}x${c}`}
                    type="button"
                    aria-label={`${r} rows by ${c} columns`}
                    onMouseEnter={() => setHover({ rows: r, cols: c })}
                    onFocus={() => setHover({ rows: r, cols: c })}
                    onClick={() => apply({ rows: r, cols: c })}
                    style={{
                      width: cell,
                      height: cell,
                      border: "1px solid var(--border)",
                      background: lit ? "color-mix(in srgb, var(--text) 35%, transparent)" : "transparent",
                    }}
                  />
                );
              }),
            )}
          </div>

          <span className="data text-[11px]" style={{ color: "var(--text)" }}>
            {shown.rows} × {shown.cols}
            <span style={{ color: "var(--text-faint)" }}>
              {" "}· {deckCells(shown)} pane{deckCells(shown) === 1 ? "" : "s"}
            </span>
          </span>

          {/* Exact numbers, for the keyboard and for anyone who knows what they want. */}
          <div className="flex items-center gap-1">
            {[
              { label: "rows", value: rows, set: setRows },
              { label: "cols", value: cols, set: setCols },
            ].map((f, i) => [
              i === 1 && (
                <span key="by" className="data text-[11px]" style={{ color: "var(--text-faint)" }}>
                  ×
                </span>
              ),
              <input
                key={f.label}
                value={f.value}
                inputMode="numeric"
                aria-label={f.label}
                title={`${f.label}, 1–${MAX_DECK_SIDE}`}
                onChange={(e) => {
                  f.set(e.target.value.replace(/\D/g, "").slice(0, 2));
                  setHover(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && typedValid) apply(typed);
                }}
                className="data w-[38px] px-[6px] py-[3px] text-center text-[11px]"
                style={{ border: "1px solid var(--border)", background: "transparent", color: "var(--text)" }}
              />,
            ])}
            <button
              type="button"
              className="data flex-1 px-[6px] py-[3px] text-[10px]"
              disabled={!typedValid}
              onClick={() => apply(typed)}
              title={`Apply ${typed.rows} × ${typed.cols}`}
              style={{
                border: "1px solid var(--border)",
                color: typedValid ? "var(--text)" : "var(--text-faint)",
              }}
            >
              APPLY
            </button>
          </div>
          <span className="micro" style={{ color: "var(--text-faint)" }}>
            1–{MAX_DECK_SIDE} EACH WAY
          </span>
        </div>
      )}
    </div>
  );
}
