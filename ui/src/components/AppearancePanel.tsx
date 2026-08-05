import { useEffect, useState } from "react";

import { convertFileSrc } from "@tauri-apps/api/core";


import { api, isTauri } from "../lib/api";
import { BackgroundPicker } from "./BackgroundPicker";
import { loadUserCss, saveUserCss } from "../lib/user-css";
import { gpuRendering, setGpuRendering } from "../lib/terminal-theme";
import {
  type Theme,
  ANSI_KEYS,
  SPECIAL_KEYS,
  ROLE_KEYS,
  isBuiltIn,
} from "../lib/theme";
import {
  themeSnapshot,
  subscribeTheme,
  resolve,
  applyTheme,
  setActiveTheme,
  saveTheme,
  deleteTheme,
  copyName,
} from "../lib/theme-runtime";
import {
  UI_FONTS,
  MONO_FONTS,
  DEFAULT_UI_FONT,
  DEFAULT_MONO_FONT,
  applyFonts,
  applyUiFont,
  applyMonoFont,
} from "../lib/fonts";

/**
 * Everything about how rmux looks, in one panel.
 *
 * This merges what were two Settings tabs — **Palette** (the ANSI theme) and
 * **Appearance** (backdrop, glass, scale) — behind a single Apply bar (ADR-002).
 * Both were "how the app looks", split only by which backend stored them:
 * `theme.toml` via Rust for colour, `localStorage` for material. The split read
 * as arbitrary, and two `sticky` footers cannot share one scroll container.
 *
 * The colour half stages a theme *selection* and *colour edits* (previewed live,
 * written to `theme.toml` on Apply). The material half stages a `draft` against
 * `saved`. One `dirty`, one Apply, one Discard commit or revert both. Three
 * things stay instant, under their own boundary: the theme library ops
 * (Duplicate/Delete), the GPU toggle, and custom CSS.
 *
 * The material knobs are the same two the previous app exposed under Appearance ›
 * Glass, and for the same reason: the right amount of translucency depends
 * entirely on what is behind the window, which is not a judgement this app can
 * make on the operator's behalf.
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

  /**
   * Typeface ids (ADR-003), resolved through `lib/fonts.ts`. Stored as stable
   * ids rather than CSS stacks so a font's fallbacks can change without
   * rewriting saved settings. `uiFont` drives the chrome; `monoFont` drives
   * every monospace surface — terminal, Claude, editor and data — at once.
   *
   * Unlike the material knobs above, these two *preview live* (like a theme
   * chip): picking one repaints immediately, but the choice still only persists
   * on Apply.
   */
  uiFont: string;
  monoFont: string;
};

const DEFAULTS: Appearance = {
  tint: 38,
  glass: false,
  glassClear: false,
  overlay: 14,
  // **A first run is opaque black, not the desktop.**
  //
  // Showing the wallpaper through is the more striking default and it was the
  // original one, but it is the wrong thing to *start* someone on: the app's
  // legibility then depends on a picture rmux did not choose and cannot see.
  // A busy or light wallpaper puts 9px labels over arbitrary colour, which is
  // a first impression of "I cannot read this" rather than of the design.
  //
  // Black is the neutral floor every token in `signal-room.css` was measured
  // against — the contrast ratios in the design rules assume it. Anyone who
  // wants the desktop is one radio button away, and that choice is theirs to
  // make once they can see the interface.
  background: "color",
  backgroundColor: "#0b0b0d",
  backgroundCover: 100,
  scale: 100,
  // Today's look exactly — an absent/old setting is unchanged (SFU Futura +
  // IBM Plex Mono), and `load()`'s spread fills these in for pre-existing
  // stored appearances.
  uiFont: DEFAULT_UI_FONT,
  monoFont: DEFAULT_MONO_FONT,
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

  // The typefaces. `applyFonts` writes `--font-display`/`--font-mono` and fires
  // the theme event so the terminals and Monaco re-read (they cache the family
  // and do not watch CSS — same reason a colour has to be pushed into xterm).
  // The chrome and the metric widgets read the tokens and follow with nothing
  // further. This runs before the `isTauri()` return below so it applies in the
  // browser check harnesses too.
  applyFonts(a.uiFont, a.monoFont);

  // Text and accent are no longer set here — the ANSI theme owns every colour now
  // (Settings › Palette, `lib/theme-runtime.ts`). Two writers of `--text` /
  // `--primary` would fight, so this panel keeps only the *material* knobs (tint,
  // glass, background, scale) and leaves colour to the theme.

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
  // **One panel, two backends, one Apply (ADR-002).** The material half stages a
  // `draft` against `saved` (localStorage); the colour half stages a theme
  // selection and colour edits (theme.toml, via Rust). A single `dirty` flag and
  // a single Apply commit both. Nothing crosses from staged to saved without
  // Apply — live-applying every keystroke reads as the app changing under you,
  // worst of all on the interface-scale slider, which re-lays the whole window
  // out mid-drag.
  const [saved, setSaved] = useState<Appearance>(load);
  const [draft, setDraft] = useState<Appearance>(saved);
  const [glassAvailable, setGlassAvailable] = useState(false);
  const [restarting, setRestarting] = useState(false);

  // The colour half, lifted in from the old PalettePanel so one Apply bar
  // governs it too. `selectedName` is the *staged* active choice — previewed via
  // `applyTheme`, persisted only on Apply. `colourDraft` is a forked/edited
  // theme; `origin` remembers what it forked from, for rename cleanup.
  // Duplicate/Delete stay instant (they manage the library, not the composed
  // look — ADR-002 §3), so they carry their own `busy`/`error`.
  const [snap, setSnap] = useState(themeSnapshot);
  const [selectedName, setSelectedName] = useState(snap.active);
  const [colourDraft, setColourDraft] = useState<Theme | null>(null);
  const [origin, setOrigin] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => subscribeTheme(() => setSnap(themeSnapshot())), []);

  // Follow the active theme when it moves under us — a commit here, or a switch
  // from another window — but not while a colour edit is mid-fork, or the
  // preview would jump out from under the operator's hands.
  useEffect(() => {
    if (!colourDraft) setSelectedName(snap.active);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snap.active]);

  const materialDirty = JSON.stringify(draft) !== JSON.stringify(saved);
  const themeStaged = selectedName !== snap.active;
  const colourEdited = colourDraft !== null;
  const dirty = materialDirty || themeStaged || colourEdited;

  // What the editor shows and what previews live on the workbench.
  const preview = colourDraft ?? resolve(selectedName, snap.user);
  const editingBuiltIn = isBuiltIn(preview.name);

  /** Stage a theme switch: preview it live, persist nothing until Apply. */
  const switchTo = (name: string) => {
    setColourDraft(null);
    setOrigin(null);
    setSelectedName(name);
    applyTheme(resolve(name, snap.user));
  };

  /** A colour well changed: fork a built-in on first touch, then preview live. */
  const edit = (field: keyof Theme, value: string) => {
    let base = colourDraft;
    let baseOrigin = origin;
    if (!base) {
      const sel = resolve(selectedName, snap.user);
      base = sel;
      baseOrigin = sel.name;
      if (isBuiltIn(sel.name)) base = { ...sel, name: copyName(sel.name) };
    }
    const next = { ...base, [field]: value } as Theme;
    setOrigin(baseOrigin);
    setColourDraft(next);
    applyTheme(next);
  };

  /**
   * Pick a font. Fonts preview live (ADR-003 §5) — colour and font are the two
   * live axes, so this repaints the type immediately and stages the choice into
   * `draft` (which lights the Apply bar via `materialDirty`). Each handler
   * applies *only its own axis*, so it touches neither the other role nor a
   * staged scale/backdrop change — and does not depend on the other role's
   * possibly-stale draft value.
   */
  const pickUiFont = (id: string) => {
    applyUiFont(id);
    setDraft((a) => ({ ...a, uiFont: id }));
  };
  const pickMonoFont = (id: string) => {
    applyMonoFont(id);
    setDraft((a) => ({ ...a, monoFont: id }));
  };

  /** Commit everything — both backends — in one press. Throws on failure so
   *  Apply & restart can skip the relaunch. */
  const commit = async () => {
    setBusy("Saving…");
    setError(null);
    try {
      // Colour half first (async, can fail), then material (synchronous).
      if (colourDraft) {
        await saveTheme(colourDraft);
        // A rename of a user theme leaves the old name behind; remove it. A fork
        // of a built-in keeps the built-in (it lives in code, not the file).
        if (origin && origin !== colourDraft.name && !isBuiltIn(origin)) {
          await deleteTheme(origin);
        }
        await setActiveTheme(colourDraft.name);
        setSelectedName(colourDraft.name);
      } else if (themeStaged) {
        await setActiveTheme(selectedName);
      }
      setColourDraft(null);
      setOrigin(null);

      // Material. Other windows learn about this through the `storage` event,
      // which does not fire in the document that wrote it — so this window
      // applies it directly and every other one is told.
      localStorage.setItem(STORAGE_KEY, JSON.stringify(draft));
      applyAppearance(draft);
      setSaved(draft);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      throw e;
    } finally {
      setBusy(null);
    }
  };

  /** Drop every staged change and repaint the live preview to the saved look. */
  const discard = () => {
    setDraft(saved);
    setColourDraft(null);
    setOrigin(null);
    setSelectedName(snap.active);
    applyTheme(resolve(snap.active, snap.user));
    // Fonts previewed live, so repaint them back to the saved look too.
    applyFonts(saved.uiFont, saved.monoFont);
  };

  /** Instant library op: copy the previewed theme, stage the copy as selected. */
  const duplicate = async () => {
    setBusy("Duplicating…");
    setError(null);
    try {
      const copy = { ...resolve(selectedName, snap.user), name: copyName(selectedName) };
      await saveTheme(copy); // writes the file now; leaves `active` alone
      setColourDraft(null);
      setOrigin(null);
      setSelectedName(copy.name); // staged selection, not persisted-active
      applyTheme(copy);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  /** Instant library op: delete the previewed theme, revert preview to saved. */
  const remove = async () => {
    const target = resolve(selectedName, snap.user).name;
    setBusy("Deleting…");
    setError(null);
    try {
      await deleteTheme(target);
      const s = themeSnapshot();
      setColourDraft(null);
      setOrigin(null);
      setSelectedName(s.active);
      applyTheme(resolve(s.active, s.user));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
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
    <section className="flex max-w-[560px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">APPEARANCE</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Everything about how rmux looks: the colours it is drawn from, the typefaces, the
          backdrop behind the window, and the interface scale. Nothing here is written until you
          press Apply — except the two live controls at the bottom.
        </p>
      </header>

      {/* ── COLOURS ─────────────────────────────────────────────────────────
          The ANSI theme, lifted in from the old Palette tab. Every colour in
          rmux — chrome, terminals, editor — derives from the active theme.
          Switching a chip previews live but stages the choice; a colour edit
          forks a built-in and previews; both persist only on Apply. */}
      <div className="flex flex-col gap-1">
        <span className="micro">PALETTE</span>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Pick a theme, or edit its 23 colours — the ANSI 16 plus Background, Text, Bold Text,
          Selection and Cursor, and two roles the terminal has no slot for: Accent (&ldquo;you
          must act&rdquo;) and Working. Built-ins are read-only; editing one forks a copy.
        </p>
      </div>

      {/* The theme list. Selecting stages a switch and previews it. */}
      <div className="flex flex-col gap-1">
        <span className="micro">THEMES</span>
        <div className="flex flex-wrap gap-2">
          {snap.all.map((t) => (
            <button
              key={t.name}
              type="button"
              className="chip"
              aria-pressed={t.name === selectedName}
              disabled={busy !== null}
              onClick={() => switchTo(t.name)}
              title={isBuiltIn(t.name) ? "Built-in (read-only)" : "Your theme"}
            >
              <Dot theme={t} />
              {t.name}
              {isBuiltIn(t.name) ? "" : " ·"}
            </button>
          ))}
        </div>
      </div>

      {/* Duplicate / delete — instant library ops (ADR-002 §3). */}
      <div className="flex items-center gap-2">
        <button type="button" className="chip" disabled={busy !== null} onClick={() => void duplicate()}>
          DUPLICATE
        </button>
        {!editingBuiltIn && (
          <button type="button" className="chip" disabled={busy !== null} onClick={() => void remove()}>
            DELETE
          </button>
        )}
        <span className="data text-[10px] ml-auto" style={{ color: "var(--text-faint)" }}>
          {editingBuiltIn ? "Built-in — edits fork a copy" : "Your theme"}
        </span>
      </div>

      {/* The colour editor. */}
      <div className="flex flex-col gap-4" style={{ borderTop: "1px solid var(--border)", paddingTop: 16 }}>
        <div className="flex items-baseline justify-between">
          <span className="micro">EDITING</span>
          <span className="data text-[12px]" style={{ color: "var(--text)" }}>
            {preview.name}
          </span>
        </div>

        <div className="flex flex-col gap-2">
          <span className="micro">ANSI · NORMAL</span>
          <div className="flex flex-wrap gap-2">
            {ANSI_KEYS.slice(0, 8).map((k, i) => (
              <Well key={k} label={ANSI_LABELS[i] ?? k} value={preview[k]} onChange={(v) => edit(k, v)} />
            ))}
          </div>
          <span className="micro">ANSI · BRIGHT</span>
          <div className="flex flex-wrap gap-2">
            {ANSI_KEYS.slice(8).map((k, i) => (
              <Well key={k} label={ANSI_LABELS[i] ?? k} value={preview[k]} onChange={(v) => edit(k, v)} />
            ))}
          </div>
        </div>

        <div className="flex flex-col gap-2">
          <span className="micro">SPECIAL</span>
          <div className="flex flex-wrap gap-2">
            {SPECIAL_KEYS.map((k) => (
              <Well key={k} label={SPECIAL_LABELS[k]} value={preview[k]} onChange={(v) => edit(k, v)} />
            ))}
          </div>
        </div>

        <div className="flex flex-col gap-2">
          <span className="micro">ROLE</span>
          <div className="flex flex-wrap gap-2">
            {ROLE_KEYS.map((k) => (
              <Well key={k} label={ROLE_LABELS[k]} value={preview[k]} onChange={(v) => edit(k, v)} />
            ))}
          </div>
        </div>
      </div>

      {/* ── TYPE ────────────────────────────────────────────────────────────
          The two font roles (ADR-003). Like a theme chip, picking one previews
          live across chrome, terminal and editor and stages the choice; Apply
          persists, Discard reverts. Each chip renders its own label in its own
          face, so the list previews itself. Placed here, between the two
          live-preview axes (colour above) and the apply-on-Apply material knobs
          (below), so layout matches the preview split. */}
      <div className="flex flex-col gap-4" style={{ borderTop: "1px solid var(--border)", paddingTop: 16 }}>
        <div className="flex flex-col gap-1">
          <span className="kicker">TYPE</span>
          <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            The typefaces rmux is drawn with. The UI font sets labels, headings and body; the mono
            font sets every monospace surface at once — the terminal, the Claude pane, the code
            editor and the data readouts. Both preview as you pick them, and persist on Apply.
          </p>
        </div>

        <div className="flex flex-col gap-1">
          <span className="micro">UI FONT</span>
          <div className="flex flex-wrap gap-2">
            {UI_FONTS.map((f) => (
              <button
                key={f.id}
                type="button"
                className="chip"
                aria-pressed={draft.uiFont === f.id}
                onClick={() => pickUiFont(f.id)}
                style={{ fontFamily: f.stack }}
                title={f.provider === "system" ? "Your OS's own font — not identical across machines" : undefined}
              >
                {f.label}
              </button>
            ))}
          </div>
        </div>

        <div className="flex flex-col gap-1">
          <span className="micro">MONO FONT</span>
          <div className="flex flex-wrap gap-2">
            {MONO_FONTS.map((f) => (
              <button
                key={f.id}
                type="button"
                className="chip"
                aria-pressed={draft.monoFont === f.id}
                onClick={() => pickMonoFont(f.id)}
                style={{ fontFamily: f.stack }}
                title={f.provider === "system" ? "Your OS's own font — not identical across machines" : undefined}
              >
                {f.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* ── MATERIAL ────────────────────────────────────────────────────── */}
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

      <button
        type="button"
        className="btn self-start"
        onClick={() => {
          // The stored picture goes too. Resetting the settings while leaving a
          // wallpaper on disk is a file nobody will ever find again.
          if (draft.backgroundImage) void api.backgroundClear().catch(() => {});
          setDraft(DEFAULTS);
          // Fonts preview live, so a reset repaints them to the defaults now;
          // the material knobs it also resets still wait for Apply.
          applyFonts(DEFAULTS.uiFont, DEFAULTS.monoFont);
        }}
      >
        Reset everything
      </button>

      {/*
        The two controls that follow take effect as you type — a stylesheet you
        cannot see take effect is one you cannot debug — while everything above
        stages into the Apply bar. The boundary is labelled rather than guessed.
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

      {/*
        The Apply bar is the **last child** deliberately: `sticky bottom-0` only
        pins to the bottom of the scroll area if nothing follows it. It used to sit
        above the two live controls, so scrolling down to them un-stuck it and it
        drifted up the page. As the final element it stays pinned while the rest of
        the panel scrolls under it.
      */}
      <ApplyBar
        dirty={dirty}
        busy={busy}
        error={error}
        restarting={restarting}
        onApply={() => void commit().catch(() => {})}
        onDiscard={discard}
        onApplyAndRestart={async () => {
          try {
            await commit();
          } catch {
            return; // a failed commit leaves the error showing; do not relaunch
          }
          setRestarting(true);
          // No catch that clears the flag: on success this process is replaced,
          // so there is nothing left to clear. A failure leaves the label
          // showing, which is the honest state — the restart did not happen.
          void api.restartApp().catch(() => setRestarting(false));
        }}
      />
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
  busy,
  error,
  restarting,
  onApply,
  onDiscard,
  onApplyAndRestart,
}: {
  dirty: boolean;
  busy: string | null;
  error: string | null;
  restarting: boolean;
  onApply: () => void;
  onDiscard: () => void;
  onApplyAndRestart: () => void;
}) {
  return (
    <div
      className="sticky bottom-0 -mx-6 -mb-6 mt-1 flex flex-col gap-2 px-6 pb-6 pt-3"
      style={{
        borderTop: "1px solid var(--border-strong)",
        // Near-opaque: this is a sticky footer, so the panel content scrolls
        // under it — at 88% the text behind bled through and read as a smudge.
        background: "color-mix(in srgb, var(--app-panel) 97%, transparent)",
      }}
    >
      <div className="flex items-center gap-3">
        <button
          type="button"
          className="btn"
          disabled={!dirty || restarting || busy !== null}
          onClick={onApply}
        >
          Apply
        </button>
        <button
          type="button"
          className="btn"
          disabled={restarting || busy !== null}
          onClick={onApplyAndRestart}
          title="Applies your changes, then relaunches so the terminals re-measure cleanly."
        >
          {restarting ? "Restarting…" : "Apply & restart"}
        </button>
        {dirty && !restarting && !busy && (
          <button type="button" className="chip ml-auto" onClick={onDiscard}>
            DISCARD
          </button>
        )}
      </div>

      {/* Status, in priority order: an error persists until the next attempt; a
          restart or an in-flight save reports itself; a dirty panel explains the
          preview split (colours live, material on Apply); at rest it points at
          the canonical file. */}
      <span
        className="data text-[10px] leading-relaxed"
        style={{ color: error ? "rgb(var(--primary))" : dirty || busy ? "rgb(var(--busy))" : "var(--text-soft)" }}
      >
        {error
          ? error
          : restarting
            ? "Relaunching. Your sessions keep running on their hosts and will reattach."
            : busy
              ? busy
              : dirty
                ? "Colours and fonts preview live; backdrop and scale apply on Apply. Nothing is written until you press it."
                : "Saved to theme.toml and your appearance settings — you can hand-edit theme.toml and the app repaints. Restart only helps if the terminals look off after a scale change; sessions survive it."}
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

/* ────────────────────────── the colour editor ──────────────────────────
 * Lifted from the former PalettePanel so one Apply bar governs colour too
 * (ADR-002). Purely presentational — all state lives in AppearancePanel. */

/** Short labels for the ANSI grid, in `ANSI_KEYS` order. */
const ANSI_LABELS = ["BLK", "RED", "GRN", "YEL", "BLU", "MAG", "CYN", "WHT"];

const SPECIAL_LABELS: Record<(typeof SPECIAL_KEYS)[number], string> = {
  background: "BACKGROUND",
  foreground: "TEXT",
  boldText: "BOLD TEXT",
  selection: "SELECTION",
  cursor: "CURSOR",
};

const ROLE_LABELS: Record<(typeof ROLE_KEYS)[number], string> = {
  accent: "ACCENT · ACT",
  working: "WORKING",
};

/** A tiny two-colour swatch — background and accent — to tell themes apart. */
function Dot({ theme }: { theme: Theme }) {
  return (
    <span
      className="round"
      style={{
        width: 10,
        height: 10,
        display: "inline-block",
        background: theme.background,
        boxShadow: `inset 0 0 0 2px ${theme.accent}`,
      }}
    />
  );
}

/**
 * One colour well.
 *
 * The swatch is a `div` whose background *is* the value, with a transparent
 * `<input type=color>` on top to open the picker. That is deliberate: WKWebView
 * (the webview rmux runs in) does not paint a styled `input[type=color]` with its
 * value the way Chrome does — it came out a black box, which read as "the wells
 * are broken". Painting the colour ourselves shows it in every engine; the native
 * input is only the click target and the picker.
 */
function Well({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col items-center gap-1" style={{ width: 56 }}>
      <span
        className="relative block h-7 w-full"
        style={{ background: value, border: "1px solid var(--border-strong)" }}
        title={value.toUpperCase()}
      >
        <input
          type="color"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
          aria-label={label.toLowerCase()}
        />
      </span>
      <span className="micro" style={{ fontSize: 8, letterSpacing: "0.08em" }}>
        {label}
      </span>
    </label>
  );
}
