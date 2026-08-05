import { useRef, useState } from "react";

import { api } from "../lib/api";

/**
 * What sits behind the app: your desktop, a colour, or a picture.
 *
 * ## Three buttons, not a dropdown
 *
 * The choice is small, closed and consequential, and the whole point of this
 * screen is that the result is visible the moment you pick. A `<select>` hides
 * two of the three options behind a click and gives no hint that anything will
 * change; three labelled buttons show the entire choice at once and make the
 * current state readable without opening anything.
 *
 * ## The controls that do not apply are not shown
 *
 * Picking DESKTOP hides the colour well, the file button and the coverage
 * slider — they cannot affect anything in that mode, and a control that visibly
 * does nothing teaches people to distrust the ones that do. The values are kept,
 * so switching back and forth is free.
 *
 * ## Nothing here can fail silently
 *
 * Choosing a file is the one operation that can genuinely go wrong — wrong
 * format, unreadable, too large — so it reports inline, next to the button that
 * started it, and the error stays until the next attempt.
 */
export function BackgroundPicker({
  mode,
  color,
  image,
  cover,
  onChange,
}: {
  mode: "desktop" | "color" | "image";
  color: string;
  image?: string;
  cover: number;
  onChange: (patch: {
    background?: "desktop" | "color" | "image";
    backgroundColor?: string;
    backgroundImage?: string;
    backgroundCover?: number;
  }) => void;
}) {
  const fileRef = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pick = async (file: File) => {
    setBusy(true);
    setError(null);
    try {
      const buffer = await file.arrayBuffer();
      // Chunked rather than `String.fromCharCode(...bytes)`: spreading a
      // multi-megabyte array into arguments overflows the call stack, which
      // shows up as a picture that works for a small icon and crashes the tab
      // for a real wallpaper.
      const bytes = new Uint8Array(buffer);
      let binary = "";
      for (let i = 0; i < bytes.length; i += 0x8000) {
        binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
      }
      const path = await api.backgroundSet(btoa(binary));
      onChange({ backgroundImage: path, background: "image" });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
      // Cleared so choosing the *same* file again still fires a change event —
      // otherwise re-picking after an error appears to do nothing.
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  return (
    <div className="flex flex-col gap-3">
      {/* "Window backdrop", not just "background" — Palette also has a Background
          (the chrome base colour), and the two meaning different things under the
          same word is what read as duplicated. This one is what sits behind the
          whole window; that one is what the panels are built from. */}
      <span className="micro">WINDOW BACKDROP</span>

      <div className="flex" style={{ border: "1px solid var(--border-strong)" }}>
        <Mode selected={mode === "desktop"} onClick={() => onChange({ background: "desktop" })}>
          DESKTOP
        </Mode>
        <Mode selected={mode === "color"} onClick={() => onChange({ background: "color" })} divider>
          COLOUR
        </Mode>
        <Mode selected={mode === "image"} onClick={() => onChange({ background: "image" })} divider>
          PICTURE
        </Mode>
      </div>

      <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        {mode === "desktop"
          ? "The window is translucent and your own wallpaper shows through."
          : mode === "color"
            ? "A flat colour fills the window instead of the desktop."
            : "A picture fills the window. It is copied into rmux, so moving or deleting the original is safe."}
      </span>

      {mode !== "desktop" && (
        <>
          <label className="flex items-center gap-3">
            <input
              type="color"
              value={color}
              onChange={(e) => onChange({ backgroundColor: e.target.value })}
              className="h-7 w-12 cursor-pointer border-0 bg-transparent p-0"
              aria-label="background colour"
            />
            <span className="micro">{mode === "image" ? "BEHIND THE PICTURE" : "COLOUR"}</span>
            <span className="data text-[11px]" style={{ color: "var(--text-soft)" }}>
              {color.toUpperCase()}
            </span>
          </label>

          {mode === "image" && (
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-3">
                <button
                  type="button"
                  className="btn"
                  disabled={busy}
                  onClick={() => fileRef.current?.click()}
                >
                  {busy ? "Copying…" : image ? "Change picture" : "Choose a picture…"}
                </button>
                {image && !busy && (
                  <button
                    type="button"
                    className="chip"
                    onClick={() => {
                      void api.backgroundClear().catch(() => {});
                      onChange({ backgroundImage: undefined, background: "color" });
                    }}
                  >
                    REMOVE
                  </button>
                )}
              </div>

              {/* The one operation that can genuinely fail, reporting where it
                  was started and persisting until the next attempt. */}
              {error && (
                <span className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
                  {error}
                </span>
              )}

              {!image && !error && (
                <span className="micro" style={{ color: "var(--text-faint)" }}>
                  PNG, JPEG, GIF, WEBP OR SVG
                </span>
              )}

              <input
                ref={fileRef}
                type="file"
                accept="image/png,image/jpeg,image/gif,image/webp,image/svg+xml"
                className="hidden"
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  if (file) void pick(file);
                }}
              />
            </div>
          )}

          <label className="flex flex-col gap-1">
            <div className="flex items-baseline justify-between">
              <span className="micro">COVERAGE</span>
              <span className="data text-[11px]" style={{ color: "var(--text)" }}>
                {cover}%
              </span>
            </div>
            <input
              type="range"
              min={0}
              max={100}
              value={cover}
              onChange={(e) => onChange({ backgroundCover: Number(e.target.value) })}
              style={{ accentColor: "rgb(var(--primary))" }}
            />
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              Below 100% the desktop shows through, so a colour becomes a wash over your wallpaper.
            </span>
          </label>
        </>
      )}
    </div>
  );
}

function Mode({
  selected,
  onClick,
  divider,
  children,
}: {
  selected: boolean;
  onClick: () => void;
  divider?: boolean;
  children: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={selected}
      className="micro flex-1 px-3 py-[7px] text-center"
      style={{
        color: selected ? "var(--text)" : "var(--text-soft)",
        background: selected ? "var(--app-elev)" : "transparent",
        borderLeft: divider ? "1px solid var(--border-strong)" : undefined,
        // The selected state is carried by an underline as well as by tone.
        // Tone alone is a 1.5:1 difference at 9px, which is not a state anyone
        // can read at a glance — and glancing is the entire job of this row.
        boxShadow: selected ? "inset 0 -2px 0 var(--text)" : undefined,
      }}
    >
      {children}
    </button>
  );
}
