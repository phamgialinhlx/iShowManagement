import { useEffect, useRef, useState } from "react";

import { useWorkspace } from "../lib/workspace";

/**
 * Save the current workbench layout — the grid and what sits in each cell — under
 * a name, and switch back to it in one click. A layout is `{ deck, panes }`; the
 * store owns the snapshot and the restore, this is only the control for it.
 *
 * It sits beside the grid picker in the footer and opens **upward**, because the
 * footer is at the bottom of the window and a menu that grew downward would fall
 * off the screen.
 */
export function SavedViews() {
  const savedViews = useWorkspace((s) => s.savedViews);
  const saveCurrentView = useWorkspace((s) => s.saveCurrentView);
  const applyView = useWorkspace((s) => s.applyView);
  const deleteView = useWorkspace((s) => s.deleteView);

  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const box = useRef<HTMLDivElement>(null);

  // Outside-click and Escape both close it — the same dismissal the context menu
  // uses, so the two feel like one app.
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

  const save = () => {
    const trimmed = name.trim();
    if (!trimmed) return;
    saveCurrentView(trimmed);
    setName("");
  };

  const chipStyle = {
    border: "1px solid var(--border)",
    color: "var(--text-soft)",
  };

  return (
    <div className="relative" ref={box}>
      <button
        type="button"
        className="data px-[5px] text-[10px]"
        onClick={() => setOpen((v) => !v)}
        title="Saved views — store this layout and switch back to it"
        style={{
          ...chipStyle,
          color: open ? "var(--text)" : "var(--text-soft)",
          background: open ? "var(--hover)" : "transparent",
        }}
      >
        SAVED{savedViews.length ? ` · ${savedViews.length}` : ""}
      </button>

      {open && (
        <div
          className="menu absolute bottom-full mb-1 left-0 flex min-w-[190px] max-w-[280px] flex-col gap-[2px] p-1"
          style={{ border: "1px solid var(--border)", zIndex: 50 }}
        >
          {savedViews.length === 0 && (
            <span className="micro px-1 py-[3px]" style={{ color: "var(--text-faint)" }}>
              no saved views yet
            </span>
          )}

          {savedViews.map((v) => (
            <div key={v.id} className="flex items-center gap-1">
              <button
                type="button"
                className="data flex-1 truncate px-[6px] py-[3px] text-left text-[11px]"
                onClick={() => {
                  applyView(v.id);
                  setOpen(false);
                }}
                onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
                onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
                style={{ color: "var(--text)" }}
                title={`Switch to “${v.name}” (${v.deck.cols}×${v.deck.rows})`}
              >
                {v.name}
              </button>
              <button
                type="button"
                className="micro px-1 py-[3px]"
                onClick={() => deleteView(v.id)}
                title={`Delete “${v.name}”`}
                aria-label={`Delete ${v.name}`}
                onMouseEnter={(e) => (e.currentTarget.style.color = "var(--text)")}
                onMouseLeave={(e) => (e.currentTarget.style.color = "var(--text-faint)")}
                style={{ color: "var(--text-faint)" }}
              >
                ✕
              </button>
            </div>
          ))}

          <div style={{ borderTop: "1px solid var(--border)", margin: "3px 0" }} />

          {/* Name and save the current layout. Enter saves, Escape closes. Naming
              a view the same as an existing one updates it in place. */}
          <div className="flex items-center gap-1 px-[2px]">
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") save();
              }}
              placeholder="name this view…"
              className="data min-w-0 flex-1 px-[6px] py-[3px] text-[11px]"
              style={{
                border: "1px solid var(--border)",
                background: "transparent",
                color: "var(--text)",
              }}
            />
            <button
              type="button"
              className="data px-[6px] py-[3px] text-[10px]"
              onClick={save}
              disabled={!name.trim()}
              title="Save the current layout under this name"
              style={{
                border: "1px solid var(--border)",
                color: name.trim() ? "var(--text)" : "var(--text-faint)",
              }}
            >
              SAVE
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
