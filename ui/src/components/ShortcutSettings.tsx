import { useEffect, useRef, useState } from "react";

import {
  ACTIONS,
  DEFAULTS,
  accelOf,
  conflicts,
  hasModifier,
  isMac,
  label,
  reachesTerminal,
  readBindings,
  writeBindings,
  type ActionId,
  type Bindings,
} from "../lib/shortcuts";
import {
  PREFIX_ACTIONS,
  defaultConfig,
  isModifierOnly,
  prefixConflicts,
  readPrefixConfig,
  tokenLabel,
  tokenOf,
  validatePrefix,
  writePrefixConfig,
  type PrefixActionId,
  type PrefixConfig,
} from "../lib/prefix-mode";

/**
 * Rebinding, by pressing the keys.
 *
 * A text field asking someone to type `Mod+Alt+ArrowLeft` is a field where the
 * only way to find out whether you spelled it right is to close settings and
 * try. Pressing the chord *is* the input, and it is also the only way to
 * discover that a key you meant to use is intercepted by the OS before the app
 * ever sees it.
 *
 * ## Live, not staged
 *
 * Appearance edits a draft because a slider that re-lays the window out on
 * every tick reads as the app changing under your hands. A shortcut is the
 * opposite: its whole value is that it works, and the way you check is to press
 * it. Staging it behind Apply would mean the thing under test is never the
 * thing bound.
 */
export function ShortcutSettings() {
  const [bindings, setBindings] = useState<Bindings>(() => readBindings());
  const [capturing, setCapturing] = useState<ActionId | null>(null);
  const [refused, setRefused] = useState<string | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  const clashes = conflicts(bindings);
  const clashing = new Set(clashes.flat());

  /**
   * Bindings the terminal *also* acts on.
   *
   * Worth its own marker rather than folding into DOUBLE-BOUND, because the
   * failure is different and much harder to diagnose: nothing in rmux looks
   * wrong at all. `useShortcuts` listens on the bubble, so xterm has already
   * written to the pty by the time the shortcut runs — both things happen, and
   * the shell half is invisible until someone notices their history jumping.
   *
   * **Shown off macOS only.** ⌘ is not a terminal modifier, so there is little
   * to warn about there; and the one macOS chord this would flag —
   * `Mod+Alt+Arrow`, four of the shipped defaults — depends on xterm's
   * `macOptionIsMeta` and was *not* measured. Showing an unverified warning
   * over defaults that platform ships and tests would teach the operator to
   * ignore the marker, which costs more than the marker is worth.
   */
  const noisy = new Set(
    isMac() ? [] : ACTIONS.filter((a) => reachesTerminal(bindings[a.id] ?? "")).map((a) => a.id),
  );

  /**
   * While capturing, this listener owns the keyboard.
   *
   * Capture phase and `preventDefault` on everything, which is the one place in
   * the app that is right: the operator has explicitly asked to press a chord,
   * so a `⌘2` here must be recorded rather than switching a view — including
   * chords that are already bound to something.
   */
  useEffect(() => {
    if (!capturing) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();

      if (e.key === "Escape") {
        setCapturing(null);
        setRefused(null);
        return;
      }
      // A modifier on its own is a chord in progress, not a chord. Recording
      // "Mod" the instant ⌘ goes down would make every capture impossible.
      if (["Meta", "Control", "Alt", "Shift"].includes(e.key)) return;

      const chord = accelOf(e);
      if (!hasModifier(chord)) {
        // Refused with the reason, because the reason is not obvious and the
        // consequence is invisible: a bare key would be swallowed before the
        // terminal ever saw it.
        setRefused(`${label(chord)} needs ⌘, ⌥ or ⌃ — a bare key would be taken from the terminal`);
        return;
      }
      const next = { ...bindings, [capturing]: chord };
      setBindings(next);
      writeBindings(next);
      setCapturing(null);
      setRefused(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [capturing, bindings]);

  const resetAll = () => {
    const next = { ...DEFAULTS };
    setBindings(next);
    writeBindings(next);
    setRefused(null);
  };

  return (
    <div className="flex flex-col gap-3" ref={boxRef}>
      <div className="flex items-baseline justify-between gap-3">
        <span className="kicker">SHORTCUTS</span>
        <button type="button" className="chip" onClick={resetAll}>
          RESET ALL
        </button>
      </div>

      <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        Click a shortcut and press the keys you want. These work while a terminal has focus —
        which is why every one of them needs a modifier: a bare key would be taken from the
        shell before it ever got there.
      </p>

      <ul className="flex flex-col">
        {ACTIONS.map((action) => {
          const isCapturing = capturing === action.id;
          const clash = clashing.has(action.id);
          return (
            <li
              key={action.id}
              className="flex items-center gap-3 border-b py-[6px]"
              style={{ borderColor: "var(--border)" }}
            >
              <div className="flex min-w-0 flex-1 flex-col">
                <span className="data text-[12px]" style={{ color: "var(--text)" }}>
                  {action.label}
                </span>
                <span className="micro" style={{ color: "var(--text-faint)" }}>
                  {action.hint}
                </span>
              </div>

              {clash && (
                // Reported, not prevented — see `conflicts`. Mid-rebind, two
                // actions sharing a chord is a normal state to pass through.
                <span className="micro shrink-0" style={{ color: "rgb(var(--primary))" }}>
                  DOUBLE-BOUND
                </span>
              )}

              {noisy.has(action.id) && (
                <span className="micro shrink-0" style={{ color: "rgb(var(--primary))" }}>
                  REACHES SHELL
                </span>
              )}

              <button
                type="button"
                className="chip shrink-0"
                style={
                  isCapturing
                    ? { color: "rgb(var(--busy))", borderColor: "rgb(var(--busy))" }
                    : undefined
                }
                onClick={() => {
                  setRefused(null);
                  setCapturing(isCapturing ? null : action.id);
                }}
              >
                {isCapturing ? "PRESS KEYS · ESC" : label(bindings[action.id] ?? "")}
              </button>
            </li>
          );
        })}
      </ul>

      {refused && (
        <span className="micro leading-relaxed" style={{ color: "rgb(var(--primary))" }}>
          {refused}
        </span>
      )}

      {clashes.length > 0 && (
        <span className="micro leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Two actions share a shortcut. Whichever is listed first will win — rebind one of them.
        </span>
      )}

      {noisy.size > 0 && (
        <span className="micro leading-relaxed" style={{ color: "var(--text-soft)" }}>
          A shortcut marked REACHES SHELL is also sent to the terminal — it will do both, and the
          shell half is silent. Ctrl+Shift+ combinations are free.
        </span>
      )}

      <PrefixSettings />
    </div>
  );
}

/**
 * The prefix layer (`prefix-mode.ts`): the prefix chord(s), and the table of
 * keys that mean something for one keystroke after it.
 *
 * Here a **bare letter is fine** — that is the point of a prefix: `v` is only a
 * command right after `ctrl+b`, so it costs the terminal nothing. What must
 * carry a modifier is the *prefix itself*, which is refused as a bare printable
 * key with the reason, because a prefix of `b` would make b untypeable.
 *
 * Live like the chords above: the way to check a binding is to press it.
 */
function PrefixSettings() {
  const [config, setConfig] = useState<PrefixConfig>(() => readPrefixConfig());
  /** An action id, `"prefix:<index>"` to replace a prefix, or `"prefix:new"` to add one. */
  const [capturing, setCapturing] = useState<string | null>(null);
  const [refused, setRefused] = useState<string | null>(null);

  const save = (next: PrefixConfig) => {
    setConfig(next);
    writePrefixConfig(next);
  };

  const clashing = new Set(prefixConflicts(config.bindings, config.prefixes).flat());

  useEffect(() => {
    if (!capturing) return;
    const onKey = (e: KeyboardEvent) => {
      // Capture phase and preventDefault on everything, as the chord capture
      // above: the operator asked to press a key, so it is recorded rather than
      // acted on — including the prefix itself.
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setCapturing(null);
        setRefused(null);
        return;
      }
      if (isModifierOnly(e)) return;
      const token = tokenOf(e);
      if (capturing.startsWith("prefix:")) {
        const verdict = validatePrefix(token);
        if (!verdict.ok) {
          setRefused(verdict.reason);
          return;
        }
        const slot = capturing.slice("prefix:".length);
        const prefixes = [...config.prefixes];
        if (slot === "new") prefixes.push(token);
        else prefixes[Number(slot)] = token;
        save({ ...config, prefixes: [...new Set(prefixes)] });
      } else {
        save({ ...config, bindings: { ...config.bindings, [capturing as PrefixActionId]: token } });
      }
      setCapturing(null);
      setRefused(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [capturing, config]);

  const chip = (id: string, text: string) => {
    const on = capturing === id;
    return (
      <button
        type="button"
        className="chip shrink-0"
        style={on ? { color: "rgb(var(--busy))", borderColor: "rgb(var(--busy))" } : undefined}
        onClick={() => {
          setRefused(null);
          setCapturing(on ? null : id);
        }}
      >
        {on ? "PRESS A KEY · ESC" : text}
      </button>
    );
  };

  let lastGroup = "";
  return (
    <div className="mt-4 flex flex-col gap-3">
      <div className="flex items-baseline justify-between gap-3">
        <span className="kicker">PREFIX</span>
        <button
          type="button"
          className="chip"
          onClick={() => {
            save(defaultConfig());
            setRefused(null);
          }}
        >
          RESET PREFIX
        </button>
      </div>

      <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
        Press the prefix, then one key. The prefix never reaches the terminal on its own — press it
        twice to send it. Esc cancels. The keys below mean something only right after the prefix,
        so a bare letter is safe here.
      </p>

      <div className="flex flex-wrap items-center gap-2">
        <span className="data text-[12px]" style={{ color: "var(--text)" }}>
          Prefix
        </span>
        {config.prefixes.map((p, i) => (
          <span key={`${p}-${i}`} className="inline-flex items-center gap-1">
            {chip(`prefix:${i}`, tokenLabel(p))}
            {config.prefixes.length > 1 && (
              <button
                type="button"
                className="chip"
                aria-label={`Remove prefix ${tokenLabel(p)}`}
                onClick={() => save({ ...config, prefixes: config.prefixes.filter((_, j) => j !== i) })}
              >
                ✕
              </button>
            )}
          </span>
        ))}
        {chip("prefix:new", "+ ADD")}
      </div>

      {/* Bounded: the table grows with every action added. */}
      <ul className="flex max-h-[50vh] flex-col overflow-y-auto">
        {PREFIX_ACTIONS.map((action) => {
          const head = action.group !== lastGroup;
          lastGroup = action.group;
          return (
            <li key={action.id} className="flex flex-col">
              {head && (
                <span className="micro pb-1 pt-2" style={{ color: "var(--text-faint)" }}>
                  {action.group.toUpperCase()}
                </span>
              )}
              <div className="flex items-center gap-3 border-b py-[5px]" style={{ borderColor: "var(--border)" }}>
                <span className="data min-w-0 flex-1 truncate text-[12px]" style={{ color: "var(--text)" }}>
                  {action.label}
                </span>
                {clashing.has(action.id) && (
                  <span className="micro shrink-0" style={{ color: "rgb(var(--primary))" }}>
                    DOUBLE-BOUND
                  </span>
                )}
                {chip(action.id, tokenLabel(config.bindings[action.id] ?? ""))}
              </div>
            </li>
          );
        })}
      </ul>

      <span className="micro leading-relaxed" style={{ color: "var(--text-faint)" }}>
        Fixed: 1–9 pick a tab and the arrows move focus, after the prefix. A key marked DOUBLE-BOUND
        shares its key with another action, the prefix, or one of those fixed keys.
      </span>

      {refused && (
        <span className="micro leading-relaxed" style={{ color: "rgb(var(--primary))" }}>
          {refused}
        </span>
      )}
    </div>
  );
}
