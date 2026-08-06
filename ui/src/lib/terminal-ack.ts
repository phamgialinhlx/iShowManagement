/**
 * Batched write-acknowledgements, the view's half of terminal flow control.
 *
 * The Rust forwarding task stops streaming while too many bytes are
 * unacknowledged (`Flow` in `src-tauri/src/terminal.rs`); the queue behind it
 * then fills and the PTY reader parks, so a firehose blocks at its source
 * instead of piling up in this webview faster than xterm can parse. The ack is
 * sent from `xterm.write`'s completion callback — after the bytes are truly
 * consumed, not merely received.
 *
 * Batched because each ack is an `invoke`: interactive echo acks at most once
 * per 250 ms, a firehose once per 64 KiB. Both are far below the 384 KiB
 * high-water mark, so a healthy view never stalls the stream.
 *
 * `send` returns whether it could deliver — `false` (no terminal id yet, while
 * the open/attach call is still in flight) keeps the count pending for the
 * next flush rather than losing credit.
 */
export function ackBatcher(send: (bytes: number) => boolean) {
  const FLUSH_BYTES = 64 * 1024;
  const FLUSH_MS = 250;

  let pending = 0;
  let timer: number | null = null;
  let disposed = false;

  const flush = () => {
    timer = null;
    if (disposed || pending === 0) return;
    if (send(pending)) {
      pending = 0;
    } else {
      // Not deliverable yet — try again shortly.
      timer = window.setTimeout(flush, FLUSH_MS);
    }
  };

  return {
    add(bytes: number) {
      if (disposed) return;
      pending += bytes;
      if (pending >= FLUSH_BYTES) flush();
      else if (timer === null) timer = window.setTimeout(flush, FLUSH_MS);
    },
    dispose() {
      disposed = true;
      if (timer !== null) window.clearTimeout(timer);
      timer = null;
    },
  };
}
