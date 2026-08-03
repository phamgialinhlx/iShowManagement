import { useEffect, useState } from "react";

import { convertFileSrc } from "@tauri-apps/api/core";


import { api, isTauri } from "../lib/api";
import { BackgroundPicker } from "./BackgroundPicker";
import { loadUserCss, saveUserCss } from "../lib/user-css";
import { gpuRendering, setGpuRendering } from "../lib/terminal-theme";

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
  /**
   * Apple's native Liquid Glass, where the machine has it (macOS 26+).
   *
   * Off by default even on a machine that supports it. It is a visibly different
   * window, and an app that silently changes its own appearance on an OS upgrade
   * is unsettling in a way that is hard to name and easy to avoid.
   */
  glass: boolean;
  /** Apple's thinner style: more wallpaper, less contrast under small text. */
  glassClear: boolean;
  /**
   * How much colour is laid *over* native glass, 0–50.
   *
   * A separate knob from `tint`, deliberately. Under real glass the panel
   * background stops being a glass simulation and becomes pure legibility
   * residue — a different quantity with a different right answer. Reusing
   * `tint` would let a value chosen for the flat material silently decide how
   * much of the translucent one you get to see.
   */
  overlay: number;

  /**
   * What sits behind the app.
   *
   * `desktop` is the shipped behaviour — a translucent window over your own
   * wallpaper. The other two paint the window instead, which is what someone
   * wants when the desktop behind it is busy, or simply theirs to choose.
   *
   * They are mutually exclusive with native glass by physics, not by policy:
   * glass refracts what is behind the *window*, and a background painted inside
   * the page covers it. So a non-desktop background switches glass off while it
   * is in effect, without forgetting the preference.
   */
  background: "desktop" | "color" | "image";
  backgroundColor: string;
  /** Absolute path on disk, written by `background_set`. */
  backgroundImage?: string;
  /** How completely the background covers the desktop, 0–100. */
  backgroundCover: number;

  /**
   * Interface scale, 60–200.
   *
   * Implemented as `zoom`, and that is the honest choice rather than a
   * font-size: the type in this app is 157 hard-coded pixel sizes, so a font
   * variable would move almost nothing and read as a broken control. `zoom`
   * scales layout with the text, which is what "make it bigger" actually means
   * — and it reaches the terminals, which re-fit to the new cell size.
   */
  scale: number;
  /** Base text colour. `--text-soft` and `--text-faint` are derived from it. */
  textColor?: string;
  /** The one accent. Reserved for "you must act" — rule 0 survives recolouring. */
  accent?: string;
};

const DEFAULT_TEXT = "#e8e6e1";
const DEFAULT_ACCENT = "#e63b2e";

const DEFAULTS: Appearance = {
  tint: 38,
  glass: false,
  glassClear: false,
  overlay: 14,
  background: "desktop",
  backgroundColor: "#0b0b0d",
  backgroundCover: 100,
  scale: 100,
};

function load(): Appearance {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? { ...DEFAULTS, ...(JSON.parse(raw) as Partial<Appearance>) } : DEFAULTS;
  } catch {
    return DEFAULTS;
  }
}

/**
 * Is this document the Settings window?
 *
 * Read from the URL for the same reason `main.tsx` does: it must be known
 * synchronously, before the first paint, or the sheet flashes at the wrong
 * scale.
 */
const isSettingsWindow =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("window") === "settings";

/** The scale as a zoom factor, clamped to what the slider offers. */
function zoom(scale: number): number {
  return Math.min(Math.max(scale, 60), 200) / 100;
}

/**
 * `#rrggbb` to the `r g b` triplet `--primary` is built from.
 *
 * Returns null rather than a guess for anything unparseable: a malformed value
 * must leave the shipped accent standing, not produce an invisible one.
 */
function toTriplet(hex: string): string | null {
  const clean = hex.trim().replace(/^#/, "");
  if (!/^[0-9a-fA-F]{6}$/.test(clean)) return null;
  const n = Number.parseInt(clean, 16);
  return `${(n >> 16) & 255} ${(n >> 8) & 255} ${n & 255}`;
}

/**
 * A filesystem path as the webview can load it.
 *
 * The picture lives in the app data directory and is served over Tauri's asset
 * protocol — the webview cannot read `file://` at all under this CSP, which is
 * exactly the point.
 */
function assetUrl(path: string): string {
  return convertFileSrc(path);
}

/**
 * The glass options last handed to Rust, so an unchanged one is not re-applied.
 * Module scope rather than a ref: `applyAppearance` is also called from startup,
 * outside any component.
 */
let lastGlass: string | null = null;

/**
 * Re-apply when *another window* changes the settings.
 *
 * Settings is its own window (`settings_window.rs`), with its own document — so
 * writing these values only ever styled the window the operator was editing in,
 * and the workbench picked them up on the next launch because it re-reads
 * `localStorage` at startup. That is why a background colour appeared to need a
 * restart: it had already been saved, and nothing had told the other window to
 * look.
 *
 * The `storage` event is exactly this signal — it fires in every *other*
 * document of the same origin — so no IPC, no Rust, and no restart button for
 * something that should never have needed one.
 */
if (typeof window !== "undefined") {
  window.addEventListener("storage", (event) => {
    if (event.key === STORAGE_KEY) applyAppearance(load());
  });
}

/** Push the values into the document. Exported so startup can apply them
 *  before the first paint rather than flashing the defaults. */
export function applyAppearance(a: Appearance = load()) {
  const root = document.documentElement;
  // Clamped, because a value stored by an older build was calibrated against a
  // CSS blur that no longer exists — see `.panel` in signal-room.css. 64% used
  // to sit on top of an opaque blur layer and looked like glass; on its own it
  // is very nearly solid, so an old setting would leave the operator with the
  // exact bug they just watched get fixed.
  root.style.setProperty("--panel-tint", `${Math.min(a.tint, 60)}%`);
  root.style.setProperty("--glass-overlay", `${Math.min(Math.max(a.overlay, 0), 50)}%`);

  // --- what sits behind the app -------------------------------------------
  const cover = Math.min(Math.max(a.backgroundCover, 0), 100) / 100;
  if (a.background === "color") {
    root.style.setProperty("--backdrop-color", a.backgroundColor);
    root.style.setProperty("--backdrop-image", "none");
    root.style.setProperty("--backdrop-opacity", String(cover));
  } else if (a.background === "image" && a.backgroundImage) {
    root.style.setProperty("--backdrop-color", a.backgroundColor);
    // `url("…")` with the path quoted: a wallpaper is routinely called
    // something with spaces and brackets in it, and an unquoted url() token
    // simply fails to parse — leaving no background and no error.
    root.style.setProperty("--backdrop-image", `url("${assetUrl(a.backgroundImage)}")`);
    root.style.setProperty("--backdrop-opacity", String(cover));
  } else {
    root.style.removeProperty("--backdrop-color");
    root.style.removeProperty("--backdrop-image");
    root.style.removeProperty("--backdrop-opacity");
  }

  // --- type and colour -----------------------------------------------------
  //
  // Published as a variable and applied to `#root` in CSS, *not* set as
  // `:root.style.zoom`. Zooming the document element scales the root box while
  // its `height: 100%` still resolves against the unscaled viewport, so the app
  // ended up sized to a fraction of its own window — panels clipped at the
  // bottom edge and a scrollbar in the middle of the settings sheet at any
  // scale but 100%. `#root` is compensated with `calc(100% / zoom)`, whose
  // percentage resolves against an *unzoomed* parent, so the product is exactly
  // the window. See `signal-room.css`.
  //
  // **The settings window is never scaled.** It carries the control, and a
  // control that resizes itself as you drag it slides out from under the cursor
  // — the slider moves, the sheet outgrows its own window, and the setting
  // becomes hard to put back. The workbench is visible while Settings is open,
  // so the effect is watched there, on the thing being configured, rather than
  // on the instrument doing the configuring.
  root.style.setProperty("--ui-zoom", isSettingsWindow ? "1" : String(zoom(a.scale)));

  if (a.textColor && a.textColor.toLowerCase() !== DEFAULT_TEXT) {
    root.style.setProperty("--text", a.textColor);
    // The flag is what lets the derived ramp exist only when it is wanted; see
    // `[data-custom-text]` in signal-room.css.
    root.dataset.customText = "on";
  } else {
    root.style.removeProperty("--text");
    delete root.dataset.customText;
  }

  const rgb = a.accent && a.accent.toLowerCase() !== DEFAULT_ACCENT ? toTriplet(a.accent) : null;
  if (rgb) {
    // `--primary` is a bare `r g b` triplet, not a colour: every use is
    // `rgb(var(--primary) / <alpha>)`. Writing a hex here would break every
    // translucent accent in the app at once.
    root.style.setProperty("--primary", rgb);
  } else {
    root.style.removeProperty("--primary");
  }

  // Native glass is a *window* change, so it is applied through Rust rather than
  // CSS — see `src-tauri/src/glass.rs` for why the page cannot do this itself.
  if (!isTauri()) return;

  // A painted background covers the desktop, so glass would be refracting
  // something nobody can see. Turned off while that is true — the stored
  // preference is untouched, so returning to the desktop background brings it
  // straight back.
  const wantGlass = a.glass && a.background === "desktop";

  // Only when the glass itself changed. The overlay slider runs this function on
  // every tick, and each call crosses IPC and mutates AppKit views on the main
  // thread for every open window — so without this guard dragging one slider
  // would stutter the whole app for an effect that is pure CSS.
  const signature = `${wantGlass}:${a.glassClear}`;
  if (signature === lastGlass) return;
  lastGlass = signature;

  // Fire-and-forget: this runs before the first paint, and a machine without
  // glass answers `available: false` rather than failing.
  void api
    .setGlass({ enabled: wantGlass, clear: a.glassClear })
    .then((status) => {
      // The scrim and the panel tint are calibrated against the *material* the
      // window has. Real glass already carries its own dimming and refraction,
      // so leaving the CSS backdrop at full strength on top of it would hide
      // exactly the effect that was just turned on.
      root.dataset.nativeGlass = status.active ? "on" : "off";
    })
    .catch(() => {
      root.dataset.nativeGlass = "off";
    });
}

export function AppearancePanel() {
  // **Two states, deliberately.** `saved` is what the app is wearing; `draft` is
  // what the operator is composing. Nothing crosses from one to the other
  // without Apply.
  //
  // Live-applying every keystroke was the original design and it reads as the
  // app changing under you — worst of all on a slider, where each tick of the
  // interface scale re-laid the whole window out mid-drag. Editing a copy makes
  // the change deliberate, makes "I have not applied this yet" visible, and
  // makes Discard possible at all.
  const [saved, setSaved] = useState<Appearance>(load);
  const [draft, setDraft] = useState<Appearance>(saved);
  const [glassAvailable, setGlassAvailable] = useState(false);
  const [restarting, setRestarting] = useState(false);

  const dirty = JSON.stringify(draft) !== JSON.stringify(saved);

  const commit = () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(draft));
    applyAppearance(draft);
    setSaved(draft);
    // Other windows learn about this through the `storage` event, which does
    // not fire in the document that wrote it — so this window applies it
    // directly and every other one is told.
  };

  useEffect(() => {
    if (!isTauri()) return;
    api
      .glassStatus()
      .then((s) => setGlassAvailable(s.available))
      .catch(() => setGlassAvailable(false));
  }, []);

  // Native glass replaces the CSS material rather than sitting beside it, so
  // exactly one of these two knobs is meaningful at a time. Showing both would
  // leave a slider that visibly does nothing — the mistake the Frost setting
  // already made once.
  const native = glassAvailable && draft.glass;

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
          The window is translucent over your desktop. How much of it you see is yours to pick —
          the right amount depends entirely on your wallpaper.
        </p>
      </header>

      <BackgroundPicker
        mode={draft.background}
        color={draft.backgroundColor}
        image={draft.backgroundImage}
        cover={draft.backgroundCover}
        onChange={(patch) => setDraft((a) => ({ ...a, ...patch }))}
      />

      {/* Hidden rather than disabled while a background is painted: glass
          refracts what is behind the *window*, so a painted background covers
          it outright. Offering a switch that cannot show its effect is the
          thing this settings screen is trying hardest not to do. */}
      {draft.background === "desktop" && (
        <LiquidGlass
          available={glassAvailable}
          value={draft}
          onChange={(patch) => setDraft((a) => ({ ...a, ...patch }))}
        />
      )}

      {native
        ? row(
            "OVERLAY",
            "Colour laid over the glass, for contrast under small text. Nothing else here simulates glass any more — macOS is drawing the real thing.",
            draft.overlay,
            0,
            50,
            "%",
            (overlay) => setDraft((a) => ({ ...a, overlay })),
          )
        : row(
            "GLASS",
            "Lower shows more desktop. Too low and 9px labels stop being readable over a bright wallpaper.",
            draft.tint,
            20,
            100,
            "%",
            (tint) => setDraft((a) => ({ ...a, tint })),
          )}

      {!native && (
        <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          There is no frost setting, and that is deliberate. The blur behind this window is macOS's
          own <span style={{ color: "var(--text)" }}>underWindowBackground</span> material, applied
          by the compositor before rmux draws anything. A CSS blur on top of it filtered the page
          rather than the desktop — which is what made the whole app look solid.
        </p>
      )}

      {row(
        "INTERFACE SCALE",
        "Everything gets bigger together — labels, panels and terminal text. Terminals re-fit to the new size.",
        draft.scale,
        60,
        200,
        "%",
        (scale) => setDraft((a) => ({ ...a, scale })),
      )}

      <div className="flex flex-col gap-3">
        <span className="micro">COLOUR</span>

        <Swatch
          label="TEXT"
          hint="The dimmer two levels are derived from this, so headings, prose and micro-labels keep their order."
          value={draft.textColor ?? DEFAULT_TEXT}
          fallback={DEFAULT_TEXT}
          onChange={(textColor) => setDraft((a) => ({ ...a, textColor }))}
        />

        <Swatch
          label="ACCENT"
          hint="Used only where you must act — a waiting prompt, an error, the caret. Everything else stays monochrome."
          value={draft.accent ?? DEFAULT_ACCENT}
          fallback={DEFAULT_ACCENT}
          onChange={(accent) => setDraft((a) => ({ ...a, accent }))}
        />
      </div>

      <button
        type="button"
        className="btn self-start"
        onClick={() => {
          // The stored picture goes too. Resetting the settings while leaving a
          // wallpaper on disk is a file nobody will ever find again.
          if (draft.backgroundImage) void api.backgroundClear().catch(() => {});
          setDraft(DEFAULTS);
        }}
      >
        Reset everything
      </button>

      <ApplyBar
        dirty={dirty}
        restarting={restarting}
        onApply={commit}
        onDiscard={() => setDraft(saved)}
        onApplyAndRestart={() => {
          commit();
          setRestarting(true);
          // No catch that clears the flag: on success this process is replaced,
          // so there is nothing left to clear. A failure leaves the label
          // showing, which is the honest state — the restart did not happen.
          void api.restartApp().catch(() => setRestarting(false));
        }}
      />

      {/*
        Everything below the Apply bar takes effect as it is typed or ticked,
        and that is on purpose rather than an oversight — a stylesheet you
        cannot see take effect is a stylesheet you cannot debug. An invisible
        boundary between "staged" and "live" would be the worst of both, so it
        is labelled where it happens.
      */}
      <div className="flex flex-col gap-1 pt-2">
        <span className="micro">APPLIED AS YOU TYPE</span>
        <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          These two take effect immediately — no Apply. Their whole value is seeing the result
          while you change them.
        </span>
      </div>

      <TerminalRendering />

      <UserCss />

    </section>
  );
}


/**
 * Apply, discard, or apply and start clean.
 *
 * **Always present, never appearing.** A bar that materialises only once
 * something is dirty moves the page under the operator at the exact moment they
 * are reading it, and teaches them nothing about how this screen works until
 * they have already changed something. It is here from the first render; only
 * its *state* changes.
 *
 * The buttons are disabled rather than hidden when there is nothing to apply,
 * and the line above them says which of the two situations they are in — so
 * "why is this greyed out" is answered before it is asked.
 *
 * **Restart is offered, not required.** Everything here takes effect
 * immediately, across windows. A relaunch buys one thing: the terminals
 * re-measure from scratch, which settles xterm cleanly after an interface-scale
 * change. Sessions survive it — they run under the agent on the target — and the
 * copy says so, because "will this kill my work?" is the only question that
 * matters before pressing it.
 */
function ApplyBar({
  dirty,
  restarting,
  onApply,
  onDiscard,
  onApplyAndRestart,
}: {
  dirty: boolean;
  restarting: boolean;
  onApply: () => void;
  onDiscard: () => void;
  onApplyAndRestart: () => void;
}) {
  return (
    <div
      className="sticky bottom-0 -mx-6 mt-1 flex flex-col gap-2 px-6 py-3"
      style={{
        borderTop: "1px solid var(--border-strong)",
        background: "color-mix(in srgb, var(--app-panel) 88%, transparent)",
      }}
    >
      <div className="flex items-center gap-3">
        <button type="button" className="btn" disabled={!dirty || restarting} onClick={onApply}>
          Apply
        </button>
        <button
          type="button"
          className="btn"
          disabled={restarting}
          onClick={onApplyAndRestart}
          title="Applies your changes, then relaunches so the terminals re-measure cleanly."
        >
          {restarting ? "Restarting…" : "Apply & restart"}
        </button>
        {dirty && !restarting && (
          <button type="button" className="micro ml-auto" onClick={onDiscard}>
            DISCARD
          </button>
        )}
      </div>

      <span className="data text-[10px] leading-relaxed" style={{ color: dirty ? "rgb(var(--busy))" : "var(--text-soft)" }}>
        {restarting
          ? "Relaunching. Your sessions keep running on their hosts and will reattach."
          : dirty
            ? "Not applied yet — the app still looks the way it did."
            : "Everything here is applied. Restart is only needed if the terminals look off after a scale change; your sessions survive it."}
      </span>
    </div>
  );
}

/**
 * A colour, with the way back.
 *
 * The reset is always present rather than appearing once the value differs from
 * the default. A control that materialises only after you have already made a
 * mess is a control you do not know exists at the moment you need it — and "how
 * do I undo this" is the first question anyone asks of a colour picker.
 */
function Swatch({
  label,
  hint,
  value,
  fallback,
  onChange,
}: {
  label: string;
  hint: string;
  value: string;
  fallback: string;
  onChange: (value: string) => void;
}) {
  const changed = value.toLowerCase() !== fallback.toLowerCase();
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-3">
        <input
          type="color"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="h-7 w-12 cursor-pointer border-0 bg-transparent p-0"
          aria-label={label.toLowerCase()}
        />
        <span className="micro">{label}</span>
        <span className="data text-[11px]" style={{ color: "var(--text-soft)" }}>
          {value.toUpperCase()}
        </span>
        <button
          type="button"
          className="micro ml-auto"
          onClick={() => onChange(fallback)}
          style={{ color: changed ? "var(--text)" : "var(--text-faint)" }}
        >
          {changed ? "RESET" : "DEFAULT"}
        </button>
      </div>
      <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        {hint}
      </span>
    </div>
  );
}

/**
 * Apple's real Liquid Glass, offered only where it exists.
 *
 * The control is hidden — not disabled — on a machine without
 * `NSGlassEffectView`. A greyed-out switch invites the question "how do I turn
 * this on", and the honest answer is "upgrade macOS", which is not a setting.
 *
 * There is one thing worth being upfront about, and the copy says it: this is a
 * single sheet behind the whole window, not per-panel glass. Glass is a native
 * view and every rmux panel is HTML inside one webview, so sixteen individually
 * refracting panels would mean sixteen native views chasing DOM geometry — wrong
 * for a frame on every resize, for an effect nobody asked to be per-panel.
 */
function LiquidGlass({
  available,
  value,
  onChange,
}: {
  available: boolean;
  value: { glass: boolean; glassClear: boolean };
  onChange: (patch: Partial<{ glass: boolean; glassClear: boolean }>) => void;
}) {
  if (!available) return null;

  return (
    <div className="flex flex-col gap-2">
      <label className="flex items-baseline gap-2">
        <input
          type="checkbox"
          checked={value.glass}
          onChange={(e) => onChange({ glass: e.target.checked })}
          style={{ accentColor: "rgb(var(--primary))" }}
        />
        <span className="micro">LIQUID GLASS</span>
      </label>

      <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        macOS 26&apos;s own glass material, drawn by the compositor — so it refracts your wallpaper
        rather than blurring a copy of it. One sheet behind the window, not one per panel: glass is
        a native view and the panels are HTML. With this on, nothing in the page simulates glass any
        more — the only knob left is how much colour is laid over it.
      </p>

      {value.glass && (
        <label className="flex items-baseline gap-2">
          <input
            type="checkbox"
            checked={value.glassClear}
            onChange={(e) => onChange({ glassClear: e.target.checked })}
            style={{ accentColor: "rgb(var(--primary))" }}
          />
          <span className="micro">CLEAR STYLE</span>
          <span className="data text-[10px]" style={{ color: "var(--text-soft)" }}>
            thinner — more wallpaper, harder on small text
          </span>
        </label>
      )}
    </div>
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

/**
 * The one lever over a rendering artefact we do not otherwise control.
 *
 * Claude's TUI draws a background plate behind quoted text and its status line.
 * On a glass terminal that plate can come back as an opaque black box around
 * each styled word — with gaps where the spaces are, which is the tell: a
 * *background run* would colour the spaces too, so what is being painted is
 * per-glyph, and that points at xterm's WebGL glyph atlas rather than at any
 * colour rmux chooses. The theme palette cannot reach it; the renderer can.
 *
 * Left on by default deliberately. The DOM renderer has no such artefact but is
 * visibly slower to scroll a constantly-redrawing TUI, which was itself a real
 * complaint about this pane. Whichever bothers you more is not a judgement this
 * app can make for you.
 */
function TerminalRendering() {
  const [gpu, setGpu] = useState(gpuRendering);

  return (
    <section
      className="flex flex-col gap-2"
      style={{ borderTop: "1px solid var(--border)", paddingTop: 16 }}
    >
      <h2 className="kicker">TERMINAL RENDERING</h2>
      <label className="flex items-start gap-2">
        <input
          type="checkbox"
          checked={gpu}
          style={{ accentColor: "rgb(var(--primary))", marginTop: 2 }}
          onChange={(e) => {
            setGpu(e.target.checked);
            setGpuRendering(e.target.checked);
            // **Reload, because nothing else applies it.** The renderer is
            // chosen when an xterm is constructed, and every pane in rmux stays
            // mounted for the life of the run — that is what makes switching
            // sessions instant. So there is no "reopen the tab" that rebuilds
            // it; the advice this panel used to give was simply wrong, and the
            // switch appeared to do nothing at all.
            //
            // A reload is cheap here in a way it would not be in most apps:
            // terminals reattach by name and Claude reattaches by session, both
            // running under the agent on the target. Nothing is lost.
            setTimeout(() => window.location.reload(), 150);
          }}
        />
        <span className="flex flex-col gap-[2px]">
          <span className="data text-[11px]" style={{ color: "var(--text)" }}>
            Draw terminals on the GPU
          </span>
          <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            On by default, and faster. Turn it off if Claude's quoted text shows black boxes
            around each word — that artefact comes from the GPU renderer's glyph cache and no
            colour setting can reach it. Scrolling a busy TUI will be slower.
          </span>
        </span>
      </label>
      <span className="micro" style={{ color: "var(--text-faint)" }}>
        RELOADS THE WINDOW — SESSIONS REATTACH, NOTHING IS LOST
      </span>
    </section>
  );
}
