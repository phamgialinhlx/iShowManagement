import { listen } from "./bridge";
import { api, type TargetRef } from "./api";
import { finish, progress, start, useTransfers } from "./transfers";
import { UploadCancelled, cancelRequested, notifyIfUnseen, register, settleUploadError } from "./transfer-run";

/**
 * Putting local files onto whichever machine the tree is showing.
 *
 * ## Why this is a shared module and not a handler
 *
 * There are two ways in — the right-click menu and a drop onto a folder — and
 * they must behave identically. Two copies of "encode, join, upload, count the
 * failures" is two chances for the drop to report something the menu does not.
 *
 * ## Files are uploaded one at a time, on purpose
 *
 * A remote upload is a shell over SSH; ten in parallel is ten connections'
 * worth of contention on the same ControlMaster socket, and the progress count
 * becomes meaningless because everything finishes at once at the end. Serial is
 * both faster in practice and the only way to say "3 of 8" honestly.
 *
 * ## One file failing must not cancel the rest
 *
 * Dropping a folder's worth of files where one name already exists should
 * upload the other seven and *name* the one it refused. Aborting the batch on
 * the first collision loses work the operator had every reason to expect.
 */

/**
 * The slice read and sent per IPC call.
 *
 * Files used to cross as one base64 string, which is why they were capped at
 * 64 MB: the string is built, copied and parsed whole on both sides. A chunk
 * bounds that to a few megabytes whatever the file's size, and Rust appends
 * each to a staging file it then streams. Big enough that the per-call overhead
 * vanishes, small enough that no single call is a noticeable stall.
 */
const CHUNK_BYTES = 4 * 1024 * 1024;

export type UploadFailure = { name: string; message: string };

export type UploadOutcome = {
  uploaded: string[];
  failed: UploadFailure[];
};

/**
 * Read a `Blob` as base64.
 *
 * `FileReader` rather than `btoa(String.fromCharCode(...bytes))`: spreading a
 * multi-megabyte array into a call blows the argument limit and throws, which
 * would make this work in testing and fail on a real screenshot.
 */
function toBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("could not read the file"));
    reader.onload = () => {
      const result = String(reader.result);
      // A data URL: `data:<mime>;base64,<payload>`. Only the payload is wanted.
      const comma = result.indexOf(",");
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.readAsDataURL(blob);
  });
}

const percent = (part: number, whole: number) =>
  whole > 0 ? Math.min(100, Math.floor((part / whole) * 100)) : 100;

/**
 * One file: stage it in chunks, then have Rust stream it to the target.
 *
 * Two phases, reported separately because they run at very different speeds:
 * reading is local disk and takes seconds, sending is the network and takes the
 * rest. A single bar that hit 100% at the end of reading and then sat there for
 * minutes would be a progress report that lies.
 *
 * Every step is mirrored into the transfers list (`transfers.ts`), counting
 * bytes as each chunk is *acknowledged* — not as it is read — so the bar never
 * runs ahead of what Rust actually holds.
 */
async function uploadOne(
  target: TargetRef,
  path: string,
  file: File,
  transfer: string,
  onPhase: (phase: string) => void,
): Promise<void> {
  const { update } = useTransfers.getState();
  if (cancelRequested(transfer)) throw new UploadCancelled();
  update(transfer, (e) => start(e, Date.now()));

  const id = await api.fsUploadBegin();
  // Persisted with the row, so a cancel from any window can reach it.
  update(transfer, (e) => ({ ...e, uploadId: id }));
  let finished = false;
  try {
    for (let offset = 0; offset < file.size; offset += CHUNK_BYTES) {
      if (cancelRequested(transfer)) throw new UploadCancelled();
      onPhase(`reading ${percent(offset, file.size)}%`);
      const chunk = file.slice(offset, offset + CHUNK_BYTES);
      await api.fsUploadChunk(id, await toBase64(chunk));
      const acked = Math.min(file.size, offset + chunk.size);
      update(transfer, (e) => progress(e, acked, file.size, Date.now(), "reading"), false);
    }
    if (cancelRequested(transfer)) throw new UploadCancelled();

    onPhase("sending 0%");
    update(transfer, (e) => progress(e, 0, file.size, Date.now(), "sending"));
    const unlisten = await listen<{ id: string; sent: number; size: number }>(
      "fs-upload-progress",
      ({ payload }) => {
        if (payload.id !== id) return;
        onPhase(`sending ${percent(payload.sent, payload.size)}%`);
        useTransfers.getState().update(transfer, (e) => progress(e, payload.sent, payload.size, Date.now(), "sending"), false);
      },
    );
    try {
      // `finish` discards the staging copy itself, success or not.
      finished = true;
      await api.fsUploadFinish(target, path, id, file.size);
    } finally {
      unlisten();
    }
  } finally {
    // A failure while staging leaves a partial copy of a possibly private file.
    if (!finished) void api.fsUploadAbort(id).catch(() => {});
  }
}

export async function uploadFiles(
  target: TargetRef,
  dir: string,
  files: File[],
  onProgress?: (done: number, total: number, name: string) => void,
): Promise<UploadOutcome> {
  const uploaded: string[] = [];
  const failed: UploadFailure[] = [];

  // Every file is registered up front, queued, so a batch shows its whole size
  // in the transfers list at once — and a queued one can be cancelled before
  // it starts.
  const rows = await Promise.all(
    files.map(async (file) => {
      const path = await api.fsJoin(dir, file.name).catch(() => `${dir}/${file.name}`);
      return { file, path, id: register("up", target, path, file.name, file.size) };
    }),
  );

  for (const [index, { file, path, id }] of rows.entries()) {
    onProgress?.(index, files.length, file.name);

    try {
      await uploadOne(target, path, file, id, (phase) =>
        onProgress?.(index, files.length, `${file.name} · ${phase}`),
      );
      useTransfers.getState().update(id, (e) => finish(e, Date.now(), { bytes: file.size }));
      notifyIfUnseen(id);
      uploaded.push(file.name);
    } catch (e) {
      settleUploadError(id, e);
      notifyIfUnseen(id);
      failed.push({ name: file.name, message: e instanceof Error ? e.message : String(e) });
    }
  }

  onProgress?.(files.length, files.length, "");
  return { uploaded, failed };
}

/**
 * What a drop actually contains.
 *
 * A drag from a browser or another app carries items that are not files at all —
 * a URL, some HTML, an image already on the page. `DataTransfer.files` is empty
 * for those, so a drop that looks identical to the operator does nothing and
 * says nothing. Reading the items lets the caller explain which it was.
 */
export function filesFrom(transfer: DataTransfer | null): File[] {
  if (!transfer) return [];

  // `items` is the richer view, but Safari populates `files` more reliably for
  // a plain Finder drag, so prefer whichever produced something.
  const fromItems = Array.from(transfer.items ?? [])
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null);

  return fromItems.length ? fromItems : Array.from(transfer.files ?? []);
}

/** One line stating exactly what happened, for the caller to show in place. */
export function describe(outcome: UploadOutcome): string {
  const { uploaded, failed } = outcome;

  if (!failed.length) {
    return uploaded.length === 1 ? `Uploaded ${uploaded[0]}` : `Uploaded ${uploaded.length} files`;
  }

  // One failure is named; several are counted, with the first named — a list of
  // eight messages is not something anyone reads off a context menu.
  const first = failed[0];
  const detail = first ? `${first.name}: ${first.message}` : "unknown error";
  const summary = failed.length === 1 ? detail : `${failed.length} failed — ${detail}`;

  return uploaded.length ? `Uploaded ${uploaded.length}, ${summary}` : summary;
}
