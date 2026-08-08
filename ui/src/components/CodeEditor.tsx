import { useEffect, useRef } from "react";

import { initMonaco, languageForPath, monaco, THEME_NAME } from "../lib/monaco";
import { readMonoStack } from "../lib/fonts";
import type { Buffer } from "../lib/buffers";
import { PanelLoader } from "./PanelLoader";

/**
 * The Monaco editor bound to one buffer.
 *
 * A single Monaco instance is reused across every open file, with a `TextModel`
 * per buffer swapped into it. That is how VS Code itself works, and it matters:
 * constructing an editor costs tens of milliseconds and discards undo history,
 * so a per-tab editor would make switching files feel sluggish and silently drop
 * the ability to undo across a tab change.
 *
 * Models are keyed by buffer, so each file keeps its own undo stack, view state,
 * and cursor position.
 */

const models = new Map<string, monaco.editor.ITextModel>();
const viewStates = new Map<string, monaco.editor.ICodeEditorViewState | null>();

export function disposeBufferModel(key: string) {
  models.get(key)?.dispose();
  models.delete(key);
  viewStates.delete(key);
}

export function CodeEditor({
  buffer,
  onSave,
  onEdit,
}: {
  buffer: Buffer;
  onSave: () => void;
  onEdit: (key: string, text: string) => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);

  // Kept in refs so Monaco's callbacks always see the current handlers without
  // tearing the editor down and rebuilding it on every render.
  const saveRef = useRef(onSave);
  saveRef.current = onSave;
  const editRef = useRef(onEdit);
  editRef.current = onEdit;

  // Latest values, read inside Monaco callbacks without re-creating the editor.
  const bufferRef = useRef(buffer);
  bufferRef.current = buffer;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    initMonaco();

    const editor = monaco.editor.create(host, {
      theme: THEME_NAME,
      automaticLayout: true,
      // The operator's chosen mono font (ADR-003), read from the live token.
      // Monaco caches this, so it is re-applied on the appearance/theme events
      // below.
      fontFamily: readMonoStack(),
      fontSize: 12.5,
      lineHeight: 1.6,
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      renderLineHighlight: "line",
      smoothScrolling: true,
      cursorBlinking: "blink",
      padding: { top: 10, bottom: 10 },
      // The panel already has a border; a second one inside reads as a seam.
      overviewRulerBorder: false,
      hideCursorInOverviewRuler: true,
      scrollbar: { verticalScrollbarSize: 7, horizontalScrollbarSize: 7 },
      tabSize: 2,
      renderWhitespace: "selection",
      // Rule 1: Monaco rounds its widgets by default.
      roundedSelection: false,
      fixedOverflowWidgets: true,
    });

    editorRef.current = editor;
    // The editor that ⌘F and "go to line" act on. Module scope because those
    // callers are keyboard handlers and search results, neither of which sits
    // inside this component's tree.
    active = editor;

    // ⌘S / Ctrl+S inside the editor. Registered on the editor rather than the
    // window so it fires even when Monaco has swallowed the keystroke.
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
      saveRef.current();
    });

    // Adopt a font change (ADR-003). Monaco caches `fontFamily` and does not
    // watch CSS, so the mono font is re-applied when it changes in this document
    // (`rmux-theme`, fired by `applyFonts` for the live preview) or in another
    // window (the `storage` event on `rmux.appearance`).
    const restyleFont = () => editor.updateOptions({ fontFamily: readMonoStack() });
    const onStorage = (e: StorageEvent) => {
      if (!e.key || e.key === "rmux.appearance") restyleFont();
    };
    window.addEventListener("rmux-theme", restyleFont);
    window.addEventListener("storage", onStorage);

    return () => {
      window.removeEventListener("rmux-theme", restyleFont);
      window.removeEventListener("storage", onStorage);
      // Detach the model first: disposing an editor that still owns a shared
      // model would take the model with it, losing every other tab's content.
      editor.setModel(null);
      editor.dispose();
      editorRef.current = null;
      if (active === editor) active = null;
    };
  }, []);

  // Swap in the model for whichever buffer is active.
  useEffect(() => {
    const editor = editorRef.current;
    if (!editor || buffer.content?.kind !== "text") return;

    const previous = editor.getModel();
    if (previous) {
      // Remember where the user was, so returning to this tab restores it.
      for (const [key, model] of models) {
        if (model === previous) viewStates.set(key, editor.saveViewState());
      }
    }

    let model = models.get(buffer.key);
    if (!model || model.isDisposed()) {
      model = monaco.editor.createModel(
        buffer.text,
        languageForPath(buffer.path),
        // A unique URI per buffer keeps language services from confusing two
        // files with the same path on different hosts.
        monaco.Uri.parse(`rmux://${encodeURIComponent(buffer.key)}`),
      );
      models.set(buffer.key, model);

      model.onDidChangeContent(() => {
        editRef.current(buffer.key, model!.getValue());
      });
    }

    editor.setModel(model);
    const state = viewStates.get(buffer.key);
    if (state) editor.restoreViewState(state);
    editor.focus();
  }, [buffer.key, buffer.content?.kind, buffer.path, buffer.text]);

  // A save that rewrites the file elsewhere (or a reload) must be reflected
  // without clobbering the cursor.
  useEffect(() => {
    const model = models.get(buffer.key);
    if (model && !model.isDisposed() && buffer.text !== model.getValue()) {
      model.setValue(buffer.text);
    }
  }, [buffer.key, buffer.text]);

  // Anything that is not editable text is shown *over* the host element rather
  // than instead of it.
  const overlay = buffer.loading ? (
    // Reading a file is an SSH round trip, and a big one is slow enough to read
    // as a hang. `rows` because what arrives is lines of text.
    <PanelLoader variant="rows" phase="READING THE FILE" detail={buffer.path} rows={8} />
  ) : buffer.error ? (
    <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
      {buffer.error}
    </p>
  ) : buffer.content?.kind === "binary" ? (
    <p className="data text-center text-[11px]" style={{ color: "var(--text-soft)" }}>
      Binary file — {buffer.content.bytes.toLocaleString()} bytes.
      <br />
      Not shown, because editing it as text would corrupt it.
    </p>
  ) : buffer.content?.kind === "tooLarge" ? (
    <p className="data text-center text-[11px]" style={{ color: "var(--text-soft)" }}>
      {buffer.content.bytes.toLocaleString()} bytes — too large to open.
    </p>
  ) : null;

  return (
    <div className="relative h-full w-full">
      {/*
        The host element is ALWAYS rendered, never swapped out for a loading or
        error state. Returning a different element while the file loads leaves
        `hostRef` null when the mount effect runs, and because that effect has an
        empty dependency list it never runs again — so Monaco is never created and
        the editor stays permanently blank once the content arrives. That is
        exactly the bug this shape prevents.
      */}
      <div ref={hostRef} className="h-full w-full" />

      {overlay && (
        <div
          className="absolute inset-0 grid place-items-center p-6"
          style={{ background: "var(--app-panel)" }}
        >
          {overlay}
        </div>
      )}
    </div>
  );
}


/**
 * The editor currently on screen, for callers outside this component.
 *
 * There is at most one visible editor per session view, so a single reference is
 * enough — and it is cleared on unmount so a disposed editor is never messaged.
 */
let active: import("monaco-editor").editor.IStandaloneCodeEditor | null = null;

/**
 * Open Monaco's find widget.
 *
 * Needed because Monaco only answers ⌘F when it already has focus. Coming from
 * the file tree — or straight after clicking a search result — the key did
 * nothing, which reads as a missing feature rather than a focus rule. Focus is
 * taken first, because the widget opens against the focused editor.
 */
export function findInOpenFile(): void {
  if (!active) return;
  active.focus();
  active.getAction("actions.find")?.run();
}

/** Scroll to a line and put the caret on it — used by the project search. */
export function revealLine(line: number): void {
  if (!active) return;
  active.revealLineInCenter(line);
  active.setPosition({ lineNumber: line, column: 1 });
  active.focus();
}
