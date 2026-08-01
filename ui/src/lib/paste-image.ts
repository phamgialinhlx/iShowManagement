import { api } from "./api";
import type { TargetRef } from "./api";

/**
 * Getting a pasted or dropped image to a Claude that may not be on this machine.
 *
 * Claude Code reads images off the clipboard itself, which works when it is
 * running here and cannot work when it is on a server — that machine has no
 * clipboard and no route to yours. So the bytes travel: the image is written to
 * a file on the target and its **path** is typed into the prompt, which Claude
 * reads the same way it reads any file you mention.
 *
 * The path is typed, not sent. Same rule as a browser report: rmux fills the
 * composer and the operator presses Enter.
 */

/** What a paste or drop is carrying, if anything worth uploading. */
export function imagesFrom(data: DataTransfer | null): File[] {
  if (!data) return [];

  const files: File[] = [];

  // `items` covers a screenshot pasted from the clipboard, where there is no
  // file on disk at all; `files` covers a drag from Finder. They overlap, so
  // the two are merged by identity rather than concatenated — a drag reports
  // the same image through both and would otherwise upload twice.
  for (const item of Array.from(data.items ?? [])) {
    if (item.kind === "file" && item.type.startsWith("image/")) {
      const file = item.getAsFile();
      if (file) files.push(file);
    }
  }
  for (const file of Array.from(data.files ?? [])) {
    if (file.type.startsWith("image/") && !files.some((f) => same(f, file))) {
      files.push(file);
    }
  }

  return files;
}

const same = (a: File, b: File) =>
  a.name === b.name && a.size === b.size && a.lastModified === b.lastModified;

/**
 * Bytes to base64, without blowing the stack.
 *
 * `String.fromCharCode(...bytes)` is the idiomatic one-liner and it throws on
 * anything large — the argument list becomes one argument per byte, and a
 * megabyte screenshot is a megabyte of arguments. Chunked instead.
 */
function toBase64(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  const CHUNK = 0x8000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}

export type Uploaded = { path: string; bytes: number };

/** Put one image on the target. Returns the path to mention in the prompt. */
export async function uploadImage(target: TargetRef, file: File): Promise<Uploaded> {
  return api.claudePasteImage(target, toBase64(await file.arrayBuffer()), file.type);
}

/**
 * What to type into Claude once the images are there.
 *
 * Bare paths, space-separated, with a trailing space so the operator can keep
 * typing their question straight after. Claude picks a path out of a prompt on
 * its own; wrapping it in prose would only add words it has to ignore.
 *
 * Quoted only when it has to be. A path with a space in it is otherwise two
 * paths, and `~/.rmux/pastes` avoids that by construction — but the operator
 * can drag a file in from anywhere.
 */
export function promptFor(paths: string[]): string {
  return `${paths.map((p) => (/[\s'"]/.test(p) ? JSON.stringify(p) : p)).join(" ")} `;
}
