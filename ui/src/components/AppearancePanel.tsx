import { useEffect, useState } from "react";

import { loadUserCss, saveUserCss } from "../lib/user-css";

/**
 * How much of the desktop shows through.
 *
 * The same two knobs the previous app exposed under Appearance › Glass, and for
 * the same reason: the right amount depends entirely on what is behind the
 * window. A bright wallpaper needs a heavier tint before 9px labels stay
 * legible; a dark one can carry far more transparency before anything is lost.
 * That is not a judgement this app can make on the operator's behalf.
 *
 * Written straight onto `:root`, because the whole design system reads these
 * two variables — every panel, menu and floating window picks the change up at
 * once with nothing to re-render.
 */

const STORAGE_KEY = "rmux.appearance";

type Appearance = {
  /** Panel opacity, 0–100. Lower shows more desktop. */
  tint: number;
  /** Frost radius in px. Below about 10 it stops reading as glass. */
  blur: number;
};

const DEFAULTS: Appearance = { tint: 64, blur: 20 };

function load(): Appearance {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? { ...DEFAULTS, ...(JSON.parse(raw) as Partial<Appearance>) } : DEFAULTS;
  } catch {
    return DEFAULTS;
  }
}

/** Push the values into the document. Exported so startup can apply them
 *  before the first paint rather than flashing the defaults. */
export function applyAppearance(a: Appearance = load()) {
  const root = document.documentElement;
  root.style.setProperty("--panel-tint", `${a.tint}%`);
  root.style.setProperty("--app-blur", `${a.blur}px`);
}

export function AppearancePanel() {
  const [appearance, setAppearance] = useState<Appearance>(load);

  useEffect(() => {
    applyAppearance(appearance);
    localStorage.setItem(STORAGE_KEY, JSON.stringify(appearance));
  }, [appearance]);

  const row = (
    label: string,
    hint: string,
    value: number,
    min: number,
    max: number,
    suffix: string,
    set: (n: number) => void,
  ) => (
    <label className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between">
        <span className="micro">{label}</span>
        <span className="data text-[11px]" style={{ color: "var(--text)" }}>
          {value}
          {suffix}
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        value={value}
        onChange={(e) => set(Number(e.target.value))}
        style={{ accentColor: "rgb(var(--primary))" }}
      />
      <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        {hint}
      </span>
    </label>
  );

  return (
    <section className="flex max-w-[520px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">APPEARANCE</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          The window is translucent over your desktop. These two decide how much of it you see —
          the right setting depends on your wallpaper, so it is yours to pick.
        </p>
      </header>

      {row(
        "GLASS",
        "Lower shows more desktop. Too low and 9px labels stop being readable over a bright wallpaper.",
        appearance.tint,
        20,
        100,
        "%",
        (tint) => setAppearance((a) => ({ ...a, tint })),
      )}

      {row(
        "FROST",
        "How far the backdrop is blurred behind each panel. Below about 10px it reads as dirty transparency rather than glass.",
        appearance.blur,
        0,
        60,
        "px",
        (blur) => setAppearance((a) => ({ ...a, blur })),
      )}

      <button
        type="button"
        className="btn self-start"
        onClick={() => setAppearance(DEFAULTS)}
      >
        Reset
      </button>

      <UserCss />

      <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
        Blur is applied per panel, never across the whole app — that is what keeps the backdrop
        sharp in the gaps between panels and makes the material read as separate sheets of glass.
      </p>
    </section>
  );
}

/**
 * Your own stylesheet, on top of everything.
 *
 * Applied live as it is typed rather than behind a Save, because CSS is the one
 * kind of setting where the result *is* the feedback — a rule you cannot see
 * take effect is a rule you cannot debug. It is stored on every keystroke too,
 * debounced by nothing: this is a text box someone visits occasionally, not a
 * hot path.
 *
 * See `lib/user-css.ts` for why this is safe in a webview that can reach IPC,
 * and why it works at all without a Chromium extension.
 */
function UserCss() {
  const [css, setCss] = useState(loadUserCss);

  return (
    <section
      className="flex flex-col gap-2"
      style={{ borderTop: "1px solid var(--border)", paddingTop: 16 }}
    >
      <h2 className="kicker">CUSTOM CSS</h2>
      <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        Applied last, so it wins. Everything in the design system is a CSS variable — start with{" "}
        <span style={{ color: "var(--text)" }}>:root &#123; --text: #fff; &#125;</span> and inspect
        the app for the rest.
      </p>
      <textarea
        value={css}
        spellCheck={false}
        rows={8}
        onChange={(e) => {
          setCss(e.target.value);
          saveUserCss(e.target.value);
        }}
        placeholder={":root { --app-blur: 30px; }"}
        className="data inset w-full resize-y px-2 py-[6px] text-[11px] leading-relaxed outline-none"
        style={{
          border: "1px solid var(--border-strong)",
          color: "var(--text)",
          background: "transparent",
          minHeight: 140,
        }}
      />
      <button
        type="button"
        className="btn self-start"
        onClick={() => {
          setCss("");
          saveUserCss("");
        }}
      >
        Clear
      </button>
    </section>
  );
}
