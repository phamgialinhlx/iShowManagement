import { useState } from "react";

import { autosaveEnabled, setAutosaveEnabled, AUTOSAVE_DELAY } from "../lib/autosave";

/**
 * How the file editor behaves.
 *
 * Autosave is **on by default**, which is the unusual choice here and the
 * deliberate one. ⌘S is a habit formed on local disks. rmux's files are usually
 * on another machine, and the only copy of an unsaved edit is text held in a
 * webview — a reload, a crash or a closed window takes it with no warning and no
 * recovery. "I typed it and it was gone" is the worst thing this app can do, so
 * the default is that typing is enough.
 *
 * The switch exists because that is not everyone's answer: a file under a
 * running watcher, a live config, or anything where a half-finished edit
 * reaching disk has consequences of its own. Turning it off restores ⌘S, which
 * never stopped working.
 */
export function EditorPanel() {
  const [auto, setAuto] = useState(autosaveEnabled);

  return (
    <section className="flex max-w-[560px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">EDITOR</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          Files opened here are usually on a server. The buffer holding your changes is not —
          it lives in this window until it is written back.
        </p>
      </header>

      <div className="flex flex-col gap-2">
        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            checked={auto}
            style={{ accentColor: "rgb(var(--primary))", marginTop: 2 }}
            onChange={(e) => {
              setAuto(e.target.checked);
              setAutosaveEnabled(e.target.checked);
            }}
          />
          <span className="flex flex-col gap-[2px]">
            <span className="data text-[11px]" style={{ color: "var(--text)" }}>
              Save automatically as I type
            </span>
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              On by default. The file is written {AUTOSAVE_DELAY / 1000} seconds after you stop
              typing — not on every keystroke, because each save is a whole-file write across the
              network. Closing a tab writes any pending change rather than discarding it.
            </span>
          </span>
        </label>
      </div>

      <div
        className="flex flex-col gap-1"
        style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}
      >
        <span className="micro">WHAT NEVER GETS WRITTEN</span>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          A file that is still loading, or whose read <em>failed</em>, is never saved back — with
          either, the buffer is empty or partial, and writing it would truncate a file whose only
          fault was being opened. Autosave also never touches a file you have not changed, so it
          cannot set off a watcher or a build on its own.
        </p>
      </div>

      <div
        className="flex flex-col gap-1"
        style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}
      >
        <span className="micro">WITH IT OFF</span>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          ⌘S saves the file in front of you, exactly as it always has. A tab with unsaved changes
          shows a filled dot instead of its close cross.
        </p>
      </div>
    </section>
  );
}
