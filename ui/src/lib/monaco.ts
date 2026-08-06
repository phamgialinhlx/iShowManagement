import * as monaco from "monaco-editor";

import { monacoTheme } from "./theme";
import { activeTheme } from "./theme-runtime";
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
 * ## Code is colour-coded, and that is not a break with the design system
 *
 * This theme used to be near-monochrome: `keyword` and `identifier` were both
 * `#e8e6e1`, so a Python file rendered as one flat grey with a slightly dimmer
 * comment. The reasoning was that code sits in a dense instrument panel and a
 * loud syntax theme would out-shout the controls around it. That was wrong, and
 * the screenshot of a real file made it obvious: syntax colour is not
 * decoration, it is the parse. Reading unhighlighted code means doing the
 * tokenizer's job yourself on every line.
 *
 * Rule 0 — red only where the operator must act — is untouched. Red still
 * appears nowhere here but genuine errors, unmatched brackets and the caret.
 * Syntax colour is a *different axis* from the alarm palette, and conflating
 * them was the mistake: suppressing every hue to protect one of them left the
 * app with no way to say "this is a string".
 *
 * ## The hues are the terminal's hues
 *
 * The six are taken from `TERMINAL_THEME` verbatim, so a string is the same
 * green whether it is in the editor, in a transcript code block, or printed by
 * a program in a shell. Three surfaces disagreeing about what green means is
 * three palettes to learn instead of one.
 */
export const THEME_NAME = "signal-room";

let initialized = false;

export function initMonaco() {
  if (initialized) return;
  initialized = true;

  defineActiveTheme();

  // Re-derive when the ANSI theme changes, and re-apply so open editors and any
  // colourised transcript pick up the new palette. Monaco caches the theme's
  // token stylesheet, so a redefine plus `setTheme` is what updates live views.
  if (typeof window !== "undefined") {
    window.addEventListener("rmux-theme", () => {
      defineActiveTheme();
      monaco.editor.setTheme(THEME_NAME);
    });
  }
}

/**
 * Define (or redefine) the editor theme from the active palette.
 *
 * The mapping — keyword=magenta, string=green, the one red for errors and the
 * caret — lives in `monacoTheme` (`lib/theme.ts`) so it is derived from the same
 * 23 colours the terminal uses, rather than hard-coded here. `monacoTheme`
 * returns exactly `defineTheme`'s shape; the cast is only because it is typed
 * structurally to stay free of a monaco import.
 */
function defineActiveTheme() {
  monaco.editor.defineTheme(
    THEME_NAME,
    monacoTheme(activeTheme()) as monaco.editor.IStandaloneThemeData,
  );
}

/**
 * Highlight a snippet to HTML, using the same tokenizer the editor uses.
 *
 * Monaco is already bundled, so this costs nothing extra — and it means there is
 * **one definition of what highlighting is**. A second highlighter for
 * transcript code blocks would be a second palette and a second set of language
 * rules, and the two would disagree about the same file within a week.
 *
 * The returned HTML is Monaco's own: it escapes the source text as it
 * tokenizes, so the code being highlighted cannot introduce markup. That
 * matters because this text is a *transcript* — it is whatever Claude printed,
 * including file contents from a machine we do not control.
 *
 * Resolves to `null` when the language is unknown or tokenizing fails, so the
 * caller can fall back to plain text rather than showing nothing.
 */
export async function highlight(code: string, language: string): Promise<string | null> {
  const id = languageId(language);
  if (!id) return null;

  initMonaco();
  // `colorize` reads the *current* theme, which is only set once an editor has
  // been created — without this a transcript opened before any file would be
  // highlighted in Monaco's stock `vs-dark`.
  monaco.editor.setTheme(THEME_NAME);
  ensureThemeStyles();

  try {
    return await monaco.editor.colorize(code, id, { tabSize: 4 });
  } catch {
    return null;
  }
}

/**
 * Make sure the theme's token stylesheet exists in the document.
 *
 * **This is the whole reason `colorize` looked broken.** It returns markup like
 * `<span class="mtk21">def</span>` — class names, not colours — and the
 * stylesheet that gives `.mtk21` a colour is injected by Monaco's theme service
 * only when an *editor* is constructed. So a transcript rendered before any file
 * had been opened produced perfectly tokenized HTML that painted in one
 * inherited grey: the exact symptom of no highlighting at all, from code that
 * was working.
 *
 * Constructing one throwaway editor is what triggers the injection, and the
 * stylesheet outlives its disposal — measured, both halves, in a real browser
 * (`ui/highlight-check.html`). Doing it lazily keeps the cost off anyone who
 * never opens a transcript.
 */
let themeStylesReady = false;

function ensureThemeStyles() {
  if (themeStylesReady) return;
  themeStylesReady = true;

  const host = document.createElement("div");
  // Off-screen rather than `display: none`: Monaco measures on construction,
  // and a zero-sized *hidden* subtree is the case its layout code bails on.
  host.style.cssText =
    "position:absolute;top:-9999px;left:-9999px;width:0;height:0;overflow:hidden";
  document.body.appendChild(host);

  try {
    const editor = monaco.editor.create(host, {
      value: "",
      theme: THEME_NAME,
      automaticLayout: false,
    });
    editor.dispose();
  } catch {
    // Highlighting degrades to plain text rather than taking the view down.
  } finally {
    host.remove();
  }
}

/** Resolve a fence's language hint (`python`, `py`, `sh`) to a Monaco id. */
export function languageId(hint: string): string | null {
  const wanted = hint.trim().toLowerCase();
  if (!wanted || wanted === "plaintext" || wanted === "text") return null;

  for (const language of monaco.languages.getLanguages()) {
    if (language.id === wanted) return language.id;
    // Monaco records the aliases people actually type — "py", "sh", "yml".
    if (language.aliases?.some((a) => a.toLowerCase() === wanted)) return language.id;
    if (language.extensions?.some((e) => e.toLowerCase() === `.${wanted}`)) return language.id;
  }
  return null;
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
