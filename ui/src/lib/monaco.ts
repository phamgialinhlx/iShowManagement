import * as monaco from "monaco-editor";
import editorWorker from "monaco-editor/editor/editor.worker.js?worker";
import jsonWorker from "monaco-editor/language/json/json.worker.js?worker";
import cssWorker from "monaco-editor/language/css/css.worker.js?worker";
import htmlWorker from "monaco-editor/language/html/html.worker.js?worker";
import tsWorker from "monaco-editor/language/typescript/ts.worker.js?worker";

/**
 * Monaco setup: workers, theme, and language detection.
 *
 * Monaco is bundled, never fetched from a CDN. Its default loader pulls ~5MB
 * from unpkg at runtime, which would make the editor unusable offline — a
 * ridiculous failure mode for a tool whose entire job is working on machines
 * over SSH, often from a laptop with no route to the public internet.
 *
 * The workers are what make it an editor rather than a highlighter: tokenization
 * and language services run off the main thread, so typing stays responsive in a
 * large file. They are declared as Vite `?worker` imports so they end up as real
 * same-origin files in the bundle.
 */

// Vite's `?worker` returns a constructor; Monaco asks for one per language.
self.MonacoEnvironment = {
  getWorker(_workerId: string, label: string) {
    switch (label) {
      case "json":
        return new jsonWorker();
      case "css":
      case "scss":
      case "less":
        return new cssWorker();
      case "html":
      case "handlebars":
      case "razor":
        return new htmlWorker();
      case "typescript":
      case "javascript":
        return new tsWorker();
      default:
        return new editorWorker();
    }
  },
};

/**
 * The SIGNAL ROOM code theme.
 *
 * Deliberately desaturated. Code sits inside a dense instrument panel, and a
 * typical high-contrast syntax theme would out-shout every control around it —
 * the accent red in particular is reserved for "the operator must act" and must
 * never be spent on a string literal. Structure is carried by weight and small
 * hue shifts instead: sage for strings, sand for numbers, lavender for
 * variables. Red appears only for genuine errors and deletions.
 */
export const THEME_NAME = "signal-room";

let initialized = false;

export function initMonaco() {
  if (initialized) return;
  initialized = true;

  monaco.editor.defineTheme(THEME_NAME, {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "", foreground: "e8e6e1" },
      { token: "comment", foreground: "5f5c56", fontStyle: "italic" },
      { token: "keyword", foreground: "e8e6e1", fontStyle: "bold" },
      { token: "string", foreground: "9fb3a4" },
      { token: "number", foreground: "c9b891" },
      { token: "regexp", foreground: "9fb3a4" },
      { token: "type", foreground: "b8b5ae" },
      { token: "type.identifier", foreground: "b8b5ae" },
      { token: "identifier", foreground: "e8e6e1" },
      { token: "variable", foreground: "a9a6c2" },
      { token: "variable.predefined", foreground: "a9a6c2" },
      { token: "attribute.name", foreground: "a9a6c2" },
      { token: "attribute.value", foreground: "9fb3a4" },
      { token: "function", foreground: "d6d3cd" },
      { token: "tag", foreground: "b8b5ae" },
      { token: "meta", foreground: "7f7c76" },
      { token: "delimiter", foreground: "98958f" },
      { token: "operator", foreground: "98958f" },
      { token: "invalid", foreground: "e63b2e" },
    ],
    colors: {
      // Transparent, so the editor sits on the glass panel rather than punching
      // an opaque rectangle through it.
      "editor.background": "#00000000",
      "editor.foreground": "#e8e6e1",
      "editorLineNumber.foreground": "#5c5953",
      "editorLineNumber.activeForeground": "#98958f",
      // The caret is the ONE place red belongs in the editor (rule 0, and rule 2
      // makes the cursor the only thing allowed to blink).
      "editorCursor.foreground": "#e63b2e",
      // Selection and word-occurrence highlights are a neutral chalk wash. Left
      // to inherit, Monaco derives them from the cursor colour and paints a
      // bright red block around whatever word you are sitting on — which reads
      // as an error on every single line you visit.
      "editor.selectionBackground": "#e8e6e133",
      "editor.inactiveSelectionBackground": "#e8e6e11a",
      "editor.selectionHighlightBackground": "#e8e6e114",
      "editor.wordHighlightBackground": "#e8e6e114",
      "editor.wordHighlightStrongBackground": "#e8e6e11f",
      "editor.findMatchBackground": "#c9b89147",
      "editor.findMatchHighlightBackground": "#c9b89129",
      "editor.lineHighlightBackground": "#e8e6e10a",
      "editorIndentGuide.background1": "#e8e6e114",
      "editorIndentGuide.activeBackground1": "#e8e6e124",
      "editorWidget.background": "#0e0e0e",
      "editorWidget.border": "#e8e6e124",
      "editorSuggestWidget.background": "#0e0e0e",
      "editorSuggestWidget.selectedBackground": "#e8e6e114",
      // Bracket pair colourisation ignores the token rules above and ships a
      // rainbow (gold, orchid, blue) that shouts louder than every control in
      // the app. Overridden with muted steps of the same palette so nesting is
      // still legible without spending attention.
      "editorBracketHighlight.foreground1": "#98958f",
      "editorBracketHighlight.foreground2": "#a9a6c2",
      "editorBracketHighlight.foreground3": "#9fb3a4",
      "editorBracketHighlight.foreground4": "#c9b891",
      "editorBracketHighlight.foreground5": "#b8b5ae",
      "editorBracketHighlight.foreground6": "#7f7c76",
      // An unmatched bracket IS an error the operator must fix — the one place
      // red is correct here.
      "editorBracketHighlight.unexpectedBracket.foreground": "#e63b2e",
      "editorBracketPairGuide.activeBackground1": "#e8e6e124",
      "editorError.foreground": "#e63b2e",
      "editorWarning.foreground": "#e0a44a",
      "scrollbarSlider.background": "#24242499",
      "scrollbarSlider.hoverBackground": "#2e2e2ecc",
      "scrollbarSlider.activeBackground": "#2e2e2e",
    },
  });
}

/**
 * Resolve a Monaco language id from a filename.
 *
 * Driven by Monaco's own registry rather than a hand-written map, so every
 * language it ships with is matched without us maintaining a list that quietly
 * drifts out of date.
 */
export function languageForPath(path: string): string {
  const name = path.split("/").pop() ?? path;
  const lower = name.toLowerCase();
  const dot = lower.lastIndexOf(".");
  const extension = dot >= 0 ? lower.slice(dot) : "";

  for (const language of monaco.languages.getLanguages()) {
    if (language.extensions?.some((e) => e.toLowerCase() === extension)) {
      return language.id;
    }
    // Dotfiles and extensionless names (Dockerfile, Makefile) match by filename.
    if (language.filenames?.some((f) => f.toLowerCase() === lower)) {
      return language.id;
    }
  }

  return "plaintext";
}

export { monaco };
