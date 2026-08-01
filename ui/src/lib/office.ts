import JSZip from "jszip";

/**
 * Readers for the OOXML office formats.
 *
 * All three — `.docx`, `.xlsx`, `.pptx` — are zip archives of XML, so they can be
 * read in the webview with no server and no network. That matters: rmux must work
 * against a machine on the other side of the world with nothing installed on it,
 * and a preview that needs a round trip to the Cowork server would break the one
 * architectural rule this project has.
 *
 * The legacy binary formats (`.doc`, `.xls`, `.ppt`) are a completely different,
 * undocumented, pre-2007 encoding. They are not zips and nothing here can read
 * them; they are reported as such rather than half-rendered.
 *
 * Spreadsheets and slides are parsed here by hand rather than with a library.
 * The one maintained npm package for xlsx is pinned at a release with unfixed
 * prototype-pollution and ReDoS advisories, and this code runs in a webview that
 * can reach Tauri IPC — parsing the subset a preview actually needs is both
 * smaller and safer than importing that.
 */

/** A parsed spreadsheet: one entry per sheet, each a grid of display strings. */
export type Sheet = { name: string; rows: string[][] };

/** A parsed deck: one entry per slide, in order, holding its text runs. */
export type Slide = { title: string | null; lines: string[] };

const parser = new DOMParser();

/** Parse OOXML, refusing silently-broken input rather than returning an empty document. */
function parseXml(xml: string): Document {
  const doc = parser.parseFromString(xml, "application/xml");
  if (doc.querySelector("parsererror")) throw new Error("malformed OOXML");
  return doc;
}

/**
 * `A1` → column 0. Sheet cells carry their address rather than their position,
 * and a row omits empty cells entirely — so without decoding this, a row with a
 * gap silently shifts every value after it into the wrong column.
 */
function columnIndex(ref: string): number {
  let index = 0;
  for (const ch of ref) {
    const code = ch.charCodeAt(0);
    if (code < 65 || code > 90) break; // hit the row number
    index = index * 26 + (code - 64);
  }
  return Math.max(0, index - 1);
}

/** Text of an element and all its descendants, in document order. */
const textOf = (node: Element | null) => node?.textContent ?? "";

/**
 * Spreadsheet → grids of strings.
 *
 * Values only. Formulas are shown as their last computed result, which is what
 * the file stores and what the author last saw; formatting, charts and merged
 * cells are dropped. This is a preview, and a grid of the actual numbers is the
 * part that carries the information.
 */
export async function readSpreadsheet(bytes: Uint8Array): Promise<Sheet[]> {
  const zip = await JSZip.loadAsync(bytes);

  // Strings are pooled in one shared table and referenced by index from the
  // cells, so this has to be read before any sheet can be interpreted.
  const sharedFile = zip.file("xl/sharedStrings.xml");
  const shared: string[] = [];
  if (sharedFile) {
    const doc = parseXml(await sharedFile.async("string"));
    for (const si of Array.from(doc.getElementsByTagName("si"))) {
      // A single string may be split across several runs when part of it is
      // styled differently; concatenating the runs is what rebuilds the value.
      shared.push(textOf(si));
    }
  }

  // Sheet display names live in the workbook; the files themselves are numbered.
  const workbookFile = zip.file("xl/workbook.xml");
  const names: string[] = [];
  if (workbookFile) {
    const doc = parseXml(await workbookFile.async("string"));
    for (const sheet of Array.from(doc.getElementsByTagName("sheet"))) {
      names.push(sheet.getAttribute("name") ?? `Sheet${names.length + 1}`);
    }
  }

  const files = zip
    .file(/^xl\/worksheets\/sheet\d+\.xml$/)
    .sort((a, b) => sheetNumber(a.name) - sheetNumber(b.name));

  const sheets: Sheet[] = [];
  for (const [index, file] of files.entries()) {
    const doc = parseXml(await file.async("string"));
    const rows: string[][] = [];

    for (const row of Array.from(doc.getElementsByTagName("row"))) {
      const cells: string[] = [];
      for (const cell of Array.from(row.getElementsByTagName("c"))) {
        const at = columnIndex(cell.getAttribute("r") ?? "");
        // Pad the gaps left by omitted empty cells.
        while (cells.length < at) cells.push("");

        const type = cell.getAttribute("t");
        const v = cell.getElementsByTagName("v")[0] ?? null;
        let value: string;
        if (type === "s") {
          // Shared-string reference.
          const i = Number(textOf(v));
          value = shared[i] ?? "";
        } else if (type === "inlineStr") {
          value = textOf(cell.getElementsByTagName("is")[0] ?? null);
        } else {
          value = textOf(v);
        }
        cells[at] = value;
      }
      rows.push(cells);
    }

    sheets.push({ name: names[index] ?? `Sheet${index + 1}`, rows });
  }

  return sheets;
}

const sheetNumber = (name: string) => Number(name.match(/sheet(\d+)\.xml$/)?.[1] ?? 0);

/**
 * Deck → text per slide.
 *
 * Explicitly a text outline, not a rendering: slide layout is absolute
 * positioning over inherited masters with its own theme and typography, and
 * approximating that produces something that looks like the deck but is not it.
 * The caller labels this as an outline so nobody mistakes it for the slides.
 */
export async function readSlides(bytes: Uint8Array): Promise<Slide[]> {
  const zip = await JSZip.loadAsync(bytes);

  const files = zip
    .file(/^ppt\/slides\/slide\d+\.xml$/)
    .sort((a, b) => slideNumber(a.name) - slideNumber(b.name));

  const slides: Slide[] = [];
  for (const file of files) {
    const doc = parseXml(await file.async("string"));
    const lines: string[] = [];

    // `a:p` is a paragraph; its `a:t` runs are the pieces of text, split
    // wherever formatting changes mid-sentence.
    for (const p of Array.from(doc.getElementsByTagName("a:p"))) {
      const text = Array.from(p.getElementsByTagName("a:t"))
        .map((t) => t.textContent ?? "")
        .join("")
        .trim();
      if (text) lines.push(text);
    }

    slides.push({ title: lines[0] ?? null, lines: lines.slice(1) });
  }

  return slides;
}

const slideNumber = (name: string) => Number(name.match(/slide(\d+)\.xml$/)?.[1] ?? 0);

/**
 * The tags a converted document is allowed to contain.
 *
 * Everything a `.docx` can express structurally, and nothing that executes or
 * loads. Anything outside this list is unwrapped — its text survives, its
 * behaviour does not.
 */
const ALLOWED_TAGS = new Set([
  "p", "br", "strong", "b", "em", "i", "u", "s", "sub", "sup", "span",
  "h1", "h2", "h3", "h4", "h5", "h6",
  "ul", "ol", "li", "blockquote", "pre", "code", "hr",
  "table", "thead", "tbody", "tr", "th", "td",
  "a", "img",
]);

/**
 * Tags removed along with everything inside them.
 *
 * Every other unknown tag is unwrapped, because its children are usually prose
 * worth keeping. These are the exceptions: their contents are code, and
 * unwrapping a `<script>` does not execute anything but does paste its source
 * into the document as visible text.
 */
const DROP_ENTIRELY = new Set(["script", "style", "noscript", "template", "head", "title"]);

/** Attributes kept per tag. Nothing else survives — no `style`, no `on*`. */
const ALLOWED_ATTRS: Record<string, Set<string>> = {
  a: new Set(["href", "title"]),
  img: new Set(["src", "alt", "width", "height"]),
  th: new Set(["colspan", "rowspan"]),
  td: new Set(["colspan", "rowspan"]),
};

/**
 * Reduce converted HTML to a known-safe subset.
 *
 * Exported so it can be tested against hostile input directly — a `.docx` from
 * Word will never produce any, so exercising this through the converter would
 * leave the whole function unverified.
 *
 * The CSP already blocks inline scripts and event handlers, so this is the
 * second layer rather than the only one — but the document comes from a file of
 * unknown origin and is being injected into a webview that can reach Tauri IPC,
 * which is not a place to rely on a single control. Cheaper than trusting the
 * converter's output to stay conservative across its future versions.
 */
export function sanitize(html: string): string {
  const doc = parser.parseFromString(`<body>${html}</body>`, "text/html");

  const walk = (node: Element) => {
    for (const child of Array.from(node.children)) {
      walk(child);

      const tag = child.tagName.toLowerCase();
      if (DROP_ENTIRELY.has(tag)) {
        child.remove();
        continue;
      }
      if (!ALLOWED_TAGS.has(tag)) {
        // Unwrap rather than delete: the text of an unknown wrapper is still
        // the document's content, and dropping it would silently lose prose.
        child.replaceWith(...Array.from(child.childNodes));
        continue;
      }

      const allowed = ALLOWED_ATTRS[tag];
      for (const attr of Array.from(child.attributes)) {
        if (!allowed?.has(attr.name.toLowerCase())) {
          child.removeAttribute(attr.name);
          continue;
        }
        // A `javascript:` or `data:text/html` URL in an href is a navigation
        // that runs code; images may only come from the document's own embedded
        // data, which mammoth emits as a base64 image data URL.
        const value = attr.value.trim().toLowerCase();
        const isImageData = tag === "img" && value.startsWith("data:image/");
        if (!/^(https?:|mailto:|#)/.test(value) && !isImageData) {
          child.removeAttribute(attr.name);
        }
      }
    }
  };

  walk(doc.body);
  return doc.body.innerHTML;
}

/** Word → sanitized HTML, via mammoth. Loaded on demand; it is the largest of the three. */
export async function readDocument(bytes: Uint8Array): Promise<string> {
  const mammoth = await import("mammoth/mammoth.browser.js");
  // `slice()` copies out of the typed array's own buffer, so the caller's bytes
  // are not detached by the worker mammoth may use internally.
  const { value } = await mammoth.convertToHtml({
    arrayBuffer: bytes.slice().buffer as ArrayBuffer,
  });
  return sanitize(value);
}
