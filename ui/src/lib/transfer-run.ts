import { raiseAlert } from "./alerts";
import { api, type DownloadProgress, type Downloaded, type TargetRef } from "./api";
import { Channel } from "./bridge";
import { targetLabel } from "./connect-form";
import {
  NOTIFY_AFTER_MS,
  RUN_ID,
  cancel as cancelEntry,
  createEntry,
  fail,
  finish,
  progress,
  retry,
  start,
  useTransfers,
  type Direction,
  type TransferEntry,
} from "./transfers";

/**
 * The code that moves bytes and keeps the transfers list honest.
 *
 * Every download and upload goes through here — the context menu, a drop onto
 * the tree, and anything else — so each one registers, reports progress, can be
 * cancelled and ends in a state that says what really happened.
 */

export const newTransferId = () => `t${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Rust's word for a cancel, so a cancelled row never reads as a failure. */
const CANCELLED = "cancelled";

export function register(
  direction: Direction,
  target: TargetRef,
  remotePath: string,
  name: string,
  total: number | null = null,
): string {
  const id = newTransferId();
  useTransfers.getState().add(
    createEntry({ id, direction, server: targetLabel(target), target, remotePath, name, owner: RUN_ID, total }, Date.now()),
  );
  return id;
}

/** Long transfers that end while nobody is looking get a card — the existing ones. */
function notifyIfUnseen(id: string): void {
  const s = useTransfers.getState();
  const e = s.get(id);
  if (!e || e.owner !== RUN_ID || s.panelOpen) return;
  if ((e.endedAt ?? Date.now()) - e.startedAt < NOTIFY_AFTER_MS) return;
  if (e.state !== "done" && e.state !== "failed") return;
  const verb = e.direction === "down" ? "DOWNLOADED" : "UPLOADED";
  raiseAlert({
    sessionId: `transfer:${id}`,
    title: e.name,
    body:
      e.state === "done"
        ? e.direction === "down"
          ? `to ${e.localPath ?? "your downloads"}`
          : `to ${e.server} · ${e.remotePath}`
        : (e.error ?? "the transfer failed"),
    asking: false,
    label: e.state === "done" ? verb : "TRANSFER FAILED",
    onOpen: () => useTransfers.getState().setPanelOpen(true),
  });
}

/** The end state of a transfer whose call threw. */
function settleError(id: string, error: unknown): void {
  const { update, get } = useTransfers.getState();
  const text = message(error);
  const asked = get(id)?.cancelRequested === true;
  update(id, (e) => (asked || text === CANCELLED ? cancelEntry(e, Date.now()) : fail(e, text, Date.now())));
}

/**
 * Copy a file off the target into this machine's downloads folder, as a
 * registered transfer. Resolves with where it landed; rejects as the call did.
 */
export async function startDownload(target: TargetRef, remotePath: string, retryId?: string): Promise<Downloaded> {
  const name = remotePath.split("/").filter(Boolean).pop() ?? remotePath;
  const store = useTransfers.getState();
  let id: string;
  if (retryId && store.get(retryId)) {
    id = retryId;
    store.update(id, (e) => retry(e, RUN_ID, Date.now()));
  } else {
    id = register("down", target, remotePath, name);
  }
  store.update(id, (e) => start(e, Date.now()));

  const channel = new Channel<DownloadProgress>();
  channel.onmessage = (p) => {
    if (p.id !== id) return;
    useTransfers.getState().update(id, (e) => progress(e, p.done, p.total, Date.now()), false);
  };

  try {
    const result = await api.fsDownload(target, remotePath, id, channel);
    useTransfers.getState().update(id, (e) => finish(e, Date.now(), { bytes: result.bytes, localPath: result.path }));
    notifyIfUnseen(id);
    return result;
  } catch (e) {
    settleError(id, e);
    notifyIfUnseen(id);
    throw e;
  }
}

/** Download it again from its remote path, in the same row. Downloads only. */
export function retryTransfer(e: TransferEntry): void {
  if (e.direction !== "down") return;
  void startDownload(e.target, e.remotePath, e.id).catch(() => {
    // The row carries the error; nothing else to say.
  });
}

/**
 * Stop a queued or running transfer. Works from any window: the Rust side keys
 * live transfers by id, and the flag tells the owning window's loop to stop.
 */
export async function cancelTransfer(e: TransferEntry): Promise<void> {
  const { update } = useTransfers.getState();
  update(e.id, (x) => ({ ...x, cancelRequested: true }));
  if (e.state === "queued" && e.owner === RUN_ID) {
    update(e.id, (x) => cancelEntry(x, Date.now()));
    return;
  }
  try {
    if (e.direction === "down") await api.fsDownloadCancel(e.id);
    else if (e.uploadId) await api.fsUploadAbort(e.uploadId);
  } catch {
    // Already finished, or the backend is gone; the owner settles the row.
  }
}

export async function revealTransfer(e: TransferEntry): Promise<void> {
  if (e.localPath) await api.fsRevealDownload(e.localPath);
}

/* ---------------------------------------------------------------- uploads */

/** Thrown inside an upload loop when a cancel was asked for. */
export class UploadCancelled extends Error {
  constructor() {
    super(CANCELLED);
  }
}

export function cancelRequested(id: string): boolean {
  return useTransfers.getState().get(id)?.cancelRequested === true;
}

export { settleError as settleUploadError, notifyIfUnseen };
