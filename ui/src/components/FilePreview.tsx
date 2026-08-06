import { useEffect, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { api, type PreviewContent, type TargetRef } from "../lib/api";
import type { Sheet, Slide } from "../lib/office";

/**
 * Preview a file that is not editable source.
 *
 * Chosen by extension rather than by sniffing content: the extension is what the
 * operating system, the editor and the user all already agree the file is, and a
 * `.md` full of code fences should still render as markdown.
 *
 * Everything renders from a `blob:` URL rather than a served path. rmux has no
 * HTTP server, and the file may live on a machine across the world — a URL
 * pointing at it would have nothing to resolve against.
 *
 * A blob rather than a `data:` URL specifically: a `data:` URL for a 20MB video
 * is a 27MB string living in a DOM attribute, it cannot be range-requested (so
 * seeking a video re-reads from the start), and both WKWebView and WebView2
 * refuse to hand `data:` documents to their PDF viewers. A blob is a real
 * same-origin resource and behaves like a file the webview fetched.
 */

type Kind =
  | "markdown"
  | "image"
  | "pdf"
  | "video"
  | "audio"
  /** OOXML — a zip of XML, readable here. */
  | "document"
  | "spreadsheet"
  | "slides"
  /** Pre-2007 binary Office. Not a zip; nothing in the webview can read it. */
  | "legacyOffice"
  | "none";

/** Extension → how to show it, and the MIME type a webview needs to render it. */
const BY_EXTENSION: Record<string, { kind: Kind; mime: string }> = {
  md: { kind: "markdown", mime: "text/markdown" },
  markdown: { kind: "markdown", mime: "text/markdown" },
  mdx: { kind: "markdown", mime: "text/markdown" },

  png: { kind: "image", mime: "image/png" },
  jpg: { kind: "image", mime: "image/jpeg" },
  jpeg: { kind: "image", mime: "image/jpeg" },
  gif: { kind: "image", mime: "image/gif" },
  webp: { kind: "image", mime: "image/webp" },
  bmp: { kind: "image", mime: "image/bmp" },
  ico: { kind: "image", mime: "image/x-icon" },
  avif: { kind: "image", mime: "image/avif" },
  svg: { kind: "image", mime: "image/svg+xml" },

  pdf: { kind: "pdf", mime: "application/pdf" },

  mp4: { kind: "video", mime: "video/mp4" },
  webm: { kind: "video", mime: "video/webm" },
  mov: { kind: "video", mime: "video/quicktime" },
  m4v: { kind: "video", mime: "video/mp4" },

  mp3: { kind: "audio", mime: "audio/mpeg" },
  wav: { kind: "audio", mime: "audio/wav" },
  ogg: { kind: "audio", mime: "audio/ogg" },
  m4a: { kind: "audio", mime: "audio/mp4" },
  flac: { kind: "audio", mime: "audio/flac" },

  docx: { kind: "document", mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document" },
  xlsx: { kind: "spreadsheet", mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" },
  pptx: { kind: "slides", mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation" },

  doc: { kind: "legacyOffice", mime: "application/msword" },
  xls: { kind: "legacyOffice", mime: "application/vnd.ms-excel" },
  ppt: { kind: "legacyOffice", mime: "application/vnd.ms-powerpoint" },
};

export function previewKind(path: string): Kind {
  const name = path.split("/").filter(Boolean).pop() ?? path;
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "none";
  return BY_EXTENSION[name.slice(dot + 1).toLowerCase()]?.kind ?? "none";
}

function mimeFor(path: string): string {
  const name = path.split("/").filter(Boolean).pop() ?? path;
  const dot = name.lastIndexOf(".");
  return BY_EXTENSION[name.slice(dot + 1).toLowerCase()]?.mime ?? "application/octet-stream";
}

const humanBytes = (bytes: number) => {
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
};

/** Markdown, rendered from text the editor already has. */
export function MarkdownPreview({ text }: { text: string }) {
  return (
    <div className="h-full overflow-auto px-6 py-4">
      <div className="markdown selectable data mx-auto max-w-[820px] text-[13px] leading-[1.7]">
        <Markdown remarkPlugins={[remarkGfm]}>{text}</Markdown>
      </div>
    </div>
  );
}

/**
 * base64 → bytes, without the intermediate string copy.
 *
 * `atob` already gives one byte per code unit; writing straight into a typed
 * array avoids building a second full-size JS string on top of the base64 one,
 * which for a large video is the difference between one copy and three.
 */
function decodeBase64(base64: string): Uint8Array<ArrayBuffer> {
  const binary = atob(base64);
  const bytes = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/** Everything that needs the raw bytes. */
export function BinaryPreview({ target, path }: { target: TargetRef; path: string }) {
  const [content, setContent] = useState<PreviewContent | null>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [bytes, setBytes] = useState<Uint8Array<ArrayBuffer> | null>(null);
  const [error, setError] = useState<string | null>(null);

  const kind = previewKind(path);
  // Office formats are parsed in JS, so they need the decoded bytes; media is
  // handed to the webview as a blob and never touched again. Keeping both for a
  // 20MB video would hold it in memory twice for no purpose.
  const needsBytes = kind === "document" || kind === "spreadsheet" || kind === "slides";

  useEffect(() => {
    let cancelled = false;
    let objectUrl: string | null = null;
    setContent(null);
    setUrl(null);
    setBytes(null);
    setError(null);

    api
      .fsPreview(target, path)
      .then((c) => {
        if (cancelled) return;
        setContent(c);
        if (c.kind !== "base64") return;

        const decoded = decodeBase64(c.base64);
        if (needsBytes) setBytes(decoded);
        objectUrl = URL.createObjectURL(new Blob([decoded], { type: mimeFor(path) }));
        setUrl(objectUrl);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });

    return () => {
      cancelled = true;
      // A blob URL pins its bytes in memory until revoked. Without this, opening
      // a handful of videos would hold every one of them for the session.
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [target, path, needsBytes]);

  if (error) {
    return (
      <Centered>
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      </Centered>
    );
  }

  if (content?.kind === "tooLarge") {
    return (
      <Centered>
        <p className="data text-center text-[11px]" style={{ color: "var(--text-soft)" }}>
          {humanBytes(content.bytes)} — too large to preview.
        </p>
      </Centered>
    );
  }

  if (!content || !url) {
    return (
      <Centered>
        <span className="micro">loading preview…</span>
      </Centered>
    );
  }

  if (kind === "image") {
    return (
      <div className="grid h-full place-items-center overflow-auto p-4">
        {/* Contained rather than stretched: a screenshot shown at the wrong
            aspect ratio is worse than one shown small. */}
        <img
          src={url}
          alt={path}
          style={{ maxWidth: "100%", maxHeight: "100%", objectFit: "contain" }}
        />
      </div>
    );
  }

  if (kind === "pdf") {
    // The webview's own PDF viewer — WKWebView and WebView2 both have one.
    return <iframe src={url} title={path} className="h-full w-full" style={{ border: 0 }} />;
  }

  if (kind === "video") {
    return (
      <div className="grid h-full place-items-center p-4">
        <video src={url} controls style={{ maxWidth: "100%", maxHeight: "100%" }} />
      </div>
    );
  }

  if (kind === "audio") {
    return (
      <Centered>
        <div className="flex flex-col items-center gap-3">
          <span className="micro">{humanBytes(content.bytes)}</span>
          <audio src={url} controls />
        </div>
      </Centered>
    );
  }

  if (bytes && needsBytes) {
    return <OfficePreview kind={kind} bytes={bytes} path={path} url={url} />;
  }

  // Pre-2007 binary Office. A `.doc` is not a zip and not XML — it is an
  // undocumented compound-file format from a different era, and there is no
  // reader for it here. Saying so is better than a spinner that never resolves.
  return (
    <Centered>
      <div className="flex flex-col items-center gap-3 text-center">
        <span className="kicker">{path.split(".").pop()?.toUpperCase()} — legacy format</span>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          {humanBytes(content.bytes)} — pre-2007 Office files cannot be read in-app.
          <br />
          Re-saving it as .{kind === "legacyOffice" ? "docx / .xlsx / .pptx" : "x"} makes it
          previewable.
        </p>
        <a className="btn" href={url} download={path.split("/").pop()}>
          Download
        </a>
      </div>
    </Centered>
  );
}

/**
 * Word, Excel and PowerPoint, parsed in the webview.
 *
 * Parsing runs on demand rather than at import: these readers are the heaviest
 * code in the bundle and most sessions never open an office file, so loading
 * them eagerly would cost every cold start for a feature used occasionally.
 */
function OfficePreview({
  kind,
  bytes,
  path,
  url,
}: {
  kind: Kind;
  bytes: Uint8Array<ArrayBuffer>;
  path: string;
  url: string;
}) {
  const [result, setResult] = useState<
    { html: string } | { sheets: Sheet[] } | { slides: Slide[] } | null
  >(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setResult(null);
    setError(null);

    const load = async () => {
      // Imported here, not at module scope: this pulls in a zip reader and (for
      // Word) a half-megabyte converter, and most sessions never open an office
      // file. A static import would put that cost on every cold start.
      const office = await import("../lib/office");
      if (kind === "document") return { html: await office.readDocument(bytes) };
      if (kind === "spreadsheet") return { sheets: await office.readSpreadsheet(bytes) };
      return { slides: await office.readSlides(bytes) };
    };

    load()
      .then((r) => {
        if (!cancelled) setResult(r);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });

    return () => {
      cancelled = true;
    };
  }, [kind, bytes]);

  if (error) {
    // A file this reader cannot handle is still a file the user can open
    // elsewhere, so the failure keeps the way out rather than dead-ending.
    return (
      <Centered>
        <div className="flex flex-col items-center gap-3 text-center">
          <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
          <a className="btn" href={url} download={path.split("/").pop()}>
            Download
          </a>
        </div>
      </Centered>
    );
  }

  if (!result) {
    return (
      <Centered>
        <span className="micro">reading document…</span>
      </Centered>
    );
  }

  if ("html" in result) {
    return (
      <div className="h-full overflow-auto px-6 py-4">
        {/* mammoth emits a fixed, small set of semantic tags — no scripts, no
            styles, no attributes — so the markdown typography applies cleanly. */}
        <div
          className="markdown selectable data mx-auto max-w-[820px] text-[13px] leading-[1.7]"
          dangerouslySetInnerHTML={{ __html: result.html }}
        />
      </div>
    );
  }

  if ("sheets" in result) return <SheetsPreview sheets={result.sheets} />;
  return <SlidesPreview slides={result.slides} />;
}

/** A workbook: one tab per sheet, the active one as a plain grid. */
function SheetsPreview({ sheets }: { sheets: Sheet[] }) {
  const [active, setActive] = useState(0);
  const sheet = sheets[active];

  if (!sheets.length) {
    return (
      <Centered>
        <span className="micro">no sheets</span>
      </Centered>
    );
  }

  return (
    <div className="flex h-full flex-col">
      {sheets.length > 1 && (
        <div
          className="flex shrink-0 overflow-x-auto border-b px-2 py-1"
          style={{ borderColor: "var(--border)" }}
        >
          {/* A sheet picker is a segmented control: one frame, one chosen. Bare
              captions with the active one shaded read as a caption row. */}
          <div className="seg shrink-0">
          {sheets.map((s, i) => (
            <button
              key={`${s.name}-${i}`}
              type="button"
              className="shrink-0"
              aria-pressed={i === active}
              onClick={() => setActive(i)}
            >
              {s.name}
            </button>
          ))}
          </div>
        </div>
      )}

      {/* The grid scrolls inside itself; a wide spreadsheet must never make the
          whole panel scroll sideways. */}
      <div className="min-h-0 flex-1 overflow-auto">
        <table className="data border-collapse text-[11px]">
          <tbody>
            {sheet?.rows.map((row, r) => (
              <tr key={r}>
                {/* The row number, so a cell reference in a formula elsewhere
                    can actually be located. */}
                <td
                  className="sticky left-0 px-2 py-[2px] text-right"
                  style={{
                    color: "var(--text-faint)",
                    background: "var(--app-panel)",
                    borderRight: "1px solid var(--border)",
                  }}
                >
                  {r + 1}
                </td>
                {row.map((cell, c) => (
                  <td
                    key={c}
                    className="whitespace-pre px-2 py-[2px]"
                    style={{ borderRight: "1px solid var(--border)", borderBottom: "1px solid var(--border)" }}
                  >
                    {cell}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
        {!sheet?.rows.length && (
          <div className="p-6">
            <span className="micro">this sheet is empty</span>
          </div>
        )}
      </div>
    </div>
  );
}

/** A deck, as an outline. Labelled as one — see `readSlides`. */
function SlidesPreview({ slides }: { slides: Slide[] }) {
  if (!slides.length) {
    return (
      <Centered>
        <span className="micro">no slides</span>
      </Centered>
    );
  }

  return (
    <div className="h-full overflow-auto px-6 py-4">
      <div className="mx-auto max-w-[820px]">
        <p className="micro mb-4">
          text outline — {slides.length} slide{slides.length === 1 ? "" : "s"}; layout and
          graphics are not shown
        </p>
        {slides.map((slide, i) => (
          <div
            key={i}
            className="mb-3 border p-4"
            style={{ borderColor: "var(--border)", background: "var(--hover)" }}
          >
            <div className="mb-2 flex items-baseline gap-3">
              <span className="micro">{String(i + 1).padStart(2, "0")}</span>
              <span className="display text-[13px]">{slide.title ?? "untitled"}</span>
            </div>
            {slide.lines.map((line, j) => (
              <p key={j} className="data text-[12px] leading-[1.6]" style={{ color: "var(--text-soft)" }}>
                {line}
              </p>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

function Centered({ children }: { children: React.ReactNode }) {
  return <div className="grid h-full place-items-center p-6">{children}</div>;
}
